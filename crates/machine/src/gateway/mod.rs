//! Machine gateway: the typed envelope that reaches a worker, and the rules that gate it
//! (EXEC-002, DOMAIN.md §7.3, §8.1–§8.3).
//!
//! Every action that reaches a worker travels as one [`ActionEnvelope`]: the effect's dispatch token,
//! the lease and generation the caller holds, the tool and its arguments digest, a deadline, and a
//! kind drawn from a closed vocabulary. The gateway is where the envelope is *issued* (the runtime's
//! dispatch becomes a typed message) and where it is *validated* — by the worker, before it runs
//! anything.
//!
//! Two properties are the point:
//!
//! * **an envelope carries its own fence.** `{dispatch_token, lease_id, generation}` is what DOMAIN
//!   §7.3 says a dispatch is made of, and validation is against the lease the worker *currently* holds.
//!   A token is not a bearer credential: presenting a live one at a generation the worker no longer
//!   holds is refused.
//! * **nothing is dialled that must not be.** A `customer_private_worker` is outbound-only — it dials
//!   in, and the server never opens a connection to it — so [`MachineGateway::dial`] answers what a
//!   caller must do rather than letting a transport guess.
//!
//! Validation here is pure: no clock is read, no database is touched. The caller supplies the lease it
//! holds and the instant, so the same rules serve the worker (which has no control-plane database) and
//! a test.

use quansio_core::Digest;

use crate::control::{Substrate, TargetStatus};

/// What an envelope asks a worker to do (a closed vocabulary, because a worker executes only what it
/// can name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeKind {
    /// Run a tool.
    Dispatch,
    /// Cancel a dispatch that is still running.
    Cancel,
    /// Take a checkpoint of the target.
    Checkpoint,
    /// Upload an evidence object the worker captured.
    EvidenceUpload,
}

impl EnvelopeKind {
    /// Canonical wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dispatch => "dispatch",
            Self::Cancel => "cancel",
            Self::Checkpoint => "checkpoint",
            Self::EvidenceUpload => "evidence_upload",
        }
    }

    /// Parse the canonical wire value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "dispatch" => Some(Self::Dispatch),
            "cancel" => Some(Self::Cancel),
            "checkpoint" => Some(Self::Checkpoint),
            "evidence_upload" => Some(Self::EvidenceUpload),
            _ => None,
        }
    }
}

/// One action handed to a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionEnvelope {
    /// What the envelope asks for.
    pub kind: EnvelopeKind,
    /// Effect this action settles (`eff_…`), echoed back so the runtime can settle it.
    pub effect_id: String,
    /// Fence token the worker must present with its result (RUN-011's reservation).
    pub dispatch_token: String,
    /// Target the action is for.
    pub target_id: String,
    /// Lease the caller held when it issued the action.
    pub lease_id: String,
    /// Generation the lease was granted at.
    pub generation: i64,
    /// Run the action belongs to.
    pub run_id: String,
    /// Step the action is recorded under.
    pub step_id: String,
    /// Registered tool name; empty for kinds that run no tool.
    pub tool: String,
    /// Declaration version the call was planned against.
    pub tool_version: u32,
    /// Canonical JSON arguments; empty for kinds that run no tool.
    pub args_json: String,
    /// Digest of `args_json`, so a rewritten argument set is refused.
    pub args_digest: String,
    /// Upper bound on the output returned.
    pub max_output_bytes: usize,
    /// Instant the action stops being acceptable (canonical ISO-8601 UTC).
    pub deadline_at: String,
}

/// Everything one dispatch carries, so issuing one is a single decision rather than a long argument
/// list.
#[derive(Debug, Clone)]
pub struct DispatchRequest<'a> {
    /// Effect the dispatch settles.
    pub effect_id: &'a str,
    /// Fence token the worker must present with its result.
    pub dispatch_token: &'a str,
    /// Target the action is for.
    pub target_id: &'a str,
    /// Lease the caller holds.
    pub lease_id: &'a str,
    /// Generation the lease was granted at.
    pub generation: i64,
    /// Run the action belongs to.
    pub run_id: &'a str,
    /// Step the action is recorded under.
    pub step_id: &'a str,
    /// Registered tool name.
    pub tool: &'a str,
    /// Declaration version the call was planned against.
    pub tool_version: u32,
    /// Canonical JSON arguments.
    pub args_json: &'a str,
    /// Upper bound on the output returned.
    pub max_output_bytes: usize,
    /// Instant the action stops being acceptable.
    pub deadline_at: &'a str,
}

