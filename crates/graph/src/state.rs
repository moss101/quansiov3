//! Canonical graph states and their legal transitions (DOMAIN.md §4.1–§4.2, §5.1–§5.5).
//!
//! The transition tables are pure functions so they can be unit-tested without a
//! database, and the stores apply them before issuing any statement. A transition that is
//! not listed here fails closed: the row is never updated.
//!
//! `WorkNodeStatus` deliberately includes `Verifying → Done` in the state machine, but the
//! only store method that may write `done` is `GraphStore::mark_verification_passed`; the
//! generic `GraphStore::transition_node` refuses a `done` target so a caller cannot flip
//! status arbitrarily (DOMAIN.md §4.4: done only via the verification path).

use crate::error::{Entity, GraphError};

/// Kind of a [`crate::work::WorkNode`] (DOMAIN.md §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkNodeKind {
    /// Root long-running goal.
    Objective,
    /// Unit of work under an objective.
    Task,
    /// Unit of work under a task.
    Subtask,
    /// Node that waits on an external condition.
    Wait,
    /// Marker node for a completion point.
    Milestone,
}

impl WorkNodeKind {
    /// Every kind, in DOMAIN.md order.
    pub const ALL: [Self; 5] = [
        Self::Objective,
        Self::Task,
        Self::Subtask,
        Self::Wait,
        Self::Milestone,
    ];

    /// The stored lowercase form.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Objective => "objective",
            Self::Task => "task",
            Self::Subtask => "subtask",
            Self::Wait => "wait",
            Self::Milestone => "milestone",
        }
    }

    /// Parse the stored form.
    ///
    /// # Errors
    /// Returns [`GraphError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, GraphError> {
        match value {
            "objective" => Ok(Self::Objective),
            "task" => Ok(Self::Task),
            "subtask" => Ok(Self::Subtask),
            "wait" => Ok(Self::Wait),
            "milestone" => Ok(Self::Milestone),
            other => Err(GraphError::unknown_state(Entity::WorkNode, other)),
        }
    }
}

/// Status of a [`crate::work::WorkNode`] (DOMAIN.md §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkNodeStatus {
    /// Not yet planned.
    Draft,
    /// Planned and dispatchable.
    Ready,
    /// Waiting on a dependency or a blocker.
    Blocked,
    /// A Run is executing the node.
    InProgress,
    /// Waiting for a question, timer, approval or child result.
    Waiting,
    /// CompletionContract verification is running.
    Verifying,
    /// Verified complete.
    Done,
    /// Ended unsuccessfully.
    Failed,
    /// Cancelled before completion.
    Cancelled,
}

impl WorkNodeStatus {
    /// Every status, in DOMAIN.md order.
    pub const ALL: [Self; 9] = [
        Self::Draft,
        Self::Ready,
        Self::Blocked,
        Self::InProgress,
        Self::Waiting,
        Self::Verifying,
        Self::Done,
        Self::Failed,
        Self::Cancelled,
    ];

    /// The stored lowercase form.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Ready => "ready",
            Self::Blocked => "blocked",
            Self::InProgress => "in_progress",
            Self::Waiting => "waiting",
            Self::Verifying => "verifying",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse the stored form.
    ///
    /// # Errors
    /// Returns [`GraphError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, GraphError> {
        match value {
            "draft" => Ok(Self::Draft),
            "ready" => Ok(Self::Ready),
            "blocked" => Ok(Self::Blocked),
            "in_progress" => Ok(Self::InProgress),
            "waiting" => Ok(Self::Waiting),
            "verifying" => Ok(Self::Verifying),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(GraphError::unknown_state(Entity::WorkNode, other)),
        }
    }

    /// A terminal node is never mutated again.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }

    /// Whether `self → to` is a legal WorkNode transition.
    ///
    /// `Verifying → Done` is legal in the state machine but reachable only through
    /// [`crate::GraphStore::mark_verification_passed`].
    #[must_use]
    pub const fn can_transition_to(self, to: Self) -> bool {
        use WorkNodeStatus::{
            Blocked, Cancelled, Done, Draft, Failed, InProgress, Ready, Verifying, Waiting,
        };
        match self {
            Draft => matches!(to, Ready | Blocked | Cancelled),
            Ready => matches!(to, InProgress | Waiting | Blocked | Cancelled),
            Blocked => matches!(to, Ready | InProgress | Failed | Cancelled),
            InProgress => matches!(to, Waiting | Verifying | Blocked | Failed | Cancelled),
            Waiting => matches!(to, InProgress | Ready | Verifying | Failed | Cancelled),
            Verifying => matches!(to, Done | InProgress | Failed),
            Done | Failed | Cancelled => false,
        }
    }
}

