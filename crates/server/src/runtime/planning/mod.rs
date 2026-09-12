//! Plan compilation and validation (RUN-003, DOMAIN.md §4.5, §5.6).
//!
//! A model proposes a plan; the trusted runtime validates it and commits it. This module
//! owns the *compilation/validation step that feeds* the canonical graph mutation path: it
//! turns the model's raw JSON proposal into a typed, bounded, validated [`CompiledPlan`]
//! and hands that to a [`PlanWorkspacePort`], whose production implementation applies it
//! through `quansio_graph`'s event-emitting `GraphTransaction` (`GraphTransaction::apply_plan`).
//!
//! The port exists for the same reason `scheduler::RunDispatch` does: `quansio-graph`
//! depends on `quansio-server` for `control::schema`, so `crates/server` cannot depend on
//! `quansio_graph` without a package cycle, and the workspace conformance gate forbids one.
//! Server integration tests therefore drive the compiler through a recording port, and
//! `crates/graph/tests/planning.rs` proves the graph-backed port applies the compiled plan
//! as one `GraphTransaction` with a single revision bump and deterministic events.
//!
//! Validation runs before the port is called, so a malformed, oversized, cyclic,
//! over-authorized or stale proposal never reaches the graph: nothing is mutated and the
//! rejection is recorded as a `work.plan_rejected` RuntimeEvent. Ingestion is recorded as
//! `work.plan_proposed` and acceptance as `work.plan_applied` (emitted by the graph
//! transaction), each exactly once.
//!
//! Enforced here, in the order the compiler checks them (DOMAIN.md §4.5):
//!
//! * the proposal is a well-formed `PlanProposal` with a rationale and canonical
//!   `kind`/`status`/edge-kind values (DOMAIN.md §4.1–§4.2);
//! * it is bounded by `policies.max_plan_nodes` (the tenant/workspace minimum, or
//!   [`DEFAULT_MAX_PLAN_NODES`] when no policy row exists);
//! * every node and edge identity is unique and every referenced endpoint exists in the
//!   proposal or the current graph;
//! * the resulting `depends_on`/`parent_of` structure — including `work_nodes.parent_id`
//!   chains — stays acyclic;
//! * every added node carries a CompletionContract or inherits one from its parent, and
//!   the compiler records which;
//! * every `capability_needs` template is a narrowing of the proposer's Capability
//!   Projection, checked with RUN-005's algebra (`quansio_capability`), so a plan can
//!   never widen authority;
//! * `base_revision` equals the current graph revision; the port's compare-and-set is the
//!   final authority, so a racing writer is rejected with `CONFLICT_REVISION`.
//!
//! Determinism: for one raw proposal, graph snapshot and revision, [`compile`] produces an
//! identical [`CompiledPlan`] every run — the node/edge order, the recorded
//! `ContractSource`s and the mutation list are functions of the input alone. The
//! graph-backed port then produces the same revision and the same event sequence (types,
//! aggregate ids of the plan events and order) on every run.
//!
//! Scope limit inherited from the canonical graph seam: the graph store assigns a new
//! node's canonical id inside the transaction and offers no alias for it, so a compiled
//! plan may reference existing graph nodes but not a node created in the same transaction.
//! Intra-plan references are still validated (existence, acyclicity, inheritance) and then
//! rejected with `VALIDATION_SCHEMA` rather than emitted as a mutation the store cannot
//! apply; extending the frozen CORE-005 seam would be required to accept them.

mod compile;
mod policy;
mod port;
mod service;
mod wire;

pub use compile::{
    compile, CompiledEdge, CompiledEdgeRemoval, CompiledNode, CompiledNodeUpdate, CompiledPlan,
    ContractSource, PlanGraphSnapshot, PlanProposerProjection, SnapshotEdge, SnapshotNode,
    DEFAULT_MAX_PLAN_NODES,
};
pub use policy::max_plan_nodes;
pub use port::{PlanCommitOutcome, PlanWorkspacePort, UnavailablePlanWorkspace, PLAN_GRAPH_OWNER};
pub use service::{PlanOutcome, Planner};
pub use wire::{parse_raw, EdgeKind, NodeKind, NodeStatus, RawPlanProposal};

