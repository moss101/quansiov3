//! WorkGraph, AgentGraph and StateGraph stores plus GraphTransaction.
//!
//! Canonical owner (DOSSIER.md §17): `crates/graph`.
//!
//! This crate is the only place work definitions, agent delegation lineage and observed
//! runtime state are persisted (DOMAIN.md §4–§5). It owns no runtime scheduling, no
//! capability algebra and no event store: those belong to `quansio-runtime`, RUN-005 and
//! CORE-003 respectively. [`GraphStore::apply_batch`] is the atomic, revision-checked
//! mutation seam that CORE-005's GraphTransaction wraps to add event emission.
//!
//! Every method is tenant-scoped: it opens a transaction, sets the tenant context via
//! `quansio_server::control::schema::set_tenant_context`, and additionally constrains
//! every statement by `tenant_id`.
#![forbid(unsafe_code)]

/// Repository path of this crate's canonical owner, used by the workspace
/// conformance check to prove one owner maps to exactly one package.
pub const CANONICAL_OWNER: &str = "crates/graph";

pub mod agent;
pub mod batch;
pub mod error;
pub mod runtime;
pub mod state;
pub mod store;
pub mod work;

pub use agent::{
    AgentThread, Delegated, DelegationEdge, DelegationNarrowingCheck, DelegationRequest,
    NewAgentThread, StructuralDelegationCheck,
};
pub use batch::{BatchOutcome, GraphBatch, GraphChange};
pub use error::{Entity, GraphError};
pub use runtime::{Attempt, NewRun, NewStep, NewTurn, Run, Step, Turn};
pub use state::{
    AgentKind, AgentThreadStatus, AttemptStatus, RunStatus, RunTriggerKind, StepKind, StepStatus,
    TurnStatus, WorkEdgeKind, WorkNodeKind, WorkNodeStatus, WorkOrigin,
};
pub use store::GraphStore;
pub use work::{NewWorkEdge, NewWorkNode, WorkEdge, WorkNode};
