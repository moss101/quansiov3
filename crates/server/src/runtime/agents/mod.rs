//! AgentThread lifecycle, mailbox, delegation, handoff and join (RUN-002; DOMAIN.md
//! §4.3, §5.1, §5.6, §6.3, §9.2).
//!
//! One runtime owns one AgentGraph: this module is the runtime side of the
//! `agent_threads` / `agent_graph_edges` rows CORE-004 defines and the `agent.*` events
//! DOMAIN.md §9.2 names. Every mutation is one PostgreSQL transaction through
//! [`EventStore::commit_mutation_tx`] — the same primitive `crates/graph`'s
//! `GraphTransaction` builds on — so a lifecycle change, a mailbox delivery, a handoff or
//! a join commits together with its RuntimeEvent or not at all.
//!
//! `crates/server` cannot depend on `crates/graph` (that crate depends on this one for
//! `control::schema`, and the workspace gate rejects the cycle), so the runtime owns
//! these rows exactly as it owns Runs/Turns/Steps in [`super::state_machine`]: same
//! tables, same DOMAIN state values, same event names. `crates/graph::agent` remains the
//! graph-transaction API for callers that own a WorkGraph revision.
//!
//! Lifecycle policy (DOMAIN.md §5.1) is one state table plus two rules, so persistent
//! teammates and ephemeral workers share every primitive:
//!
//! * a persistent teammate never reaches `JOINED` (nor `JOINING`);
//! * a worker must `JOIN` before it can terminate normally.
//!
//! `JOINING → JOINED` is applied atomically by [`store::AgentStore::join_worker`], which
//! merges the worker's outcome into its parent in the same transaction; DOMAIN.md §9.2
//! names `agent.joined` but no separate event for the `JOINING` intermediate, so the
//! intermediate is entered and left inside that one committed change rather than
//! recorded under an invented event name.

pub mod port;
pub mod store;

use quansio_core::Generation;

use super::state_machine::RuntimeError;
pub use port::AgentDelegationPort;
pub use store::{
    AgentHandoff, AgentJoin, AgentStore, AgentThread, Delegated, DelegationEdge,
    DelegationNarrowingCheck, HandoffStatus, JoinRequest, Joined, MailboxCursor, MailboxItem,
    NewAgentThread, NewDelegation, NewHandoff, NewMailboxItem, StructuralDelegationCheck,
    AGENTS_OWNER,
};

/// Policy rule: a persistent teammate never joins a parent.
pub const TEAMMATE_CANNOT_JOIN: &str = "teammate_cannot_join";
/// Policy rule: a worker must join before it can terminate normally.
pub const WORKER_MUST_JOIN_BEFORE_TERMINATE: &str = "worker_must_join_before_terminate";

/// A persistent teammate or a scoped ephemeral worker (DOMAIN.md §5.1 `agent.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    /// Persistent teammate: never joins, terminates directly.
    Teammate,
    /// Ephemeral worker: joins its parent before terminating normally.
    Worker,
}

impl AgentKind {
    /// The stored form.
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
    /// Returns [`RuntimeError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, RuntimeError> {
        match value {
            "teammate" => Ok(Self::Teammate),
            "worker" => Ok(Self::Worker),
            other => Err(RuntimeError::unknown_state("agent_thread", other)),
        }
    }
}

/// Lifecycle status of an AgentThread (DOMAIN.md §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentThreadStatus {
    /// Created; capability projection not yet resolved.
    Provisioned,
    /// Participating in runs.
    Active,
    /// Paused (budget, kill switch, takeover).
    Suspended,
    /// Handing live work to another AgentThread.
    HandingOff,
    /// Handoff completed.
    HandedOff,
    /// Worker merging its outcome into its parent.
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

    /// The stored form.
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
    /// Returns [`RuntimeError::UnknownState`] for an unrecognised value.
    pub fn from_db_str(value: &str) -> Result<Self, RuntimeError> {
        match value {
            "PROVISIONED" => Ok(Self::Provisioned),
            "ACTIVE" => Ok(Self::Active),
            "SUSPENDED" => Ok(Self::Suspended),
            "HANDING_OFF" => Ok(Self::HandingOff),
            "HANDED_OFF" => Ok(Self::HandedOff),
            "JOINING" => Ok(Self::Joining),
            "JOINED" => Ok(Self::Joined),
            "TERMINATED" => Ok(Self::Terminated),
            other => Err(RuntimeError::unknown_state("agent_thread", other)),
        }
    }

    /// A terminated agent thread is never mutated again.
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

/// The DOMAIN.md §5.1 lifecycle policy both agent kinds share (RUN-002).
///
/// The state table above is kind-independent; this function adds the two policy rules.
/// Callers reject before writing anything, so a refused transition leaves state and the
/// event log untouched.
///
/// # Errors
/// Returns [`RuntimeError::LifecyclePolicy`] when the ordered pair is legal but the
/// kind's policy forbids it.
pub fn check_lifecycle_policy(
    kind: AgentKind,
    from: AgentThreadStatus,
    to: AgentThreadStatus,
) -> Result<(), RuntimeError> {
    let refuse = |rule: &'static str| {
        Err(RuntimeError::LifecyclePolicy {
            from: from.as_db_str().to_string(),
            to: to.as_db_str().to_string(),
            rule,
        })
    };
    if kind == AgentKind::Teammate
        && matches!(to, AgentThreadStatus::Joining | AgentThreadStatus::Joined)
    {
        return refuse(TEAMMATE_CANNOT_JOIN);
    }
    if kind == AgentKind::Worker
        && to == AgentThreadStatus::Terminated
        && from != AgentThreadStatus::Joined
    {
        return refuse(WORKER_MUST_JOIN_BEFORE_TERMINATE);
    }
    Ok(())
}

/// The `agent.*` event for a lifecycle transition, when DOMAIN.md §9.2 names one
/// (RUN-002).
///
/// `PROVISIONED` is the creation event; `JOINING` has no §9.2 name and is only ever
/// entered inside the atomic `join`, so it is never the target of a standalone event.
#[must_use]
pub const fn agent_event_name(to: AgentThreadStatus) -> Option<&'static str> {
    match to {
        AgentThreadStatus::Provisioned | AgentThreadStatus::Joining => None,
        AgentThreadStatus::Active => Some("agent.activated"),
        AgentThreadStatus::Suspended => Some("agent.suspended"),
        AgentThreadStatus::HandingOff => Some("agent.handoff_started"),
        AgentThreadStatus::HandedOff => Some("agent.handoff_completed"),
        AgentThreadStatus::Joined => Some("agent.joined"),
        AgentThreadStatus::Terminated => Some("agent.terminated"),
    }
}

/// Reject a mutation whose generation is behind the row's current generation.
///
/// AgentThread mutations carry the generation they were issued under (DOMAIN.md §1.2);
/// fencing runs before any write, so a stale caller changes nothing.
///
/// # Errors
/// Returns [`RuntimeError::FencedStaleGeneration`] when `received` is behind `current`.
pub(crate) fn fence(current: Generation, received: Generation) -> Result<(), RuntimeError> {
    super::state_machine::fence(current, received)
}
