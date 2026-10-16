//! The wire form of the worker control channel (EXEC-002, DOMAIN.md §8.1–§8.3).
//!
//! One JSON document per line, in both directions, over a stream the **worker opened**: a
//! `customer_private_worker` dials in and the server never dials it ([`super::Dial::AwaitInbound`]).
//! The server speaks [`ToWorker`] (the lease it holds for the target, and dispatches fenced by it) and the
//! worker speaks [`FromWorker`] (who it is, what each dispatch settled as, and what it observes).
//!
//! These are plain data. Authority stays where it was: the worker re-validates every dispatch against the
//! lease it was told ([`super::MachineGateway::validate`]), and the server accepts a result only for a
//! dispatch it sent and still waits for.

use serde::{Deserialize, Serialize};

use super::{ActionEnvelope, EnvelopeKind, LeaseClaim};

/// The longest frame either side accepts, so a peer cannot make the other buffer without bound.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// The lease a worker is told it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseWire {
    /// Lease id (`lse_…`).
    pub lease_id: String,
    /// Target the lease is for.
    pub target_id: String,
    /// Generation it was granted at.
    pub generation: i64,
    /// Canonical ISO-8601 UTC expiry.
    pub expires_at: String,
}

impl From<&LeaseClaim> for LeaseWire {
    fn from(claim: &LeaseClaim) -> Self {
        Self {
            lease_id: claim.lease_id.clone(),
            target_id: claim.target_id.clone(),
            generation: claim.generation,
            expires_at: claim.expires_at.clone(),
        }
    }
}

impl From<LeaseWire> for LeaseClaim {
    fn from(wire: LeaseWire) -> Self {
        Self {
            lease_id: wire.lease_id,
            target_id: wire.target_id,
            generation: wire.generation,
            held: true,
            expires_at: wire.expires_at,
        }
    }
}

/// An [`ActionEnvelope`] on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeWire {
    /// `dispatch`, `cancel`, `checkpoint` or `evidence_upload`.
    pub kind: String,
    /// Effect the action settles.
    pub effect_id: String,
    /// Fence token.
    pub dispatch_token: String,
    /// Target.
    pub target_id: String,
    /// Lease the caller held.
    pub lease_id: String,
    /// Generation the lease was granted at.
    pub generation: i64,
    /// Run.
    pub run_id: String,
    /// Step.
    pub step_id: String,
    /// Tool name; empty for kinds that run none.
    pub tool: String,
    /// Declaration version.
    pub tool_version: u32,
    /// Canonical JSON arguments.
    pub args_json: String,
    /// Digest of `args_json`.
    pub args_digest: String,
    /// Output bound.
    pub max_output_bytes: usize,
    /// Deadline.
    pub deadline_at: String,
}

impl From<&ActionEnvelope> for EnvelopeWire {
    fn from(envelope: &ActionEnvelope) -> Self {
        Self {
            kind: envelope.kind.as_str().to_string(),
            effect_id: envelope.effect_id.clone(),
            dispatch_token: envelope.dispatch_token.clone(),
            target_id: envelope.target_id.clone(),
            lease_id: envelope.lease_id.clone(),
            generation: envelope.generation,
            run_id: envelope.run_id.clone(),
            step_id: envelope.step_id.clone(),
            tool: envelope.tool.clone(),
            tool_version: envelope.tool_version,
            args_json: envelope.args_json.clone(),
            args_digest: envelope.args_digest.clone(),
            max_output_bytes: envelope.max_output_bytes,
            deadline_at: envelope.deadline_at.clone(),
        }
    }
}

impl EnvelopeWire {
    /// The envelope this describes.
    ///
    /// # Errors
    /// Returns the unknown kind when it is not one the protocol defines: a worker executes only what it can name.
    pub fn into_envelope(self) -> Result<ActionEnvelope, String> {
        let kind = EnvelopeKind::parse(&self.kind).ok_or(self.kind)?;
        Ok(ActionEnvelope {
            kind,
            effect_id: self.effect_id,
            dispatch_token: self.dispatch_token,
            target_id: self.target_id,
            lease_id: self.lease_id,
            generation: self.generation,
            run_id: self.run_id,
            step_id: self.step_id,
            tool: self.tool,
            tool_version: self.tool_version,
            args_json: self.args_json,
            args_digest: self.args_digest,
            max_output_bytes: self.max_output_bytes,
            deadline_at: self.deadline_at,
        })
    }
}