impl ActionEnvelope {
    /// Build a dispatch, computing the argument digest from the canonical JSON it is given.
    ///
    /// # Errors
    /// Returns [`Rejection::MalformedEnvelope`] when a required identity or instant is blank.
    /// [`Rejection::DeadlineExpired`] is *not* raised here: issuing an envelope with a deadline in the
    /// past is the validator's business, because only the validator knows the current instant.
    pub fn dispatch(request: &DispatchRequest<'_>) -> Result<Self, Rejection> {
        let envelope = Self {
            kind: EnvelopeKind::Dispatch,
            effect_id: request.effect_id.to_string(),
            dispatch_token: request.dispatch_token.to_string(),
            target_id: request.target_id.to_string(),
            lease_id: request.lease_id.to_string(),
            generation: request.generation,
            run_id: request.run_id.to_string(),
            step_id: request.step_id.to_string(),
            tool: request.tool.to_string(),
            tool_version: request.tool_version,
            args_digest: Digest::of_canonical_json(request.args_json)
                .as_str()
                .to_string(),
            args_json: request.args_json.to_string(),
            max_output_bytes: request.max_output_bytes,
            deadline_at: request.deadline_at.to_string(),
        };
        envelope.check_shape()?;
        Ok(envelope)
    }

    /// Whether the envelope carries the identities and instants every kind needs.
    ///
    /// # Errors
    /// Returns [`Rejection::MalformedEnvelope`] naming the missing field.
    pub fn check_shape(&self) -> Result<(), Rejection> {
        for (field, value) in [
            ("effect_id", self.effect_id.as_str()),
            ("dispatch_token", self.dispatch_token.as_str()),
            ("target_id", self.target_id.as_str()),
            ("lease_id", self.lease_id.as_str()),
            ("run_id", self.run_id.as_str()),
            ("step_id", self.step_id.as_str()),
            ("deadline_at", self.deadline_at.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(Rejection::MalformedEnvelope {
                    field,
                    detail: "is required",
                });
            }
        }
        if self.generation < 1 {
            return Err(Rejection::MalformedEnvelope {
                field: "generation",
                detail: "must be at least 1",
            });
        }
        if self.kind == EnvelopeKind::Dispatch {
            if self.tool.trim().is_empty() {
                return Err(Rejection::MalformedEnvelope {
                    field: "tool",
                    detail: "a dispatch names the tool it runs",
                });
            }
            if self.args_digest.len() != 64 {
                return Err(Rejection::MalformedEnvelope {
                    field: "args_digest",
                    detail: "is not a sha256",
                });
            }
        }
        Ok(())
    }

    /// Whether the arguments still hash to what the envelope pins.
    #[must_use]
    pub fn args_match_digest(&self) -> bool {
        Digest::of_canonical_json(&self.args_json).as_str() == self.args_digest
    }
}

/// The lease a worker currently holds, as it knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseClaim {
    /// Lease id the worker is holding.
    pub lease_id: String,
    /// Target the lease is for.
    pub target_id: String,
    /// Generation the lease was granted at.
    pub generation: i64,
    /// Whether the worker still believes it holds the lease.
    pub held: bool,
    /// Instant the lease expires at (canonical ISO-8601 UTC).
    pub expires_at: String,
}

/// What a worker reports about itself on the outbound half of the control channel (EXEC-002, DOMAIN.md
/// §8.2).
///
/// The report carries what the worker *observes*, never what it decides: the controller stores the
/// observation (`MachineControl::observe`) and derives health from the pair
/// `(desired_state, observed_state)`, so a worker cannot declare itself healthy. The generation travels
/// with the report because a worker that was fenced must not overwrite the observation of the
/// generation that replaced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heartbeat {
    /// Target the worker is running as.
    pub target_id: String,
    /// Generation the worker believes it holds.
    pub generation: i64,
    /// The state the worker observes itself in (a [`TargetStatus`] column value).
    pub observed_state: String,
    /// Instant the observation was taken (canonical ISO-8601 UTC).
    pub at: String,
}

impl Heartbeat {
    /// Whether the report names itself, a state the domain defines, and an instant.
    ///
    /// # Errors
    /// Returns [`Rejection::MalformedEnvelope`] naming the field that is missing or the state that is
    /// not one the lifecycle defines.
    pub fn check_shape(&self) -> Result<TargetStatus, Rejection> {
        for (field, value) in [
            ("target_id", self.target_id.as_str()),
            ("observed_state", self.observed_state.as_str()),
            ("at", self.at.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(Rejection::MalformedEnvelope {
                    field,
                    detail: "is required",
                });
            }
        }
        if self.generation < 1 {
            return Err(Rejection::MalformedEnvelope {
                field: "generation",
                detail: "must be at least 1",
            });
        }
        TargetStatus::parse(&self.observed_state).ok_or(Rejection::MalformedEnvelope {
            field: "observed_state",
            detail: "is not a state the lifecycle defines",
        })
    }
}

