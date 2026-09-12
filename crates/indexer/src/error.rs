//! Typed errors for the derived exact/lexical/symbol index.
//!
//! Fail-closed paths are distinct variants so a caller can branch without parsing a
//! message: a channel this index does not own, an over-bound request, a stale
//! snapshot, or a cross-tenant write attempt.

use thiserror::Error;

/// Result alias for index operations.
pub type IndexResult<T> = Result<T, IndexError>;

/// Typed index error (DOMAIN.md §11.3, task INT-004).
#[derive(Debug, Error)]
pub enum IndexError {
    /// The requested channel is not owned by this index.
    ///
    /// Only exact, lexical and symbol channels are materialized here. Semantic,
    /// graph, history and memory retrieval live in their own derived indexes and
    /// must be requested there; this index never silently drops a channel.
    #[error("search channel '{channel}' is not available in this index ({owner})")]
    ChannelNotAvailable {
        /// The rejected channel name.
        channel: String,
        /// The canonical owner of that channel.
        owner: &'static str,
    },

    /// The request exceeds a hard structural bound.
    #[error("search budget '{field}' = {requested} exceeds the allowed bound {limit}")]
    BoundsExceeded {
        /// The budget field (`max_results` or `max_tokens`).
        field: &'static str,
        /// The value the caller requested.
        requested: u64,
        /// The maximum this index accepts.
        limit: u64,
    },

    /// The requested snapshot is not the tenant's current index epoch.
    #[error("requested snapshot '{requested}' is stale; the current snapshot is '{current}'")]
    StaleSnapshot {
        /// Snapshot the caller asked for.
        requested: String,
        /// Snapshot the tenant index currently serves.
        current: String,
    },

    /// A program requested no channels.
    #[error("a search program must request at least one channel")]
    NoChannels,

    /// A program carried no query term.
    #[error("a search program must carry a non-empty query term")]
    EmptyQuery,

    /// A workspace scope was requested without naming a workspace.
    #[error("a workspace-scoped search program must name a workspace")]
    WorkspaceRequired,

    /// A document belongs to a different tenant than the target index.
    #[error("document for tenant '{document_tenant}' cannot be written into tenant index '{index_tenant}'")]
    CrossTenantDocument {
        /// Tenant that owns the target index.
        index_tenant: String,
        /// Tenant that owns the document.
        document_tenant: String,
    },

    /// The tenant identifier cannot be used as an index scope.
    #[error("invalid tenant id '{tenant_id}'")]
    InvalidTenant {
        /// The rejected identifier.
        tenant_id: String,
    },

    /// The request named neither tenant nor workspace scope.
    #[error("a search program requires a tenant scope")]
    TenantRequired,

    /// The embedded engine reported a failure.
    #[error("index engine failure: {0}")]
    Engine(String),

    /// The index files could not be read or written.
    #[error("index storage failure: {0}")]
    Io(String),

    /// Index metadata could not be encoded or decoded.
    #[error("index metadata encoding failure: {0}")]
    Encoding(String),

    /// An authoritative source could not be read.
    #[error("authoritative source failure: {0}")]
    Source(String),
}

impl From<std::io::Error> for IndexError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl From<serde_json::Error> for IndexError {
    fn from(error: serde_json::Error) -> Self {
        Self::Encoding(error.to_string())
    }
}