/// Kind of a [`crate::work::WorkEdge`] (DOMAIN.md §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkEdgeKind {
    /// `from` depends on `to`; must stay acyclic.
    DependsOn,
    /// `from` is the parent of `to`; must stay acyclic.
    ParentOf,
    /// `from` produces an artifact associated with `to`.
    ProducesArtifact,
    /// `from` is verified by `to`.
    VerifiedBy,
    /// `from` is blocked by `to`.
    BlockedBy,
}

impl WorkEdgeKind {
    /// Every kind, in DOMAIN.md order.
    pub const ALL: [Self; 5] = [
        Self::DependsOn,
        Self::ParentOf,
        Self::ProducesArtifact,
        Self::VerifiedBy,
        Self::BlockedBy,
    ];

    /// The stored lowercase form.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::DependsOn => "depends_on",
            Self::ParentOf => "parent_of",
            Self::ProducesArtifact => "produces_artifact",
            Self::VerifiedBy => "verified_by",
            Self::BlockedBy => "blocked_by",
        }
    }

    /// Parse the stored form.
    ///
    /// # Errors
    /// Returns [`GraphError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, GraphError> {
        match value {
            "depends_on" => Ok(Self::DependsOn),
            "parent_of" => Ok(Self::ParentOf),
            "produces_artifact" => Ok(Self::ProducesArtifact),
            "verified_by" => Ok(Self::VerifiedBy),
            "blocked_by" => Ok(Self::BlockedBy),
            other => Err(GraphError::unknown_state(Entity::WorkEdge, other)),
        }
    }

    /// The relation families that must remain acyclic (DOMAIN.md §4.2).
    #[must_use]
    pub const fn is_acyclic_relation(self) -> bool {
        matches!(self, Self::DependsOn | Self::ParentOf)
    }
}

/// How a [`crate::work::WorkNode`] came into existence (DOMAIN.md §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkOrigin {
    /// Created directly by a user command.
    User,
    /// Accepted from a model PlanProposal.
    PlanProposal,
    /// Created by a Routine.
    Routine,
    /// Created by a Business Capability Pack.
    Pack,
}

impl WorkOrigin {
    /// Every origin, in DOMAIN.md order.
    pub const ALL: [Self; 4] = [Self::User, Self::PlanProposal, Self::Routine, Self::Pack];

    /// The stored lowercase form.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::PlanProposal => "plan_proposal",
            Self::Routine => "routine",
            Self::Pack => "pack",
        }
    }

    /// Parse the stored form.
    ///
    /// # Errors
    /// Returns [`GraphError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, GraphError> {
        match value {
            "user" => Ok(Self::User),
            "plan_proposal" => Ok(Self::PlanProposal),
            "routine" => Ok(Self::Routine),
            "pack" => Ok(Self::Pack),
            other => Err(GraphError::unknown_state(Entity::WorkNode, other)),
        }
    }
}

/// Whether an AgentThread is a persistent teammate or an ephemeral worker (DOMAIN.md §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    /// Persistent agent definition instance.
    Teammate,
    /// Ephemeral delegated instance.
    Worker,
}

impl AgentKind {
    /// Every kind, in DOMAIN.md order.
    pub const ALL: [Self; 2] = [Self::Teammate, Self::Worker];

    /// The stored lowercase form.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Teammate => "teammate",
            Self::Worker => "worker",
        }
    }

    /// Parse the stored form.
    ///
    /// # Errors
    /// Returns [`GraphError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, GraphError> {
        match value {
            "teammate" => Ok(Self::Teammate),
            "worker" => Ok(Self::Worker),
            other => Err(GraphError::unknown_state(Entity::AgentThread, other)),
        }
    }
}

/// Lifecycle status of an AgentThread (DOMAIN.md §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentThreadStatus {
    /// Created, capability projection not yet resolved.
    Provisioned,
    /// Participating in runs.
    Active,
    /// Paused (budget, kill switch, takeover).
    Suspended,
    /// Handing work to another agent thread.
    HandingOff,
    /// Handoff completed.
    HandedOff,
    /// Worker merging its results back into the parent.
    Joining,
    /// Worker merged into its parent.
    Joined,
    /// Ended.
    Terminated,
}

impl AgentThreadStatus {
    /// Every status, in DOMAIN.md order.
    pub const ALL: [Self; 8] = [
        Self::Provisioned,
        Self::Active,
        Self::Suspended,
        Self::HandingOff,
        Self::HandedOff,
        Self::Joining,
        Self::Joined,
        Self::Terminated,
    ];

