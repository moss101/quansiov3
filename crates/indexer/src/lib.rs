//! quansio-indexer: exact, lexical and symbol indexes over authoritative sources.
//!
//! Canonical owner (DOSSIER.md §17): `crates/indexer`. The crate owns the derived
//! [`SearchIndex`] described in DOSSIER.md §9 and DOMAIN.md §11.3: Tantivy files
//! scoped per tenant, exact/lexical/symbol channels, typed [`SearchProgram`]
//! execution with provenance, and an explicit rebuild path over an
//! [`AuthoritativeSource`]. It is never a source of truth: every field is a
//! projection of artifact-version rows or the text stored for them, and the whole
//! index can be discarded and rebuilt.
#![forbid(unsafe_code)]

/// Repository path of this crate's canonical owner, used by the workspace
/// conformance check to prove one owner maps to exactly one package.
pub const CANONICAL_OWNER: &str = "crates/indexer";

pub mod document;
pub mod engine;
pub mod error;
pub mod index;
pub mod postgres;
pub mod program;
pub mod snapshot;
pub mod source;
pub mod tantivy_engine;

pub use document::{
    corpus_fingerprint, extract_symbols, tokenize_alnum, tokenize_whitespace, SourceDocument,
    SourceKind,
};
pub use engine::{EngineHit, EngineOutcome, EngineQuery, SearchEngine};
pub use error::{IndexError, IndexResult};
pub use index::{ApplyOutcome, RebuildReport, SearchIndex, SourceChange, INDEX_KIND};
pub use postgres::PostgresArtifactSource;
pub use program::{
    estimate_tokens, Channel, ScopeKind, SearchBudget, SearchFilters, SearchProgram, SearchResult,
    SearchResults, SearchScope, Truncation,
};
pub use snapshot::Snapshot;
pub use source::{AuthoritativeSource, ObjectTextProvider, SourceScope};
pub use tantivy_engine::TantivyEngine;
