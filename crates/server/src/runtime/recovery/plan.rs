//! The pure recovery decisions: what to do with a run, and who is stale (RUN-009).
//!
//! Both functions take plain data, so the kill/restart matrix and the fencing rule are testable
//! without a database, and the durable behaviour is covered separately by the integration suite.

use super::RecoveryError;

/// The durable view of a run that recovery decides from.
///
/// Every field comes from ProtocolState, the Run/Step/Attempt tables or the Effect Ledger.
/// Nothing here can be populated from semantic memory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DurableState {
    /// The run's current status (DOMAIN.md §5.2 spelling).
    pub run_status: String,
    /// Whether a cancellation was requested before the crash.
    pub cancellation_requested: bool,
    /// Approvals the run is parked on.
    pub pending_approvals: Vec<String>,
    /// Questions the run is parked on.
    pub open_questions: Vec<String>,
    /// Child agents the run is parked on.
    pub child_agent_threads: Vec<String>,
    /// Whether the run is parked on a timer, event or takeover.
    pub parked_on_external: bool,
    /// `OUTCOME_UNKNOWN` effect the run reserved, if any.
    pub unsettled_effect_id: Option<String>,
    /// The tool call that reserved the unsettled effect, when the reservation recorded one.
    pub unsettled_tool_call_id: Option<String>,
}

impl DurableState {
    /// Whether the run can no longer change (DOMAIN.md §5.2's terminal states).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.run_status.as_str(),
            "SUCCEEDED" | "FAILED" | "CANCELLED" | "BLOCKED_UNRECOVERABLE"
        )
    }
}

/// What recovery should do with a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafeAction {
    /// The run is already terminal; recovery changes nothing.
    Terminal,
    /// Honour the cancellation that was requested before the crash.
    Cancel,
    /// Reconcile the uncertain effect before anything else happens.
    ReconcileEffect {
        /// EffectRecord awaiting reconciliation.
        effect_id: String,
        /// Tool call that reserved it.
        tool_call_id: String,
    },
    /// Stay parked: only the resolution matching `key` releases the run.
    Wait {
        /// `WAITING_*` state the run holds.
        state: String,
        /// Key a matching resolution must carry.
        key: String,
    },
    /// Resume the run at its next turn.
    Resume,
}

impl SafeAction {
    /// Whether the action changes durable state.
    #[must_use]
    pub const fn mutates(&self) -> bool {
        matches!(
            self,
            Self::Cancel | Self::ReconcileEffect { .. } | Self::Resume
        )
    }
}

/// Decide the safe action for a run after a restart.
///
/// The precedence is the runtime's (DOMAIN.md §5.2, §5.7): a cancellation wins, an uncertain
/// effect is reconciled before any further work, a parked run stays parked until its own
/// resolution arrives, and only then may work resume. Deriving it from durable state alone is
/// what makes a restart land on the same logical point the crash interrupted.
#[must_use]
pub fn plan_from(state: &DurableState) -> SafeAction {
    if state.is_terminal() {
        return SafeAction::Terminal;
    }
    if state.cancellation_requested {
        return SafeAction::Cancel;
    }
    if let Some(effect_id) = state.unsettled_effect_id.clone() {
        // The tool call is recorded when the reservation kept one; reconciliation is required by
        // the unsettled effect itself, so a missing tool-call id never lets the run resume.
        return SafeAction::ReconcileEffect {
            effect_id,
            tool_call_id: state.unsettled_tool_call_id.clone().unwrap_or_default(),
        };
    }
    let wait = match state.run_status.as_str() {
        "WAITING_APPROVAL" => state
            .pending_approvals
            .first()
            .cloned()
            .map(|key| ("WAITING_APPROVAL".to_string(), key)),
        "WAITING_QUESTION" => state
            .open_questions
            .first()
            .cloned()
            .map(|key| ("WAITING_QUESTION".to_string(), key)),
        "WAITING_CHILD" => state
            .child_agent_threads
            .first()
            .cloned()
            .map(|key| ("WAITING_CHILD".to_string(), key)),
        "WAITING_TIMER" => Some(("WAITING_TIMER".to_string(), String::new())),
        "WAITING_EVENT" => Some(("WAITING_EVENT".to_string(), String::new())),
        "WAITING_TAKEOVER" => Some(("WAITING_TAKEOVER".to_string(), String::new())),
        _ => None,
    };
    if let Some((state, key)) = wait {
        return SafeAction::Wait { state, key };
    }
    if state.parked_on_external {
        return SafeAction::Wait {
            state: state.run_status.clone(),
            key: String::new(),
        };
    }
    SafeAction::Resume
}

/// Whether an actor carrying `observed` generation may act on a run at `current`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceDecision {
    /// The actor is current and may act.
    Current,
    /// The actor is behind and everything it produces must be discarded.
    Stale,
}

