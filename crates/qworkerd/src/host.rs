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
use std::sync::{Mutex, PoisonError};

use tokio::sync::{watch, Semaphore};

use quansio_machine::control::browser::DecisionReceipt;
use quansio_machine::gateway::wire::BrowserOrder;
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
    /// The action had started and was stopped by a cancel. Part of it may have happened, so its outcome is unknown.
    Interrupted {
        /// Effect that was interrupted.
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
            | Self::Cancelled { effect_id }
            | Self::Interrupted { effect_id } => effect_id,
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

    /// Apply an order about a browser session. An executor with no browser refuses.
    ///
    /// # Errors
    /// Returns why the order was not applied.
    async fn browser_order(&self, order: &BrowserOrder) -> Result<(), String> {
        let _ = order;
        Err("this worker has no browser".to_string())
    }

    /// Apply a user's decision on a certificate error or a permission. An executor with no browser refuses.
    ///
    /// # Errors
    /// Returns why the decision was not applied.
    async fn browser_decision(&self, receipt: &DecisionReceipt) -> Result<(), String> {
        let _ = receipt;
        Err("this worker has no browser".to_string())
    }

    /// The next frame of a session whose screencast is running. Never resolves for an executor with no browser, so a
    /// caller can wait on it beside other work.
    async fn next_frame(&self) -> Option<crate::browser::ScreencastFrame> {
        std::future::pending().await
    }

    /// The guard reports of the browser sessions this executor runs, as `(session_id, report)`; `only` limits them to
    /// one session. Empty for an executor with no browser.
    async fn browser_guards(&self, only: Option<&str>) -> Vec<(String, serde_json::Value)> {
        let _ = only;
        Vec::new()
    }
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

/// How many dispatches a host runs at once unless told otherwise.
pub const DEFAULT_CONCURRENCY: usize = 8;

/// Where one dispatch token stands on this host.
enum Slot {
    /// Received and not yet settled. `done` carries the outcome once there is one; `cancel` stops it.
    Running {
        done: watch::Receiver<Option<HostOutcome>>,
        cancel: watch::Sender<bool>,
    },
    /// Settled; a re-delivery is answered from here.
    Settled(HostOutcome),
}

/// What `receive` found for a token.
enum Entry {
    Settled(HostOutcome),
    Running(watch::Receiver<Option<HostOutcome>>),
    New(watch::Sender<Option<HostOutcome>>, watch::Receiver<bool>),
}

/// The worker host.
///
/// Dispatches run side by side, up to the host's concurrency; one past it waits for a turn. Whatever runs at once, a token
/// runs at most once: a re-delivery of a token that is still running waits for that run and is answered with its outcome
/// (`replayed`), and one that has settled is answered from the record.
pub struct WorkerHost<E: ToolExecutor, S: EvidenceSink> {
    executor: E,
    evidence: S,
    slots: Mutex<BTreeMap<String, Slot>>,
    checkpoints: Mutex<Vec<String>>,
    turns: Semaphore,
}

impl<E: ToolExecutor, S: EvidenceSink> WorkerHost<E, S> {
    /// The executor the host runs tools through.
    #[must_use]
    pub const fn executor(&self) -> &E {
        &self.executor
    }

    /// Build a host over an executor and an evidence sink that runs [`DEFAULT_CONCURRENCY`] dispatches at once.
    pub fn new(executor: E, evidence: S) -> Self {
        Self::with_concurrency(executor, evidence, DEFAULT_CONCURRENCY)
    }

    /// Build a host that runs at most `concurrency` dispatches at once (at least one).
    pub fn with_concurrency(executor: E, evidence: S, concurrency: usize) -> Self {
        Self {
            executor,
            evidence,
            slots: Mutex::new(BTreeMap::new()),
            checkpoints: Mutex::new(Vec::new()),
            turns: Semaphore::new(concurrency.max(1)),
        }
    }

    /// Receive an action, validate it against the lease this host holds, and run it at most once.
    ///
    /// A dispatch whose token was already recorded is answered from the record: the tool does not run
    /// again, and the caller learns the action was a replay. One whose token is still running waits for that run and is
    /// answered the same way. Validation happens first in every case, so a replay that arrives after the lease lapsed is
    /// fenced rather than served.
    ///
    /// # Errors
    /// Returns [`Rejection`] when the envelope may not be executed at all.
    pub async fn receive(
        &self,
        envelope: &ActionEnvelope,
        lease: &LeaseClaim,
        now: &str,
    ) -> Result<Recorded, Rejection> {
        MachineGateway::validate(envelope, lease, now)?;

        let token = envelope.dispatch_token.clone();
        // Decided under the lock, acted on after it is released: nothing is awaited while it is held.
        let entry = {
            let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            match slots.get(&token) {
                Some(Slot::Settled(outcome)) => Entry::Settled(outcome.clone()),
                Some(Slot::Running { done, .. }) => Entry::Running(done.clone()),
                None => {
                    let (finished, done) = watch::channel(None);
                    let (cancel, cancelled) = watch::channel(false);
                    slots.insert(token.clone(), Slot::Running { done, cancel });
                    Entry::New(finished, cancelled)
                }
            }
        };
        let (finished, mut cancelled) = match entry {
            Entry::Settled(outcome) => {
                return Ok(Recorded {
                    outcome,
                    replayed: true,
                })
            }
            Entry::Running(mut done) => {
                return Ok(Recorded {
                    outcome: wait_for(&mut done, &envelope.effect_id).await,
                    replayed: true,
                })
            }
            Entry::New(finished, cancelled) => (finished, cancelled),
        };

        let outcome = match envelope.kind {
            EnvelopeKind::Cancel => HostOutcome::Cancelled {
                effect_id: envelope.effect_id.clone(),
            },
            EnvelopeKind::Checkpoint => {
                self.checkpoints
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(envelope.target_id.clone());
                HostOutcome::Completed {
                    effect_id: envelope.effect_id.clone(),
                    output: Vec::new(),
                    truncated: false,
                    evidence_ref: None,
                }
            }
            EnvelopeKind::EvidenceUpload | EnvelopeKind::Dispatch => {
                // A turn first: a cancel while waiting for one means nothing ran.
                let turn = tokio::select! {
                    turn = self.turns.acquire() => turn.ok(),
                    () = raised(&mut cancelled) => None,
                };
                match turn {
                    None => HostOutcome::Cancelled {
                        effect_id: envelope.effect_id.clone(),
                    },
                    Some(_turn) => {
                        tokio::select! {
                            outcome = self.execute(envelope) => outcome,
                            // Dropping the run stops it: a command's process is killed with it.
                            () = raised(&mut cancelled) => HostOutcome::Interrupted {
                                effect_id: envelope.effect_id.clone(),
                            },
                        }
                    }
                }
            }
        };

        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(token, Slot::Settled(outcome.clone()));
        let _ = finished.send(Some(outcome.clone()));
        Ok(Recorded {
            outcome,
            replayed: false,
        })
    }

    /// Stop the dispatch `dispatch_token` if it has not settled. Returns whether there was one to stop.
    pub fn cancel(&self, dispatch_token: &str) -> bool {
        let slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        match slots.get(dispatch_token) {
            Some(Slot::Running { cancel, .. }) => {
                let _ = cancel.send(true);
                true
            }
            _ => false,
        }
    }

    /// The dispatches received and not yet settled.
    #[must_use]
    pub fn running(&self) -> Vec<String> {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, slot)| matches!(slot, Slot::Running { .. }))
            .map(|(token, _)| token.clone())
            .collect()
    }

    async fn execute(&self, envelope: &ActionEnvelope) -> HostOutcome {
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
                            return HostOutcome::Failed {
                                effect_id: envelope.effect_id.clone(),
                                failure,
                            }
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

    /// The outcomes this host has settled, by dispatch token.
    #[must_use]
    pub fn recorded(&self) -> BTreeMap<String, HostOutcome> {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter_map(|(token, slot)| match slot {
                Slot::Settled(outcome) => Some((token.clone(), outcome.clone())),
                Slot::Running { .. } => None,
            })
            .collect()
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
    pub fn checkpoints(&self) -> Vec<String> {
        self.checkpoints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Resolve once `flag` is raised; never if its sender is gone without raising it.
async fn raised(flag: &mut watch::Receiver<bool>) {
    if flag.wait_for(|raised| *raised).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// The outcome a running dispatch settles as.
async fn wait_for(done: &mut watch::Receiver<Option<HostOutcome>>, effect_id: &str) -> HostOutcome {
    match done.wait_for(Option::is_some).await {
        Ok(outcome) => outcome
            .clone()
            .unwrap_or_else(|| unreachable_outcome(effect_id)),
        // The run that held the token was dropped before it settled (its caller went away): nothing is known about it.
        Err(_) => unreachable_outcome(effect_id),
    }
}

fn unreachable_outcome(effect_id: &str) -> HostOutcome {
    HostOutcome::Interrupted {
        effect_id: effect_id.to_string(),
    }
}