    /// The stored uppercase form.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Provisioned => "PROVISIONED",
            Self::Active => "ACTIVE",
            Self::Suspended => "SUSPENDED",
            Self::HandingOff => "HANDING_OFF",
            Self::HandedOff => "HANDED_OFF",
            Self::Joining => "JOINING",
            Self::Joined => "JOINED",
            Self::Terminated => "TERMINATED",
        }
    }

    /// Parse the stored form.
    ///
    /// # Errors
    /// Returns [`GraphError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, GraphError> {
        match value {
            "PROVISIONED" => Ok(Self::Provisioned),
            "ACTIVE" => Ok(Self::Active),
            "SUSPENDED" => Ok(Self::Suspended),
            "HANDING_OFF" => Ok(Self::HandingOff),
            "HANDED_OFF" => Ok(Self::HandedOff),
            "JOINING" => Ok(Self::Joining),
            "JOINED" => Ok(Self::Joined),
            "TERMINATED" => Ok(Self::Terminated),
            other => Err(GraphError::unknown_state(Entity::AgentThread, other)),
        }
    }

    /// A terminal agent thread is never mutated again.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Terminated)
    }

    /// Whether `self → to` is a legal AgentThread transition (DOMAIN.md §5.1).
    #[must_use]
    pub const fn can_transition_to(self, to: Self) -> bool {
        use AgentThreadStatus::{
            Active, HandedOff, HandingOff, Joined, Joining, Provisioned, Suspended, Terminated,
        };
        match self {
            Provisioned => matches!(to, Active | Terminated),
            Active => matches!(to, Suspended | HandingOff | Joining | Terminated),
            Suspended => matches!(to, Active | Terminated),
            HandingOff => matches!(to, HandedOff | Terminated),
            HandedOff => matches!(to, Terminated),
            Joining => matches!(to, Joined | Terminated),
            Joined => matches!(to, Terminated),
            Terminated => false,
        }
    }
}

/// Status of a [`crate::runtime::Run`] (DOMAIN.md §5.2).
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
    /// Returns [`GraphError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, GraphError> {
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
            other => Err(GraphError::unknown_state(Entity::Run, other)),
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

    /// A terminal run is never mutated again.
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
            // Handled by `is_waiting` above; kept exhaustive without a wildcard.
            Self::WaitingApproval
            | Self::WaitingQuestion
            | Self::WaitingEvent
            | Self::WaitingTimer
            | Self::WaitingChild
            | Self::WaitingTakeover => false,
        }
    }
}

/// What started a Run (DOMAIN.md §5.2).
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
    /// Returns [`GraphError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, GraphError> {
        match value {
            "message" => Ok(Self::Message),
            "routine" => Ok(Self::Routine),
            "wake" => Ok(Self::Wake),
            "child_result" => Ok(Self::ChildResult),
            "manual" => Ok(Self::Manual),
            other => Err(GraphError::unknown_state(Entity::Run, other)),
        }
    }
}

/// Status of a [`crate::runtime::Turn`] (DOMAIN.md §5.3).
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
    /// Returns [`GraphError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, GraphError> {
        match value {
            "active" => Ok(Self::Active),
            "completed" => Ok(Self::Completed),
            "aborted" => Ok(Self::Aborted),
            other => Err(GraphError::unknown_state(Entity::Turn, other)),
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

/// Kind of a [`crate::runtime::Step`] (DOMAIN.md §5.4).
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
    /// Returns [`GraphError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, GraphError> {
        match value {
            "model_call" => Ok(Self::ModelCall),
            "tool_call" => Ok(Self::ToolCall),
            "delegate" => Ok(Self::Delegate),
            "wait" => Ok(Self::Wait),
            "verify" => Ok(Self::Verify),
            "checkpoint" => Ok(Self::Checkpoint),
            "compact" => Ok(Self::Compact),
            other => Err(GraphError::unknown_state(Entity::Step, other)),
        }
    }
}

