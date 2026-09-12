//! Run, Turn, Step and Attempt states and their legal transitions (DOMAIN.md §5.2–§5.5).
//!
//! These are the runtime's typed states. `crates/graph` owns an equivalent StateGraph
//! store, but `quansio-graph` depends on `quansio-server` for `control::schema`, so
//! `crates/server` cannot depend on it without a package cycle (see
//! `crate::scheduler::dispatch`). The stored strings and the transition tables below are
//! therefore exactly the DOMAIN.md §5.2–§5.5 values and rules, so the runtime and the
//! graph store cannot drift in meaning.
//!
//! Every table is a pure `const fn`, so legality is decided before a transaction opens
//! and an illegal transition can never reach a write.

use super::RuntimeError;

/// Status of a Run (DOMAIN.md §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunStatus {
    /// Persisted, not yet scheduled.
    Created,
    /// Scheduled and waiting for execution capacity.
    Queued,
    /// The turn loop is executing.
    Running,
    /// Parked pending an approval.
    WaitingApproval,
    /// Parked pending a human answer.
    WaitingQuestion,
    /// Parked pending an external event.
    WaitingEvent,
    /// Parked pending a timer.
    WaitingTimer,
    /// Parked pending a child agent result.
    WaitingChild,
    /// Parked while a user holds browser/target control.
    WaitingTakeover,
    /// CompletionContract verification is running.
    Verifying,
    /// Terminal success.
    Succeeded,
    /// Terminal failure.
    Failed,
    /// Terminal cancellation.
    Cancelled,
    /// Terminal, unrecoverable with a typed reason.
    BlockedUnrecoverable,
    /// Paused (budget, kill switch, target loss).
    Suspended,
}

impl RunStatus {
    /// Every status, in DOMAIN.md order.
    pub const ALL: [Self; 15] = [
        Self::Created,
        Self::Queued,
        Self::Running,
        Self::WaitingApproval,
        Self::WaitingQuestion,
        Self::WaitingEvent,
        Self::WaitingTimer,
        Self::WaitingChild,
        Self::WaitingTakeover,
        Self::Verifying,
        Self::Succeeded,
        Self::Failed,
        Self::Cancelled,
        Self::BlockedUnrecoverable,
        Self::Suspended,
    ];

    /// Every `WAITING_*` status.
    pub const WAITING: [Self; 6] = [
        Self::WaitingApproval,
        Self::WaitingQuestion,
        Self::WaitingEvent,
        Self::WaitingTimer,
        Self::WaitingChild,
        Self::WaitingTakeover,
    ];