/// A PlanProposal that failed validation (DOMAIN.md §4.5, §15).
///
/// Every variant maps to a canonical DOMAIN.md §15 error code through [`PlanError::code`]
/// and is recorded verbatim in the `work.plan_rejected` event's typed `reason`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// The proposal is not a canonical `PlanProposal` shape, names an unknown kind or
    /// status, duplicates an identity, references an unknown endpoint, or carries no
    /// change (`VALIDATION_SCHEMA`).
    #[error("plan schema: {detail}")]
    Schema {
        /// What the proposal got wrong.
        detail: String,
    },
    /// The proposal touches more nodes than the effective policy bound
    /// (`VALIDATION_BOUNDS`).
    #[error("plan touches {requested} nodes, above the limit {limit}")]
    Bounds {
        /// Nodes the proposal adds or updates.
        requested: usize,
        /// Effective `policies.max_plan_nodes` bound.
        limit: usize,
    },
    /// Applying the proposal would close a `depends_on`/`parent_of` cycle
    /// (`VALIDATION_SCHEMA`).
    #[error("the plan would close a cycle through {from} -> {to}")]
    Cycle {
        /// The edge endpoint whose relation closes the cycle.
        from: String,
        /// The other endpoint.
        to: String,
    },
    /// An added node has neither its own CompletionContract nor a parent that provides
    /// one (`VALIDATION_SCHEMA`).
    #[error("node {node:?} has no CompletionContract and no parent that provides one")]
    MissingCompletionContract {
        /// The offending node (its proposal-local id or title).
        node: String,
    },
    /// A `capability_needs` template is not a narrowing of the proposer's projection
    /// (`CAPABILITY_DENIED`).
    #[error("capability need {need} is not a narrowing of the proposer's projection")]
    CapabilityNotNarrowed {
        /// The rejected grant template.
        need: String,
    },
    /// `base_revision` is not the current graph revision (`CONFLICT_REVISION`).
    #[error("base revision {expected} is not the current graph revision {current}")]
    StaleBaseRevision {
        /// The revision the proposal carried.
        expected: u64,
        /// The current graph revision.
        current: u64,
    },
    /// The proposal carries no change at all (`VALIDATION_SCHEMA`).
    #[error("the plan proposal carries no change")]
    EmptyPlan,
    /// The graph snapshot, policy bound or graph mutation path failed (`INTERNAL`).
    #[error("plan workspace error: {detail}")]
    Workspace {
        /// The underlying failure.
        detail: String,
    },
}

impl PlanError {
    /// The DOMAIN.md §15 error code this rejection maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Bounds { .. } => "VALIDATION_BOUNDS",
            Self::CapabilityNotNarrowed { .. } => "CAPABILITY_DENIED",
            Self::StaleBaseRevision { .. } => "CONFLICT_REVISION",
            Self::Schema { .. }
            | Self::Cycle { .. }
            | Self::MissingCompletionContract { .. }
            | Self::EmptyPlan => "VALIDATION_SCHEMA",
            Self::Workspace { .. } => "INTERNAL",
        }
    }

    /// A `VALIDATION_SCHEMA` rejection with a diagnostic.
    #[must_use]
    pub fn schema(detail: impl Into<String>) -> Self {
        Self::Schema {
            detail: detail.into(),
        }
    }

    /// Whether the rejection is a plan verdict (and therefore recorded as
    /// `work.plan_rejected`) rather than an infrastructure failure.
    #[must_use]
    pub const fn is_rejection(&self) -> bool {
        !matches!(self, Self::Workspace { .. })
    }
}