/// Status of a [`crate::runtime::Step`] (DOMAIN.md §5.4).
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
    /// Returns [`GraphError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, GraphError> {
        match value {
            "pending" => Ok(Self::Pending),
            "dispatched" => Ok(Self::Dispatched),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "unknown" => Ok(Self::Unknown),
            other => Err(GraphError::unknown_state(Entity::Step, other)),
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

/// Status of a [`crate::runtime::Attempt`] (DOMAIN.md §5.5).
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
    /// Returns [`GraphError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, GraphError> {
        match value {
            "started" => Ok(Self::Started),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "timed_out" => Ok(Self::TimedOut),
            "fenced" => Ok(Self::Fenced),
            other => Err(GraphError::unknown_state(Entity::Attempt, other)),
        }
    }

    /// A finished attempt cannot be finished again.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Started)
    }

    /// Whether `self → to` is a legal Attempt transition.
    ///
    /// Retrying a Step never reuses an Attempt row: it appends a new one (DOMAIN.md §5.5).
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
    fn every_status_declares_a_round_tripping_db_string() {
        for status in WorkNodeStatus::ALL {
            assert_eq!(
                WorkNodeStatus::from_db_str(status.as_db_str()).expect("work node status"),
                status
            );
        }
        for status in AgentThreadStatus::ALL {
            assert_eq!(
                AgentThreadStatus::from_db_str(status.as_db_str()).expect("agent thread status"),
                status
            );
        }
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
    }

    #[test]
    fn kinds_and_origins_round_trip_through_their_db_strings() {
        for kind in WorkNodeKind::ALL {
            assert_eq!(
                WorkNodeKind::from_db_str(kind.as_db_str()).expect("kind"),
                kind
            );
        }
        for kind in WorkEdgeKind::ALL {
            assert_eq!(
                WorkEdgeKind::from_db_str(kind.as_db_str()).expect("edge kind"),
                kind
            );
        }
        for origin in WorkOrigin::ALL {
            assert_eq!(
                WorkOrigin::from_db_str(origin.as_db_str()).expect("origin"),
                origin
            );
        }
        for kind in AgentKind::ALL {
            assert_eq!(
                AgentKind::from_db_str(kind.as_db_str()).expect("agent kind"),
                kind
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
        for kind in WorkNodeKind::ALL {
            assert!(!kind.as_db_str().is_empty());
        }
    }

    #[test]
    fn unknown_state_strings_fail_closed() {
        assert!(matches!(
            WorkNodeStatus::from_db_str("almost_done"),
            Err(GraphError::UnknownState { .. })
        ));
        assert!(matches!(
            RunStatus::from_db_str("RUNNING_FAST"),
            Err(GraphError::UnknownState { .. })
        ));
        assert!(matches!(
            StepStatus::from_db_str("half"),
            Err(GraphError::UnknownState { .. })
        ));
    }

    #[test]
    fn terminal_work_node_states_never_transition() {
        for status in [
            WorkNodeStatus::Done,
            WorkNodeStatus::Failed,
            WorkNodeStatus::Cancelled,
        ] {
            assert!(status.is_terminal());
            for to in WorkNodeStatus::ALL {
                assert!(!status.can_transition_to(to));
            }
        }
    }

    #[test]
    fn run_reaches_success_only_through_verifying() {
        assert!(RunStatus::Created.can_transition_to(RunStatus::Queued));
        assert!(RunStatus::Queued.can_transition_to(RunStatus::Running));
        assert!(RunStatus::Running.can_transition_to(RunStatus::Verifying));
        assert!(RunStatus::Verifying.can_transition_to(RunStatus::Succeeded));
        assert!(
            !RunStatus::Running.can_transition_to(RunStatus::Succeeded),
            "a run cannot succeed without verification"
        );
        assert!(RunStatus::Verifying.can_transition_to(RunStatus::Running));
        for waiting in RunStatus::WAITING {
            assert!(waiting.is_waiting());
            assert!(waiting.can_transition_to(RunStatus::Running));
            assert!(waiting.can_transition_to(RunStatus::Suspended));
            assert!(!waiting.can_transition_to(RunStatus::Succeeded));
        }
        assert!(RunStatus::Running.can_transition_to(RunStatus::Suspended));
        assert!(RunStatus::Suspended.can_transition_to(RunStatus::Running));
        assert!(RunStatus::Suspended.can_transition_to(RunStatus::Failed));
    }

    #[test]
    fn run_terminals_are_immutable() {
        for status in [
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::Cancelled,
            RunStatus::BlockedUnrecoverable,
        ] {
            for to in RunStatus::ALL {
                assert!(!status.can_transition_to(to));
            }
        }
    }

    #[test]
    fn step_and_attempt_transitions_fail_closed() {
        assert!(StepStatus::Pending.can_transition_to(StepStatus::Dispatched));
        assert!(StepStatus::Dispatched.can_transition_to(StepStatus::Unknown));
        assert!(StepStatus::Unknown.can_transition_to(StepStatus::Completed));
        assert!(!StepStatus::Completed.can_transition_to(StepStatus::Dispatched));

        assert!(AttemptStatus::Started.can_transition_to(AttemptStatus::Succeeded));
        assert!(AttemptStatus::Started.can_transition_to(AttemptStatus::Fenced));
        assert!(!AttemptStatus::Succeeded.can_transition_to(AttemptStatus::Failed));
    }

    #[test]
    fn only_depends_on_and_parent_of_relations_must_be_acyclic() {
        assert!(WorkEdgeKind::DependsOn.is_acyclic_relation());
        assert!(WorkEdgeKind::ParentOf.is_acyclic_relation());
        assert!(!WorkEdgeKind::ProducesArtifact.is_acyclic_relation());
        assert!(!WorkEdgeKind::VerifiedBy.is_acyclic_relation());
        assert!(!WorkEdgeKind::BlockedBy.is_acyclic_relation());
    }
}