/// Why an envelope was refused. Each variant names a rule, and every one of them is decided **before**
/// any tool runs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
    /// The envelope is not a well-formed message.
    #[error("malformed envelope: {field} {detail}")]
    MalformedEnvelope {
        /// The offending field.
        field: &'static str,
        /// What is wrong with it.
        detail: &'static str,
    },
    /// The kind is not one the protocol defines.
    #[error("unknown envelope kind: {0}")]
    UnknownKind(String),
    /// The envelope is for another target than the lease the worker holds.
    #[error("envelope is for target {envelope}, the lease is for {lease}")]
    TargetMismatch {
        /// Target the envelope names.
        envelope: String,
        /// Target the lease is for.
        lease: String,
    },
    /// The envelope names a lease the worker is not holding.
    #[error("envelope names lease {envelope}, the worker holds {held}")]
    LeaseNotHeld {
        /// Lease the envelope names.
        envelope: String,
        /// Lease the worker holds.
        held: String,
    },
    /// The worker was fenced: the generation moved.
    #[error("envelope generation {envelope} does not match the lease generation {held}")]
    StaleGeneration {
        /// Generation the envelope names.
        envelope: i64,
        /// Generation the worker holds.
        held: i64,
    },
    /// The worker's lease has lapsed.
    #[error("the lease expired at {expires_at}, the action arrived at {now}")]
    LeaseExpired {
        /// When the lease expired.
        expires_at: String,
        /// When the action arrived.
        now: String,
    },
    /// The action's own deadline has passed.
    #[error("the action deadline {deadline_at} passed at {now}")]
    DeadlineExpired {
        /// The action's deadline.
        deadline_at: String,
        /// When the action arrived.
        now: String,
    },
    /// The arguments do not hash to what the envelope pins.
    #[error("arguments do not match the digest the envelope carries")]
    ArgsDigestMismatch,
}

/// How a caller may reach a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dial {
    /// The caller opens the connection to the target.
    Outbound,
    /// The target dials in and the caller waits for it: never dial a private worker.
    AwaitInbound,
}

/// The gateway: it issues envelopes and answers how a target is reached.
pub struct MachineGateway;

impl MachineGateway {
    /// Whether a caller may dial a target on this substrate.
    ///
    /// A customer's private worker is outbound-only (DOMAIN.md §8.1): it runs in their VPC and opens the
    /// control channel itself, so a caller must wait for it rather than reaching into their network.
    #[must_use]
    pub const fn dial(substrate: Substrate) -> Dial {
        if substrate.is_outbound_only() {
            Dial::AwaitInbound
        } else {
            Dial::Outbound
        }
    }

    /// Validate an envelope against the lease the worker holds, before anything runs.
    ///
    /// The order is deliberate: the shape first (a message that is not one cannot be reasoned about),
    /// then the identity the envelope claims, then the generation and the two expiries. Each refusal
    /// says which rule fired, and a caller that cannot pass this gate must not execute the action.
    ///
    /// # Errors
    /// Returns the [`Rejection`] naming the rule that refused the envelope.
    pub fn validate(
        envelope: &ActionEnvelope,
        lease: &LeaseClaim,
        now: &str,
    ) -> Result<(), Rejection> {
        envelope.check_shape()?;
        if envelope.kind == EnvelopeKind::Dispatch && !envelope.args_match_digest() {
            return Err(Rejection::ArgsDigestMismatch);
        }
        if envelope.target_id != lease.target_id {
            return Err(Rejection::TargetMismatch {
                envelope: envelope.target_id.clone(),
                lease: lease.target_id.clone(),
            });
        }
        if envelope.lease_id != lease.lease_id || !lease.held {
            return Err(Rejection::LeaseNotHeld {
                envelope: envelope.lease_id.clone(),
                held: lease.lease_id.clone(),
            });
        }
        if envelope.generation != lease.generation {
            return Err(Rejection::StaleGeneration {
                envelope: envelope.generation,
                held: lease.generation,
            });
        }
        if lease.expires_at.as_str() <= now {
            return Err(Rejection::LeaseExpired {
                expires_at: lease.expires_at.clone(),
                now: now.to_string(),
            });
        }
        if envelope.deadline_at.as_str() <= now {
            return Err(Rejection::DeadlineExpired {
                deadline_at: envelope.deadline_at.clone(),
                now: now.to_string(),
            });
        }
        Ok(())
    }

