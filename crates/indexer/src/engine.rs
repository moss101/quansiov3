//! Replaceable embedded search-engine contract.
//!
//! The lexical/exact/symbol index is built on Tantivy (`tantivy_engine`), but all
//! index mutation and querying happens through [`SearchEngine`]. Replacing the
//! engine (for example with another embedded Rust engine) means providing one
//! implementation of this trait; the `SearchIndex` surface does not change.

use crate::document::SourceDocument;
use crate::error::IndexResult;
use crate::program::{Channel, SearchFilters};

/// A normalized, engine-level query. Tokens are already split by the caller so
/// the engine never parses a query language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineQuery {
    /// Lowercased alphanumeric tokens for the lexical channel (AND semantics).
    pub lexical_tokens: Vec<String>,
    /// Lowercased whole tokens for the exact channel (AND semantics).
    pub exact_tokens: Vec<String>,
    /// Lowercased identifier tokens for the symbol channel (AND semantics).
    pub symbol_tokens: Vec<String>,
    /// Channels to fan out to; only engine-owned channels reach here.
    pub channels: Vec<Channel>,
    /// Typed filters.
    pub filters: SearchFilters,
    /// Workspace restriction from the scope, when workspace-scoped.
    pub workspace_id: Option<String>,
    /// Maximum hits to materialize.
    pub limit: usize,
}

/// One engine hit before ranking by the index facade.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineHit {
    /// Artifact identity.
    pub source_id: String,
    /// Artifact version identity (the engine's primary key).
    pub version_id: String,
    /// Within-source locator.
    pub locator: String,
    /// Human title.
    pub title: String,
    /// Bounded snippet.
    pub snippet: String,
    /// Media type.
    pub media_type: String,
    /// Stored bytes digest.
    pub content_digest: String,
    /// Score (higher is better, deterministic for a fixed corpus).
    pub score: f64,
}

/// Hits plus the total number of matches before the limit.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineOutcome {
    /// Ranked hits.
    pub hits: Vec<EngineHit>,
    /// Total matches in the tenant index.
    pub total_matches: u64,
}

/// Embedded index engine contract.
///
/// Implementations are per-tenant: construction binds one tenant directory and no
/// operation may read or write another tenant's documents.
pub trait SearchEngine: Send {
    /// Replace the whole corpus with `documents`, in a deterministic order.
    ///
    /// # Errors
    /// Returns a typed [`crate::IndexError`] on engine or storage failure.
    fn rebuild(&mut self, documents: &[SourceDocument]) -> IndexResult<()>;

    /// Insert or replace one document.
    ///
    /// Returns `false` when the same `(version_id, content_digest)` is already
    /// indexed, so re-applying an identical change is a no-op.
    ///
    /// # Errors
    /// Returns a typed [`crate::IndexError`] on engine or storage failure.
    fn upsert(&mut self, document: &SourceDocument) -> IndexResult<bool>;

    /// Delete one version by identity.
    ///
    /// Returns `false` when the version was not present (idempotent delete).
    ///
    /// # Errors
    /// Returns a typed [`crate::IndexError`] on engine or storage failure.
    fn delete(&mut self, version_id: &str) -> IndexResult<bool>;

    /// Search the tenant index.
    ///
    /// # Errors
    /// Returns a typed [`crate::IndexError`] on engine failure.
    fn search(&self, query: &EngineQuery) -> IndexResult<EngineOutcome>;

    /// Digest currently indexed for a version, when present.
    ///
    /// # Errors
    /// Returns a typed [`crate::IndexError`] on engine failure.
    fn digest_of(&self, version_id: &str) -> IndexResult<Option<String>>;

    /// Number of indexed documents.
    ///
    /// # Errors
    /// Returns a typed [`crate::IndexError`] on engine failure.
    fn document_count(&self) -> IndexResult<u64>;
}
