//! The worker host: qworkerd's side of the control channel (EXEC-002).
//!
//! qworkerd executes tools. It does not decide anything: policy, capability, approval and routing were
//! decided by the runtime before the action was dispatched, and what reaches this module is an
//! [`ActionEnvelope`] that the runtime issued and this host validated. Two rules follow, and both are
//! enforced here rather than trusted:
//!
//! * **validation happens before execution.** The envelope is checked against the lease the host holds
//!   (EXEC-001's `(lease_id, generation)` fence), so a stale, fenced or expired action fails with the
//!   executor untouched. A host that ran first and validated afterwards would have already caused the
//!   effect it was supposed to refuse.
//! * **a dispatch runs at most once.** A dispatch token identifies one effect, and a re-delivered
//!   envelope — a reconnect replay — returns the outcome that was recorded rather than running the tool
//!   again. When the lease has lapsed in between, the replay is *fenced* instead: either way the effect
//!   is not executed twice, which is the acceptance statement the runtime's recovery depends on.
//!
//! The host has no database and evaluates nothing: it holds an executor, an evidence sink and the
//! outcomes it recorded. `crates/qworkerd/tests/host.rs` scans its sources to keep it that way.

use std::collections::BTreeMap;

use quansio_machine::gateway::{
    ActionEnvelope, EnvelopeKind, Heartbeat, LeaseClaim, MachineGateway, Rejection,
};

/// What a tool produced besides its streamed output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    /// Evidence the host should upload for this action.
    pub evidence: Vec<u8>,
    /// Media type of the evidence.
    pub evidence_media_type: String,
}

impl ToolOutput {
    /// A run with no evidence to upload.
    #[must_use]
    pub fn new() -> Self {
        Self {
            evidence: Vec::new(),
            evidence_media_type: "application/octet-stream".to_string(),
        }
    }

    /// Attach an evidence payload.
    #[must_use]
    pub fn with_evidence(mut self, payload: impl Into<Vec<u8>>, media_type: &str) -> Self {
        self.evidence = payload.into();
        self.evidence_media_type = media_type.to_string();
        self
    }
}

impl Default for ToolOutput {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a running tool streams its output, bounded **as it arrives**.
///
/// The bound is the envelope's `max_output_bytes`, and it is enforced here rather than on the finished
/// result so a tool that produces unbounded output cannot make the worker hold it all first. What the
/// sink keeps is exactly what will be returned; what it discarded is counted, so truncation is
/// reported rather than silently applied.
#[derive(Debug)]
pub struct OutputSink {
    max_bytes: usize,
    kept: Vec<u8>,
    offered: usize,
}

impl OutputSink {
    /// A sink that keeps at most `max_bytes`; `0` means unbounded.
    #[must_use]
    pub fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            kept: Vec::new(),
            offered: 0,
        }
    }

    /// Offer a chunk of output, keeping only what still fits inside the bound.
    pub fn push(&mut self, chunk: &[u8]) {
        self.offered += chunk.len();
        if self.max_bytes == 0 {
            self.kept.extend_from_slice(chunk);
            return;
        }
        let room = self.max_bytes.saturating_sub(self.kept.len());
        if room > 0 {
            self.kept.extend_from_slice(&chunk[..chunk.len().min(room)]);
        }
    }

    /// Bytes the tool offered in total, whether kept or not.
    #[must_use]
    pub const fn offered(&self) -> usize {
        self.offered
    }

    /// Whether anything the tool offered was discarded.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.offered > self.kept.len()
    }

    /// The kept output.
    #[must_use]
    pub fn kept(&self) -> &[u8] {
        &self.kept
    }

    /// Take the kept output, reporting whether anything was discarded.
    #[must_use]
    pub fn finish(self) -> (Vec<u8>, bool) {
        let truncated = self.truncated();
        (self.kept, truncated)
    }
}

/// A tool that could not run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostFailure {
    /// The tool itself failed.
    #[error("tool {tool} failed: {detail}")]
    ToolFailed {
        /// Tool name.
        tool: String,
        /// What went wrong.
        detail: String,
    },
    /// The evidence could not be uploaded.
    #[error("evidence upload failed: {0}")]
    EvidenceFailed(String),
}

/// How an action ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostOutcome {
    /// The tool ran and its output is here.
    Completed {
        /// Effect that was settled.
        effect_id: String,
        /// Output, truncated to the envelope's bound if it exceeded it.
        output: Vec<u8>,
        /// Whether the output was truncated.
        truncated: bool,
        /// Evidence reference the sink returned, when evidence was captured.
        evidence_ref: Option<String>,
    },
    /// The tool ran and failed.
    Failed {
        /// Effect that failed.
        effect_id: String,
        /// Why.
        failure: HostFailure,
    },
    /// The action was cancelled before it ran.
    Cancelled {
        /// Effect that was cancelled.
        effect_id: String,
    },
}

impl HostOutcome {
    /// The effect this outcome settles.
    #[must_use]
    pub fn effect_id(&self) -> &str {
        match self {
            Self::Completed { effect_id, .. }
            | Self::Failed { effect_id, .. }
            | Self::Cancelled { effect_id } => effect_id,
        }
    }
}

/// One recorded result, kept so a replayed dispatch is answered rather than re-run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// The outcome the first delivery produced.
    pub outcome: HostOutcome,
    /// Whether this record answered a replay rather than an execution.
    pub replayed: bool,
}