    /// The stored uppercase form.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Created => "CREATED",
            Self::Queued => "QUEUED",
            Self::Running => "RUNNING",
            Self::WaitingApproval => "WAITING_APPROVAL",
            Self::WaitingQuestion => "WAITING_QUESTION",
            Self::WaitingEvent => "WAITING_EVENT",
            Self::WaitingTimer => "WAITING_TIMER",
            Self::WaitingChild => "WAITING_CHILD",
            Self::WaitingTakeover => "WAITING_TAKEOVER",
            Self::Verifying => "VERIFYING",
            Self::Succeeded => "SUCCEEDED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
            Self::BlockedUnrecoverable => "BLOCKED_UNRECOVERABLE",
            Self::Suspended => "SUSPENDED",
        }
    }

    /// Parse the stored form.
    ///
    /// # Errors
    /// Returns [`RuntimeError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, RuntimeError> {
        match value {
            "CREATED" => Ok(Self::Created),
            "QUEUED" => Ok(Self::Queued),
            "RUNNING" => Ok(Self::Running),
            "WAITING_APPROVAL" => Ok(Self::WaitingApproval),
            "WAITING_QUESTION" => Ok(Self::WaitingQuestion),
            "WAITING_EVENT" => Ok(Self::WaitingEvent),
            "WAITING_TIMER" => Ok(Self::WaitingTimer),
            "WAITING_CHILD" => Ok(Self::WaitingChild),
            "WAITING_TAKEOVER" => Ok(Self::WaitingTakeover),
            "VERIFYING" => Ok(Self::Verifying),
            "SUCCEEDED" => Ok(Self::Succeeded),
            "FAILED" => Ok(Self::Failed),
            "CANCELLED" => Ok(Self::Cancelled),
            "BLOCKED_UNRECOVERABLE" => Ok(Self::BlockedUnrecoverable),
            "SUSPENDED" => Ok(Self::Suspended),
            other => Err(RuntimeError::unknown_state("run", other)),
        }
    }

    /// Whether this is a `WAITING_*` state.
    #[must_use]
    pub const fn is_waiting(self) -> bool {
        matches!(
            self,
            Self::WaitingApproval
                | Self::WaitingQuestion
                | Self::WaitingEvent
                | Self::WaitingTimer
                | Self::WaitingChild
                | Self::WaitingTakeover
        )
    }

    /// A terminal run is never mutated again (DOMAIN.md §5.2).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::BlockedUnrecoverable
        )
    }

    /// Whether `self → to` is a legal Run transition (DOMAIN.md §5.2).
    #[must_use]
    pub const fn can_transition_to(self, to: Self) -> bool {
        use RunStatus::{
            BlockedUnrecoverable, Cancelled, Created, Failed, Queued, Running, Succeeded,
            Suspended, Verifying,
        };
        if self.is_waiting() {
            return matches!(to, Running | Cancelled | Failed | Suspended);
        }
        match self {
            Created => matches!(to, Queued),
            Queued => matches!(to, Running),
            Running => matches!(
                to,
                Self::WaitingApproval
                    | Self::WaitingQuestion
                    | Self::WaitingEvent
                    | Self::WaitingTimer
                    | Self::WaitingChild
                    | Self::WaitingTakeover
                    | Verifying
                    | Suspended
                    | Failed
                    | Cancelled
                    | BlockedUnrecoverable
            ),
            Verifying => matches!(to, Succeeded | Running),
            Suspended => matches!(to, Running | Failed),
            Succeeded | Failed | Cancelled | BlockedUnrecoverable => false,
            Self::WaitingApproval
            | Self::WaitingQuestion
            | Self::WaitingEvent
            | Self::WaitingTimer
            | Self::WaitingChild
            | Self::WaitingTakeover => false,
        }
    }
}

/// What started a Run or a Turn (DOMAIN.md §5.2, §5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunTriggerKind {
    /// A user or agent message.
    Message,
    /// A Routine firing.
    Routine,
    /// A wake from suspension.
    Wake,
    /// A delegated child's result.
    ChildResult,
    /// An explicit operator action.
    Manual,
}

impl RunTriggerKind {
    /// Every trigger, in DOMAIN.md order.
    pub const ALL: [Self; 5] = [
        Self::Message,
        Self::Routine,
        Self::Wake,
        Self::ChildResult,
        Self::Manual,
    ];

    /// The stored lowercase form.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Routine => "routine",
            Self::Wake => "wake",
            Self::ChildResult => "child_result",
            Self::Manual => "manual",
        }
    }

    /// Parse the stored form.
    ///
    /// # Errors
    /// Returns [`RuntimeError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, RuntimeError> {
        match value {
            "message" => Ok(Self::Message),
            "routine" => Ok(Self::Routine),
            "wake" => Ok(Self::Wake),
            "child_result" => Ok(Self::ChildResult),
            "manual" => Ok(Self::Manual),
            other => Err(RuntimeError::unknown_state("run trigger", other)),
        }
    }
}

/// Status of a Turn (DOMAIN.md §5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TurnStatus {
    /// The bounded loop is executing.
    Active,
    /// The loop finished.
    Completed,
    /// The loop was aborted.
    Aborted,
}

impl TurnStatus {
    /// Every status, in DOMAIN.md order.
    pub const ALL: [Self; 3] = [Self::Active, Self::Completed, Self::Aborted];

