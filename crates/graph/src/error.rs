//! Typed errors for the canonical graph stores.
//!
//! Every fail-closed path in `crates/graph` returns a [`GraphError`] and leaves the
//! database unchanged: the store only commits after the last check has passed. Where
//! DOMAIN.md §15 defines an error code, [`GraphError::code`] returns it, so the API layer
//! never has to reinterpret a store error.

use quansio_server::control::schema::SchemaError;

/// Canonical entity a graph error refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Entity {
    /// The workspace WorkGraph revision head.
    GraphHead,
    /// A `work_nodes` row.
    WorkNode,
    /// A `work_edges` row.
    WorkEdge,
    /// An `agent_threads` row.
    AgentThread,
    /// An `agent_graph_edges` row.
    Delegation,
    /// A `runs` row.
    Run,
    /// A `turns` row.
    Turn,
    /// A `steps` row.
    Step,
    /// An `attempts` row.
    Attempt,
    /// A workspace row.
    Workspace,
}

impl Entity {
    /// The entity name used in messages.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GraphHead => "graph_head",
            Self::WorkNode => "work_node",
            Self::WorkEdge => "work_edge",
            Self::AgentThread => "agent_thread",
            Self::Delegation => "delegation",
            Self::Run => "run",
            Self::Turn => "turn",
            Self::Step => "step",
            Self::Attempt => "attempt",
            Self::Workspace => "workspace",
        }
    }
}

impl std::fmt::Display for Entity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errors produced by WorkGraph, AgentGraph and StateGraph stores.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// The database rejected a statement.
    #[error("graph database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The tenant context could not be established; callers must fail closed.
    #[error("tenant context failed: {0}")]
    TenantScope(#[from] SchemaError),
    /// A canonical id does not carry the prefix the store requires.
    #[error("invalid canonical id {value:?}: expected prefix {expected:?}")]
    InvalidId {
        /// The rejected value.
        value: String,
        /// The prefix the store requires.
        expected: &'static str,
    },
    /// A persisted state string is not in the canonical state table.
    #[error("unknown {entity} state {value:?}")]
    UnknownState {
        /// The entity whose state could not be parsed.
        entity: &'static str,
        /// The unrecognised value.
        value: String,
    },
    /// A required row is not visible in this tenant.
    #[error("{entity} {id} not found in tenant {tenant_id}")]
    NotFound {
        /// The entity type that was looked up.
        entity: &'static str,
        /// The identifier that was looked up.
        id: String,
        /// The active tenant.
        tenant_id: String,
    },
    /// A mutation was issued against a stale aggregate revision.
    #[error("stale revision for {entity} {id}: expected {expected}, current {current}")]
    RevisionConflict {
        /// The entity whose revision did not match.
        entity: &'static str,
        /// The identifier whose revision did not match.
        id: String,
        /// The revision carried by the mutation.
        expected: u64,
        /// The current authoritative revision.
        current: u64,
    },
    /// A state transition is not in the canonical state machine.
    #[error("illegal {entity} transition {from} -> {to}")]
    IllegalTransition {
        /// The entity whose transition was rejected.
        entity: Entity,
        /// The persisted state.
        from: String,
        /// The requested state.
        to: String,
    },
    /// A work node can only reach `done` through [`crate::GraphStore::mark_verification_passed`].
    #[error("work node {id} can only reach done through mark_verification_passed")]
    VerificationRequired {
        /// The node whose transition was rejected.
        id: String,
    },
    /// The row changed between the read and the guarded write (lost-update guard).
    #[error("state changed concurrently for {entity} {id}: expected {expected}")]
    StateConflict {
        /// The entity whose guard did not match.
        entity: &'static str,
        /// The identifier whose guard did not match.
        id: String,
        /// The state the mutation expected.
        expected: String,
    },
    /// Adding an edge would close a cycle in the `depends_on`/`parent_of` structure.
    #[error("cycle rejected: adding {edge_kind} edge {from} -> {to} would close a cycle")]
    Cycle {
        /// The edge family that must stay acyclic.
        edge_kind: &'static str,
        /// The proposed edge source.
        from: String,
        /// The proposed edge target.
        to: String,
    },
    /// A delegation names a parent that is not visible in the same tenant/workspace.
    #[error("delegation rejected: parent {parent_id} not visible in tenant {tenant_id} workspace {workspace_id}")]
    ParentNotFound {
        /// The parent agent thread that was looked up.
        parent_id: String,
        /// The active tenant.
        tenant_id: String,
        /// The workspace the child was proposed for.
        workspace_id: String,
    },
    /// A delegation would link a child into a different workspace than its parent.
    #[error("delegation rejected: child workspace {child_workspace} differs from parent workspace {parent_workspace}")]
    WorkspaceMismatch {
        /// The parent agent thread's workspace.
        parent_workspace: String,
        /// The proposed child's workspace.
        child_workspace: String,
    },
    /// A narrowing check rejected a delegation (RUN-005 hook).
    #[error("capability narrowing rejected: {0}")]
    NarrowingRejected(String),
    /// An argument is structurally invalid.
    #[error("invalid graph argument: {0}")]
    InvalidArgument(String),
}

impl GraphError {
    /// Build an unknown-state error.
    #[must_use]
    pub fn unknown_state(entity: Entity, value: &str) -> Self {
        Self::UnknownState {
            entity: entity.as_str(),
            value: value.to_string(),
        }
    }

    /// The DOMAIN.md §15 error code this error maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Database(_) | Self::TenantScope(_) => "INTERNAL",
            Self::InvalidId { .. } | Self::UnknownState { .. } | Self::InvalidArgument(_) => {
                "VALIDATION_SCHEMA"
            }
            Self::NotFound { .. } => "NOT_FOUND",
            Self::RevisionConflict { .. } => "CONFLICT_REVISION",
            Self::IllegalTransition { entity, .. } => match entity {
                Entity::Run | Entity::Turn | Entity::Step | Entity::Attempt => {
                    "RUNTIME_ILLEGAL_TRANSITION"
                }
                _ => "CONFLICT_STATE",
            },
            Self::VerificationRequired { .. } | Self::StateConflict { .. } | Self::Cycle { .. } => {
                "CONFLICT_STATE"
            }
            Self::ParentNotFound { .. } => "NOT_FOUND",
            Self::WorkspaceMismatch { .. } | Self::NarrowingRejected(_) => "CAPABILITY_DENIED",
        }
    }
}
