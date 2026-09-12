//! Typed `SearchProgram` execution surface for the exact/lexical/symbol index.
//!
//! The request is a typed structure, never a predicate string (DOMAIN.md §11.3,
//! task INT-004): the query is a literal term, filters are typed fields and the
//! budget is two integers. There is no query language, so no SQL/DSL injection
//! surface exists.

use crate::error::{IndexError, IndexResult};
use crate::snapshot::Snapshot;

/// Retrieval channel requested by a program (DOMAIN.md §11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Whole-token exact match over metadata and body tokens.
    Exact,
    /// BM25 lexical match over title and body.
    Lexical,
    /// Exact match over extracted code symbols.
    Symbol,
    /// Semantic retrieval; owned by the embedding pipeline (INT-011).
    Semantic,
    /// Graph retrieval; owned by the graph module.
    Graph,
    /// History retrieval; owned by the runtime history projection.
    History,
    /// Memory retrieval; owned by the Knowledge Fabric memory lifecycle.
    Memory,
}

impl Channel {
    /// Canonical wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Lexical => "lexical",
            Self::Symbol => "symbol",
            Self::Semantic => "semantic",
            Self::Graph => "graph",
            Self::History => "history",
            Self::Memory => "memory",
        }
    }

    /// Whether this crate's index materializes the channel.
    #[must_use]
    pub const fn is_owned_by_indexer(self) -> bool {
        matches!(self, Self::Exact | Self::Lexical | Self::Symbol)
    }

    /// Canonical owner of a channel this index does not serve.
    #[must_use]
    pub const fn external_owner(self) -> &'static str {
        match self {
            Self::Exact | Self::Lexical | Self::Symbol => "crates/indexer",
            Self::Semantic => "python/intelligence/embeddings",
            Self::Graph => "crates/graph",
            Self::History => "crates/server runtime history projection",
            Self::Memory => "Knowledge Fabric memory lifecycle",
        }
    }
}

/// Scope of a search program (DOMAIN.md §11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeKind {
    /// The whole tenant.
    Tenant,
    /// One workspace inside the tenant.
    Workspace,
    /// One execution target inside the tenant.
    Target,
}

/// Typed tenant/workspace/target scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchScope {
    /// Scope granularity.
    pub kind: ScopeKind,
    /// Tenant that owns the index (`tn_…`).
    pub tenant_id: String,
    /// Workspace filter, required for [`ScopeKind::Workspace`].
    pub workspace_id: Option<String>,
    /// Target reference, carried for provenance by the Context plane.
    pub target_ref: Option<String>,
}

impl SearchScope {
    /// A tenant-wide scope.
    #[must_use]
    pub fn tenant(tenant_id: impl Into<String>) -> Self {
        Self {
            kind: ScopeKind::Tenant,
            tenant_id: tenant_id.into(),
            workspace_id: None,
            target_ref: None,
        }
    }

    /// A workspace scope.
    #[must_use]
    pub fn workspace(tenant_id: impl Into<String>, workspace_id: impl Into<String>) -> Self {
        Self {
            kind: ScopeKind::Workspace,
            tenant_id: tenant_id.into(),
            workspace_id: Some(workspace_id.into()),
            target_ref: None,
        }
    }

    fn validate(&self) -> IndexResult<()> {
        if self.tenant_id.trim().is_empty() {
            return Err(IndexError::TenantRequired);
        }
        if self.kind == ScopeKind::Workspace && self.workspace_id.is_none() {
            return Err(IndexError::WorkspaceRequired);
        }
        Ok(())
    }
}

/// Typed result/token budget (DOMAIN.md §11.3 `budget {max_results, max_tokens}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchBudget {
    /// Maximum number of results to return.
    pub max_results: u32,
    /// Maximum estimated tokens across returned snippets.
    pub max_tokens: u64,
}

impl SearchBudget {
    /// Hard structural cap on `max_results`; larger requests fail closed.
    pub const MAX_RESULTS: u32 = 1_000;
    /// Hard structural cap on `max_tokens`; larger requests fail closed.
    pub const MAX_TOKENS: u64 = 1_000_000;

    /// A bounded budget.
    #[must_use]
    pub const fn new(max_results: u32, max_tokens: u64) -> Self {
        Self {
            max_results,
            max_tokens,
        }
    }

    fn validate(&self) -> IndexResult<()> {
        if self.max_results == 0 || self.max_results > Self::MAX_RESULTS {
            return Err(IndexError::BoundsExceeded {
                field: "max_results",
                requested: u64::from(self.max_results),
                limit: u64::from(Self::MAX_RESULTS),
            });
        }
        if self.max_tokens == 0 || self.max_tokens > Self::MAX_TOKENS {
            return Err(IndexError::BoundsExceeded {
                field: "max_tokens",
                requested: self.max_tokens,
                limit: Self::MAX_TOKENS,
            });
        }
        Ok(())
    }
}

impl Default for SearchBudget {
    fn default() -> Self {
        Self {
            max_results: 20,
            max_tokens: 4_096,
        }
    }
}