/// The executor a host runs tools through.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Run one action, streaming its output into `output`.
    ///
    /// # Errors
    /// Returns [`HostFailure`] when the tool fails. Output already pushed stays in the sink; a failed
    /// action's output is not returned.
    async fn run(
        &self,
        envelope: &ActionEnvelope,
        output: &mut OutputSink,
    ) -> Result<ToolOutput, HostFailure>;
}

/// Where captured evidence goes.
#[async_trait::async_trait]
pub trait EvidenceSink: Send + Sync {
    /// Upload one evidence payload, returning its reference.
    ///
    /// # Errors
    /// Returns [`HostFailure::EvidenceFailed`] when the upload fails.
    async fn upload(
        &self,
        effect_id: &str,
        payload: &[u8],
        media_type: &str,
    ) -> Result<String, HostFailure>;
}

/// A host that runs nothing: the seam a host is installed into.
pub struct NoopExecutor;

#[async_trait::async_trait]
impl ToolExecutor for NoopExecutor {
    async fn run(
        &self,
        envelope: &ActionEnvelope,
        _output: &mut OutputSink,
    ) -> Result<ToolOutput, HostFailure> {
        Err(HostFailure::ToolFailed {
            tool: envelope.tool.clone(),
            detail: "no executor is installed".to_string(),
        })
    }
}

/// The worker host.
pub struct WorkerHost<E: ToolExecutor, S: EvidenceSink> {
    executor: E,
    evidence: S,
    recorded: BTreeMap<String, HostOutcome>,
    checkpoints: Vec<String>,
}

impl<E: ToolExecutor, S: EvidenceSink> WorkerHost<E, S> {
    /// Build a host over an executor and an evidence sink.
    pub fn new(executor: E, evidence: S) -> Self {
        Self {
            executor,
            evidence,
            recorded: BTreeMap::new(),
            checkpoints: Vec::new(),
        }
    }

    /// Receive an action, validate it against the lease this host holds, and run it at most once.
    ///
    /// A dispatch whose token was already recorded is answered from the record: the tool does not run
    /// again, and the caller learns the action was a replay. Validation happens first in every case, so
    /// a replay that arrives after the lease lapsed is fenced rather than served.
    ///
    /// # Errors
    /// Returns [`Rejection`] when the envelope may not be executed at all.
    pub async fn receive(
        &mut self,
        envelope: &ActionEnvelope,
        lease: &LeaseClaim,
        now: &str,
    ) -> Result<Recorded, Rejection> {
        MachineGateway::validate(envelope, lease, now)?;

        if let Some(outcome) = self.recorded.get(&envelope.dispatch_token) {
            return Ok(Recorded {
                outcome: outcome.clone(),
                replayed: true,
            });
        }

        let outcome = match envelope.kind {
            EnvelopeKind::Cancel => HostOutcome::Cancelled {
                effect_id: envelope.effect_id.clone(),
            },
            EnvelopeKind::Checkpoint => {
                self.checkpoints.push(envelope.target_id.clone());
                HostOutcome::Completed {
                    effect_id: envelope.effect_id.clone(),
                    output: Vec::new(),
                    truncated: false,
                    evidence_ref: None,
                }
            }
            EnvelopeKind::EvidenceUpload | EnvelopeKind::Dispatch => {
                let mut output = OutputSink::new(envelope.max_output_bytes);
                match self.executor.run(envelope, &mut output).await {
                    Ok(tool_output) => {
                        let (bounded, truncated) = output.finish();
                        let evidence_ref = if tool_output.evidence.is_empty() {
                            None
                        } else {
                            match self
                                .evidence
                                .upload(
                                    &envelope.effect_id,
                                    &tool_output.evidence,
                                    &tool_output.evidence_media_type,
                                )
                                .await
                            {
                                Ok(reference) => Some(reference),
                                Err(failure) => {
                                    let failed = HostOutcome::Failed {
                                        effect_id: envelope.effect_id.clone(),
                                        failure,
                                    };
                                    self.recorded
                                        .insert(envelope.dispatch_token.clone(), failed.clone());
                                    return Ok(Recorded {
                                        outcome: failed,
                                        replayed: false,
                                    });
                                }
                            }
                        };
                        HostOutcome::Completed {
                            effect_id: envelope.effect_id.clone(),
                            output: bounded,
                            truncated,
                            evidence_ref,
                        }
                    }
                    Err(failure) => HostOutcome::Failed {
                        effect_id: envelope.effect_id.clone(),
                        failure,
                    },
                }
            }
        };

        self.recorded
            .insert(envelope.dispatch_token.clone(), outcome.clone());
        Ok(Recorded {
            outcome,
            replayed: false,
        })
    }

    /// The outcomes this host has recorded.
    #[must_use]
    pub fn recorded(&self) -> &BTreeMap<String, HostOutcome> {
        &self.recorded
    }

    /// The report this worker puts on the outbound half of the channel, built from the lease it holds.
    ///
    /// The generation comes from the lease the host was fenced at, not from what it would like it to
    /// be, so a fenced worker's report is refused by the controller rather than believed.
    #[must_use]
    pub fn heartbeat(lease: &LeaseClaim, observed_state: &str, at: &str) -> Heartbeat {
        Heartbeat {
            target_id: lease.target_id.clone(),
            generation: lease.generation,
            observed_state: observed_state.to_string(),
            at: at.to_string(),
        }
    }

    /// Targets this host has checkpointed, in order.
    #[must_use]
    pub fn checkpoints(&self) -> &[String] {
        &self.checkpoints
    }
}