/// Fence an actor by generation (DOMAIN.md §5.2, §5.7).
///
/// Mirrors the runtime's own rule (`Generation::accept`, `runtime::state_machine::fence`): an
/// actor whose generation is **behind** the row's is stale and everything it produces is
/// discarded, while an equal or newer generation is current — a newer generation is how a
/// restarted runtime supersedes the one that died, so refusing it would refuse recovery itself.
#[must_use]
pub fn fence_decision(observed: u64, current: u64) -> FenceDecision {
    if observed >= current {
        FenceDecision::Current
    } else {
        FenceDecision::Stale
    }
}

/// Enforce [`fence_decision`], returning the typed refusal for a stale actor.
///
/// # Errors
/// Returns [`RecoveryError::StaleGeneration`] when the actor is not current.
pub fn fence(observed: u64, current: u64) -> Result<(), RecoveryError> {
    match fence_decision(observed, current) {
        FenceDecision::Current => Ok(()),
        FenceDecision::Stale => Err(RecoveryError::StaleGeneration { observed, current }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(status: &str) -> DurableState {
        DurableState {
            run_status: status.to_string(),
            ..DurableState::default()
        }
    }

    #[test]
    fn the_kill_restart_matrix_decides_the_same_safe_point() {
        // Terminal runs are left exactly as they are.
        for status in ["SUCCEEDED", "FAILED", "CANCELLED", "BLOCKED_UNRECOVERABLE"] {
            assert_eq!(plan_from(&state(status)), SafeAction::Terminal, "{status}");
        }
        // Work in flight resumes at its next turn.
        for status in ["CREATED", "QUEUED", "RUNNING", "VERIFYING", "SUSPENDED"] {
            assert_eq!(plan_from(&state(status)), SafeAction::Resume, "{status}");
        }
        // Parked runs stay parked until their own resolution arrives.
        let mut approval = state("WAITING_APPROVAL");
        approval.pending_approvals = vec!["apr_1".to_string()];
        assert_eq!(
            plan_from(&approval),
            SafeAction::Wait {
                state: "WAITING_APPROVAL".to_string(),
                key: "apr_1".to_string()
            }
        );
        let mut question = state("WAITING_QUESTION");
        question.open_questions = vec!["q_1".to_string()];
        assert_eq!(
            plan_from(&question),
            SafeAction::Wait {
                state: "WAITING_QUESTION".to_string(),
                key: "q_1".to_string()
            }
        );
        let mut child = state("WAITING_CHILD");
        child.child_agent_threads = vec!["ath_1".to_string()];
        assert_eq!(
            plan_from(&child),
            SafeAction::Wait {
                state: "WAITING_CHILD".to_string(),
                key: "ath_1".to_string()
            }
        );
        for status in ["WAITING_TIMER", "WAITING_EVENT", "WAITING_TAKEOVER"] {
            let action = plan_from(&state(status));
            assert!(
                matches!(action, SafeAction::Wait { .. }),
                "{status} stays parked, got {action:?}"
            );
        }
    }

    #[test]
    fn cancellation_outranks_reconciliation_and_waits() {
        let mut cancel = state("WAITING_APPROVAL");
        cancel.cancellation_requested = true;
        cancel.pending_approvals = vec!["apr_1".to_string()];
        cancel.unsettled_effect_id = Some("eff_1".to_string());
        cancel.unsettled_tool_call_id = Some("tc_1".to_string());
        assert_eq!(plan_from(&cancel), SafeAction::Cancel);
    }

    #[test]
    fn an_uncertain_effect_is_reconciled_before_work_resumes() {
        let mut running = state("RUNNING");
        running.unsettled_effect_id = Some("eff_1".to_string());
        running.unsettled_tool_call_id = Some("tc_1".to_string());
        assert_eq!(
            plan_from(&running),
            SafeAction::ReconcileEffect {
                effect_id: "eff_1".to_string(),
                tool_call_id: "tc_1".to_string()
            }
        );
        // A settled effect leaves nothing to reconcile: the run simply resumes.
        assert_eq!(plan_from(&state("RUNNING")), SafeAction::Resume);
    }

    #[test]
    fn a_behind_generation_is_stale_and_a_newer_one_supersedes() {
        assert_eq!(fence_decision(3, 3), FenceDecision::Current);
        assert_eq!(fence_decision(2, 3), FenceDecision::Stale);
        assert_eq!(
            fence_decision(4, 3),
            FenceDecision::Current,
            "a newer generation supersedes the run's, as Generation::accept does"
        );
        let error = fence(2, 3).expect_err("stale");
        assert!(matches!(
            error,
            RecoveryError::StaleGeneration {
                observed: 2,
                current: 3
            }
        ));
        assert_eq!(error.code(), "FENCED_STALE_GENERATION");
    }
}