    /// The stored lowercase form.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Aborted => "aborted",
        }
    }

    /// Parse the stored form.
    ///
    /// # Errors
    /// Returns [`RuntimeError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, RuntimeError> {
        match value {
            "active" => Ok(Self::Active),
            "completed" => Ok(Self::Completed),
            "aborted" => Ok(Self::Aborted),
            other => Err(RuntimeError::unknown_state("turn", other)),
        }
    }

    /// Whether `self → to` is a legal Turn transition.
    #[must_use]
    pub const fn can_transition_to(self, to: Self) -> bool {
        match self {
            Self::Active => matches!(to, Self::Completed | Self::Aborted),
            Self::Completed | Self::Aborted => false,
        }
    }
}

/// Kind of a Step (DOMAIN.md §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepKind {
    /// One model call.
    ModelCall,
    /// One tool call.
    ToolCall,
    /// One delegation.
    Delegate,
    /// One wait.
    Wait,
    /// One verification.
    Verify,
    /// One checkpoint.
    Checkpoint,
    /// One compaction.
    Compact,
}

impl StepKind {
    /// Every kind, in DOMAIN.md order.
    pub const ALL: [Self; 7] = [
        Self::ModelCall,
        Self::ToolCall,
        Self::Delegate,
        Self::Wait,
        Self::Verify,
        Self::Checkpoint,
        Self::Compact,
    ];

    /// The stored lowercase form.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::ModelCall => "model_call",
            Self::ToolCall => "tool_call",
            Self::Delegate => "delegate",
            Self::Wait => "wait",
            Self::Verify => "verify",
            Self::Checkpoint => "checkpoint",
            Self::Compact => "compact",
        }
    }

    /// Parse the stored form.
    ///
    /// # Errors
    /// Returns [`RuntimeError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, RuntimeError> {
        match value {
            "model_call" => Ok(Self::ModelCall),
            "tool_call" => Ok(Self::ToolCall),
            "delegate" => Ok(Self::Delegate),
            "wait" => Ok(Self::Wait),
            "verify" => Ok(Self::Verify),
            "checkpoint" => Ok(Self::Checkpoint),
            "compact" => Ok(Self::Compact),
            other => Err(RuntimeError::unknown_state("step kind", other)),
        }
    }
}

/// Status of a Step (DOMAIN.md §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepStatus {
    /// Not yet dispatched.
    Pending,
    /// Dispatched to its host.
    Dispatched,
    /// Finished successfully.
    Completed,
    /// Finished unsuccessfully.
    Failed,
    /// Cancelled.
    Cancelled,
    /// Dispatched with an unknown outcome, pending reconciliation.
    Unknown,
}

impl StepStatus {
    /// Every status, in DOMAIN.md order.
    pub const ALL: [Self; 6] = [
        Self::Pending,
        Self::Dispatched,
        Self::Completed,
        Self::Failed,
        Self::Cancelled,
        Self::Unknown,
    ];

    /// The stored lowercase form.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Dispatched => "dispatched",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }

    /// Parse the stored form.
    ///
    /// # Errors
    /// Returns [`RuntimeError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, RuntimeError> {
        match value {
            "pending" => Ok(Self::Pending),
            "dispatched" => Ok(Self::Dispatched),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "unknown" => Ok(Self::Unknown),
            other => Err(RuntimeError::unknown_state("step", other)),
        }
    }

    /// A terminal step is never mutated again.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Whether `self → to` is a legal Step transition.
    #[must_use]
    pub const fn can_transition_to(self, to: Self) -> bool {
        match self {
            Self::Pending => matches!(to, Self::Dispatched | Self::Failed | Self::Cancelled),
            Self::Dispatched => matches!(
                to,
                Self::Completed | Self::Failed | Self::Cancelled | Self::Unknown
            ),
            Self::Unknown => matches!(to, Self::Completed | Self::Failed | Self::Cancelled),
            Self::Completed | Self::Failed | Self::Cancelled => false,
        }
    }
}

/// Status of an Attempt (DOMAIN.md §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttemptStatus {
    /// Running.
    Started,
    /// Finished successfully.
    Succeeded,
    /// Finished unsuccessfully.
    Failed,
    /// Exceeded its timeout.
    TimedOut,
    /// Rejected because its generation was stale.
    Fenced,
}

impl AttemptStatus {
    /// Every status, in DOMAIN.md order.
    pub const ALL: [Self; 5] = [
        Self::Started,
        Self::Succeeded,
        Self::Failed,
        Self::TimedOut,
        Self::Fenced,
    ];

