//! Authoritative-source read port for rebuilds.
//!
//! The index is derived: `rebuild_from` asks an [`AuthoritativeSource`] for the
//! current projections of authoritative rows. The concrete implementation for
//! artifact versions is [`crate::postgres::PostgresArtifactSource`]; tests can
//! supply any implementation because the port is a trait.

use async_trait::async_trait;

use crate::document::SourceDocument;
use crate::error::IndexResult;

/// Scope a rebuild reads from: a tenant, optionally narrowed to one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceScope {
    /// Tenant whose rows are read.
    pub tenant_id: String,
    /// Workspace restriction, when rebuilding a single workspace.
    pub workspace_id: Option<String>,
}

impl SourceScope {
    /// A tenant-wide scope.
    #[must_use]
    pub fn tenant(tenant_id: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            workspace_id: None,
        }
    }

    /// A workspace-narrowed scope.
    #[must_use]
    pub fn workspace(tenant_id: impl Into<String>, workspace_id: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            workspace_id: Some(workspace_id.into()),
        }
    }
}

/// Port for reading authoritative rows as index projections.
#[async_trait]
pub trait AuthoritativeSource: Send + Sync {
    /// Load the current projections visible to `scope`.
    ///
    /// # Errors
    /// Returns [`crate::IndexError::Source`] when the authoritative rows cannot be
    /// read.
    async fn load(&self, scope: &SourceScope) -> IndexResult<Vec<SourceDocument>>;
}

/// Port for reading the stored text of an artifact version's bytes.
///
/// Production wires this to the artifact object store; the index owns extraction
/// only in the sense of consuming already-stored bytes, never of becoming an
/// artifact store.
#[async_trait]
pub trait ObjectTextProvider: Send + Sync {
    /// Return the text projection for an object key, or `None` for binary bytes.
    ///
    /// # Errors
    /// Returns [`crate::IndexError::Source`] when the object cannot be read.
    async fn text_for(&self, object_key: &str, media_type: &str) -> IndexResult<Option<String>>;
}