/// Typed filters. Every field is a concrete identity or enumeration; none is a
/// string evaluated as a predicate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFilters {
    /// Restrict to these media types (`text/markdown`, `application/json`, …).
    pub media_types: Vec<String>,
    /// Restrict to these artifact kinds (`document`, `code`, …).
    pub artifact_kinds: Vec<String>,
    /// Restrict to these source (artifact) identities.
    pub source_ids: Vec<String>,
}

impl SearchFilters {
    /// An empty filter set.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Restrict to one media type.
    #[must_use]
    pub fn media_type(mut self, media_type: impl Into<String>) -> Self {
        self.media_types.push(media_type.into());
        self
    }

    /// Restrict to one artifact kind.
    #[must_use]
    pub fn artifact_kind(mut self, kind: impl Into<String>) -> Self {
        self.artifact_kinds.push(kind.into());
        self
    }

    /// Restrict to one source identity.
    #[must_use]
    pub fn source_id(mut self, source_id: impl Into<String>) -> Self {
        self.source_ids.push(source_id.into());
        self
    }
}

/// A typed, bounded retrieval request (DOMAIN.md §11.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchProgram {
    /// Literal query term; tokenized by the index, never parsed as a language.
    pub query: String,
    /// Channels to fan out to; every requested unowned channel is rejected.
    pub channels: Vec<Channel>,
    /// Typed filters.
    pub filters: SearchFilters,
    /// Result/token budget.
    pub budget: SearchBudget,
    /// Index epoch to pin to; `None` selects the tenant's current snapshot.
    pub snapshot: Option<Snapshot>,
    /// Tenant/workspace scope.
    pub scope: SearchScope,
}

impl SearchProgram {
    /// A program over the three channels this index owns.
    #[must_use]
    pub fn new(scope: SearchScope, query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            channels: vec![Channel::Exact, Channel::Lexical, Channel::Symbol],
            filters: SearchFilters::none(),
            budget: SearchBudget::default(),
            snapshot: None,
            scope,
        }
    }

    /// Replace the requested channels.
    #[must_use]
    pub fn with_channels(mut self, channels: Vec<Channel>) -> Self {
        self.channels = channels;
        self
    }

    /// Replace the typed filters.
    #[must_use]
    pub fn with_filters(mut self, filters: SearchFilters) -> Self {
        self.filters = filters;
        self
    }

    /// Replace the budget.
    #[must_use]
    pub fn with_budget(mut self, budget: SearchBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Pin the request to an explicit snapshot/epoch.
    #[must_use]
    pub fn with_snapshot(mut self, snapshot: Option<Snapshot>) -> Self {
        self.snapshot = snapshot;
        self
    }

    /// Validate structure, channels and bounds before touching any index.
    ///
    /// # Errors
    /// Returns a typed [`IndexError`] for an empty query, no channels, an unowned
    /// channel, an out-of-bounds budget or an invalid scope.
    pub fn validate(&self) -> IndexResult<()> {
        if self.query.trim().is_empty() {
            return Err(IndexError::EmptyQuery);
        }
        if self.channels.is_empty() {
            return Err(IndexError::NoChannels);
        }
        for channel in &self.channels {
            if !channel.is_owned_by_indexer() {
                return Err(IndexError::ChannelNotAvailable {
                    channel: channel.as_str().to_string(),
                    owner: channel.external_owner(),
                });
            }
        }
        self.budget.validate()?;
        self.scope.validate()
    }
}

/// Deterministic token estimate for budget accounting.
///
/// The index does not own a model tokenizer; it uses a fixed, documented estimate
/// (whitespace words, minimum one) so a budget is enforced identically on every run.
#[must_use]
pub fn estimate_tokens(text: &str) -> u64 {
    let words = text.split_whitespace().count();
    u64::try_from(words.max(1)).unwrap_or(u64::MAX)
}

/// One ranked search hit with provenance `(source_id, locator, snapshot)`.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    /// Artifact identity.
    pub source_id: String,
    /// Locator within the source (artifact version).
    pub locator: String,
    /// Snapshot the hit was served from.
    pub snapshot: String,
    /// Ranked score (higher is better).
    pub score: f64,
    /// Human title.
    pub title: String,
    /// Bounded matching snippet.
    pub snippet: String,
    /// Media type of the hit.
    pub media_type: String,
    /// Digest of the hit's source bytes.
    pub content_digest: String,
}

/// How a result set was truncated against the budget.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Truncation {
    /// `max_results` cut the ranked list.
    pub results_truncated: bool,
    /// `max_tokens` cut the ranked list.
    pub tokens_truncated: bool,
    /// Results matched but not returned.
    pub dropped_results: u64,
}

impl Truncation {
    /// Whether anything was dropped.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.results_truncated || self.tokens_truncated
    }
}

/// Full result set returned by [`crate::SearchIndex::search`].
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResults {
    /// Ranked, budgeted hits.
    pub results: Vec<SearchResult>,
    /// Snapshot all hits were served from.
    pub snapshot: Snapshot,
    /// Estimated tokens used by the returned snippets.
    pub tokens_used: u64,
    /// Number of ranked hits the index found before budgeting.
    pub total_matches: u64,
    /// Explicit, non-silent truncation report.
    pub truncation: Truncation,
}