    /// The stored lowercase form.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Fenced => "fenced",
        }
    }

    /// Parse the stored form.
    ///
    /// # Errors
    /// Returns [`RuntimeError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, RuntimeError> {
        match value {
            "started" => Ok(Self::Started),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "timed_out" => Ok(Self::TimedOut),
            "fenced" => Ok(Self::Fenced),
            other => Err(RuntimeError::unknown_state("attempt", other)),
        }
    }

    /// A finished attempt cannot be finished again.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Started)
    }

    /// Whether `self → to` is a legal Attempt transition (DOMAIN.md §5.5).
    #[must_use]
    pub const fn can_transition_to(self, to: Self) -> bool {
        match self {
            Self::Started => matches!(
                to,
                Self::Succeeded | Self::Failed | Self::TimedOut | Self::Fenced
            ),
            Self::Succeeded | Self::Failed | Self::TimedOut | Self::Fenced => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_round_trips_through_its_stored_value() {
        for status in RunStatus::ALL {
            assert_eq!(
                RunStatus::from_db_str(status.as_db_str()).expect("run status"),
                status
            );
        }
        for status in TurnStatus::ALL {
            assert_eq!(
                TurnStatus::from_db_str(status.as_db_str()).expect("turn status"),
                status
            );
        }
        for status in StepStatus::ALL {
            assert_eq!(
                StepStatus::from_db_str(status.as_db_str()).expect("step status"),
                status
            );
        }
        for status in AttemptStatus::ALL {
            assert_eq!(
                AttemptStatus::from_db_str(status.as_db_str()).expect("attempt status"),
                status
            );
        }
        for trigger in RunTriggerKind::ALL {
            assert_eq!(
                RunTriggerKind::from_db_str(trigger.as_db_str()).expect("trigger"),
                trigger
            );
        }
        for kind in StepKind::ALL {
            assert_eq!(
                StepKind::from_db_str(kind.as_db_str()).expect("step kind"),
                kind
            );
        }
    }

    #[test]
    fn terminal_run_states_never_transition() {
        for status in [
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::Cancelled,
            RunStatus::BlockedUnrecoverable,
        ] {
            assert!(status.is_terminal());
            for to in RunStatus::ALL {
                assert!(!status.can_transition_to(to), "{status:?} -> {to:?}");
            }
        }
    }

    #[test]
    fn waiting_states_resume_cancel_fail_or_suspend_only() {
        for waiting in RunStatus::WAITING {
            for to in RunStatus::ALL {
                let expected = matches!(
                    to,
                    RunStatus::Running
                        | RunStatus::Cancelled
                        | RunStatus::Failed
                        | RunStatus::Suspended
                );
                assert_eq!(
                    waiting.can_transition_to(to),
                    expected,
                    "{waiting:?} -> {to:?}"
                );
            }
        }
    }

    #[test]
    fn run_lifecycle_states_match_domain_5_2() {
        assert!(RunStatus::Created.can_transition_to(RunStatus::Queued));
        assert!(!RunStatus::Created.can_transition_to(RunStatus::Running));
        assert!(RunStatus::Queued.can_transition_to(RunStatus::Running));
        assert!(!RunStatus::Queued.can_transition_to(RunStatus::Verifying));
        assert!(RunStatus::Running.can_transition_to(RunStatus::Verifying));
        assert!(RunStatus::Verifying.can_transition_to(RunStatus::Succeeded));
        assert!(RunStatus::Verifying.can_transition_to(RunStatus::Running));
        assert!(RunStatus::Suspended.can_transition_to(RunStatus::Running));
        assert!(RunStatus::Suspended.can_transition_to(RunStatus::Failed));
        assert!(!RunStatus::Suspended.can_transition_to(RunStatus::Verifying));
    }

    #[test]
    fn unknown_state_strings_fail_closed() {
        assert!(RunStatus::from_db_str("RUNNING_FAST").is_err());
        assert!(StepStatus::from_db_str("half").is_err());
        assert!(AttemptStatus::from_db_str("done").is_err());
    }
}