/// Server to worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToWorker {
    /// The lease the worker holds now. Sent on connect and on every renewal; a newer one replaces the old.
    Lease(LeaseWire),
    /// One fenced action.
    Dispatch(Box<EnvelopeWire>),
}

/// How a dispatch ended on the worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OutcomeWire {
    /// The tool ran; `output` is its bounded JSON document.
    Completed {
        /// The document, as UTF-8 text.
        output: String,
        /// Whether the worker's sink discarded output.
        truncated: bool,
        /// Evidence reference, when evidence was captured.
        evidence_ref: Option<String>,
    },
    /// The tool ran and failed.
    Failed {
        /// Why.
        detail: String,
    },
    /// The action was cancelled before it ran.
    Cancelled,
    /// The worker refused to run it: the envelope did not pass the lease it holds. Nothing ran.
    Rejected {
        /// The rule that fired.
        reason: String,
    },
}

/// Worker to server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromWorker {
    /// The first frame: which target this worker serves.
    Hello {
        /// Tenant the worker serves.
        tenant_id: String,
        /// Target it claims to be.
        target_id: String,
    },
    /// What a dispatch settled as.
    Result {
        /// Dispatch the result answers.
        dispatch_token: String,
        /// Effect it settles.
        effect_id: String,
        /// Whether this answered a re-delivery rather than a run.
        replayed: bool,
        /// How it ended.
        outcome: OutcomeWire,
    },
    /// What the worker observes about itself (DOMAIN.md §8.2). The server renews the lease on a heartbeat it
    /// accepts, so a worker that goes quiet is fenced when its lease lapses, with no timer on the server.
    Heartbeat {
        /// Target the worker believes it is.
        target_id: String,
        /// Generation it believes it holds.
        generation: i64,
        /// The lifecycle state it observes itself in.
        observed_state: String,
        /// Canonical instant of the observation.
        at: String,
    },
}

/// Serialize one frame, newline-terminated.
///
/// # Errors
/// Returns the serializer's error.
pub fn encode<T: Serialize>(frame: &T) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(frame)?;
    line.push('\n');
    Ok(line)
}

/// Parse one frame from a line.
///
/// # Errors
/// Returns a message when the line is over [`MAX_FRAME_BYTES`] or is not a frame of the expected type.
pub fn decode<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, String> {
    if line.len() > MAX_FRAME_BYTES {
        return Err(format!(
            "frame of {} bytes exceeds {MAX_FRAME_BYTES}",
            line.len()
        ));
    }
    serde_json::from_str(line.trim_end()).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_envelope_survives_the_wire_and_an_unknown_kind_does_not() {
        let lease = LeaseClaim {
            lease_id: "lse_1".into(),
            target_id: "tgt_1".into(),
            generation: 2,
            held: true,
            expires_at: "2999-01-01T00:00:00Z".into(),
        };
        let wire = ToWorker::Lease((&lease).into());
        let line = encode(&wire).unwrap();
        assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
        assert_eq!(decode::<ToWorker>(&line).unwrap(), wire);

        let mut envelope = EnvelopeWire {
            kind: "dispatch".into(),
            effect_id: "eff_1".into(),
            dispatch_token: "dsp_1".into(),
            target_id: "tgt_1".into(),
            lease_id: "lse_1".into(),
            generation: 2,
            run_id: "run_1".into(),
            step_id: "stp_1".into(),
            tool: "fs.read".into(),
            tool_version: 1,
            args_json: "{}".into(),
            args_digest: "0".repeat(64),
            max_output_bytes: 10,
            deadline_at: "2999-01-01T00:00:00Z".into(),
        };
        assert_eq!(
            EnvelopeWire::from(&envelope.clone().into_envelope().unwrap()),
            envelope
        );
        envelope.kind = "format_disk".into();
        assert_eq!(envelope.into_envelope().unwrap_err(), "format_disk");
    }

    #[test]
    fn a_frame_over_the_bound_or_of_the_wrong_shape_is_refused() {
        assert!(decode::<FromWorker>(&"x".repeat(MAX_FRAME_BYTES + 1)).is_err());
        assert!(decode::<FromWorker>("{\"type\":\"lease\"}").is_err());
        assert!(decode::<ToWorker>("not json").is_err());
    }
}