    /// Accept or refuse a worker's self-report, returning the state to store.
    ///
    /// The report is checked the same way an envelope is, and for the same reason: the platform must
    /// not record a claim from a worker it fenced, or a state string its lifecycle does not define.
    ///
    /// # Errors
    /// Returns [`Rejection::MalformedEnvelope`] when the report is not well formed or names a state the
    /// lifecycle does not define, [`Rejection::TargetMismatch`] when it is about another target, and
    /// [`Rejection::StaleGeneration`] when the worker was fenced.
    pub fn accept_heartbeat(
        heartbeat: &Heartbeat,
        target_id: &str,
        held_generation: i64,
    ) -> Result<TargetStatus, Rejection> {
        let observed = heartbeat.check_shape()?;
        if heartbeat.target_id != target_id {
            return Err(Rejection::TargetMismatch {
                envelope: heartbeat.target_id.clone(),
                lease: target_id.to_string(),
            });
        }
        if heartbeat.generation != held_generation {
            return Err(Rejection::StaleGeneration {
                envelope: heartbeat.generation,
                held: held_generation,
            });
        }
        Ok(observed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> DispatchRequest<'static> {
        DispatchRequest {
            effect_id: "eff_1",
            dispatch_token: "dsp_1",
            target_id: "tgt_1",
            lease_id: "lse_1",
            generation: 1,
            run_id: "run_1",
            step_id: "stp_1",
            tool: "fs.read",
            tool_version: 1,
            args_json: "{\"value\":\"42\"}",
            max_output_bytes: 64,
            deadline_at: "2026-09-13T10:01:00Z",
        }
    }

    fn envelope() -> ActionEnvelope {
        ActionEnvelope::dispatch(&request()).expect("well-formed")
    }

    fn lease() -> LeaseClaim {
        LeaseClaim {
            lease_id: "lse_1".to_string(),
            target_id: "tgt_1".to_string(),
            generation: 1,
            held: true,
            expires_at: "2026-09-13T10:05:00Z".to_string(),
        }
    }

    #[test]
    fn a_live_envelope_validates() {
        assert_eq!(
            MachineGateway::validate(&envelope(), &lease(), "2026-09-13T10:00:00Z"),
            Ok(())
        );
    }

    #[test]
    fn issuing_computes_the_argument_digest() {
        let issued = envelope();
        assert!(issued.args_match_digest());
        let mut rewritten = issued.clone();
        rewritten.args_json = "{\"value\":\"43\"}".to_string();
        assert!(!rewritten.args_match_digest());
        assert_eq!(
            MachineGateway::validate(&rewritten, &lease(), "2026-09-13T10:00:00Z"),
            Err(Rejection::ArgsDigestMismatch)
        );
    }

    #[test]
    fn shape_is_checked_before_anything_else() {
        let mut nameless = envelope();
        nameless.effect_id = "  ".to_string();
        assert!(matches!(
            MachineGateway::validate(&nameless, &lease(), "2026-09-13T10:00:00Z"),
            Err(Rejection::MalformedEnvelope {
                field: "effect_id",
                ..
            })
        ));
        let blank_token = DispatchRequest {
            dispatch_token: "",
            ..request()
        };
        assert!(ActionEnvelope::dispatch(&blank_token).is_err());
    }

    #[test]
    fn a_kind_needs_what_it_runs() {
        let mut no_tool = envelope();
        no_tool.tool = String::new();
        assert!(matches!(
            MachineGateway::validate(&no_tool, &lease(), "2026-09-13T10:00:00Z"),
            Err(Rejection::MalformedEnvelope { field: "tool", .. })
        ));
        // A checkpoint runs no tool, so it does not need one.
        let mut checkpoint = envelope();
        checkpoint.kind = EnvelopeKind::Checkpoint;
        checkpoint.tool = String::new();
        assert_eq!(
            MachineGateway::validate(&checkpoint, &lease(), "2026-09-13T10:00:00Z"),
            Ok(())
        );
    }

    #[test]
    fn the_dial_rule_is_the_substrate_s_decision() {
        assert_eq!(
            MachineGateway::dial(Substrate::CustomerPrivateWorker),
            Dial::AwaitInbound
        );
        assert_eq!(
            MachineGateway::dial(Substrate::CloudMicrovm),
            Dial::Outbound
        );
    }
}
