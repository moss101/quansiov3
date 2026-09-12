//! `SearchIndex`: the derived exact/lexical/symbol index facade.
//!
//! The facade owns the file layout (`<root>/tenants/<tenant>/…`), the per-tenant
//! epoch manifest used for staleness detection, and budget enforcement. It is
//! derived and rebuildable: nothing here is authoritative, and `rebuild` always
//! reconstructs from rows supplied by an [`AuthoritativeSource`].

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::document::{corpus_fingerprint, tokenize_alnum, tokenize_whitespace, SourceDocument};
use crate::engine::{EngineOutcome, EngineQuery, SearchEngine};
use crate::error::{IndexError, IndexResult};
use crate::program::{estimate_tokens, SearchProgram, SearchResult, SearchResults, Truncation};
use crate::snapshot::Snapshot;
use crate::source::{AuthoritativeSource, SourceScope};
use crate::tantivy_engine::TantivyEngine;

/// Canonical index kind recorded in each tenant manifest.
pub const INDEX_KIND: &str = "exact_lexical_symbol";

const MANIFEST_SCHEMA_VERSION: u32 = 1;
const EMPTY_FINGERPRINT: &str = "empty";

/// Durable per-tenant index metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TenantManifest {
    schema_version: u32,
    tenant_id: String,
    index_kind: String,
    epoch: u64,
    fingerprint: String,
    document_count: u64,
    updated_at: String,
}

/// What a rebuild produced.
#[derive(Debug, Clone, PartialEq)]
pub struct RebuildReport {
    /// Tenant that was rebuilt.
    pub tenant_id: String,
    /// Snapshot now served for the tenant.
    pub snapshot: Snapshot,
    /// Number of indexed documents.
    pub document_count: u64,
}

/// One incremental change applied to a tenant index.
#[derive(Debug, Clone)]
pub enum SourceChange {
    /// Insert or replace a projected document.
    Upsert(Box<SourceDocument>),
    /// Delete one artifact version.
    Delete {
        /// Tenant owning the version.
        tenant_id: String,
        /// Artifact version identity.
        version_id: String,
    },
}

impl SourceChange {
    /// Build an upsert change.
    #[must_use]
    pub fn upsert(document: SourceDocument) -> Self {
        Self::Upsert(Box::new(document))
    }

    /// Build a delete change.
    #[must_use]
    pub fn delete(tenant_id: impl Into<String>, version_id: impl Into<String>) -> Self {
        Self::Delete {
            tenant_id: tenant_id.into(),
            version_id: version_id.into(),
        }
    }
}

/// Result of applying one incremental change.
#[derive(Debug, Clone, PartialEq)]
pub struct ApplyOutcome {
    /// Tenant the change targeted.
    pub tenant_id: String,
    /// `false` when the change was already reflected (idempotent re-application).
    pub applied: bool,
    /// Snapshot after the change (unchanged when `applied` is `false`).
    pub snapshot: Snapshot,
}

/// The derived exact/lexical/symbol index over one or more tenants.
pub struct SearchIndex {
    root: PathBuf,
    engines: Mutex<HashMap<String, Box<dyn SearchEngine>>>,
}

impl SearchIndex {
    /// Open (creating if needed) an index rooted at `root`.
    ///
    /// # Errors
    /// Returns [`IndexError::Io`] when the root layout cannot be created.
    pub fn open(root: impl AsRef<Path>) -> IndexResult<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("tenants"))?;
        Ok(Self {
            root,
            engines: Mutex::new(HashMap::new()),
        })
    }

    /// Root directory of the index files.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Rebuild one tenant from scratch over `documents`.
    ///
    /// Rebuilding an identical corpus reuses the previous epoch, so the snapshot is
    /// stable; a changed corpus advances the epoch. Reconstruction happens through a
    /// single Tantivy commit, so a crash before commit leaves the previous index
    /// visible rather than a half-written one.
    ///
    /// # Errors
    /// Returns [`IndexError`] when the tenant is invalid, a document belongs to
    /// another tenant, or the index files cannot be written.
    pub fn rebuild(
        &self,
        tenant_id: &str,
        documents: &[SourceDocument],
    ) -> IndexResult<RebuildReport> {
        let name = tenant_dir_name(tenant_id)?;
        for document in documents {
            self.check_document(tenant_id, document)?;
        }
        let directory = self.tenant_dir(&name);
        fs::create_dir_all(directory.join("tantivy"))?;
        let previous = read_manifest(&directory.join("manifest.json"))?;
        let fingerprint = corpus_fingerprint(documents);
        let epoch = match &previous {
            Some(manifest) if manifest.fingerprint == fingerprint => manifest.epoch,
            Some(manifest) => manifest.epoch.saturating_add(1),
            None => 1,
        };
        self.engine_mut(tenant_id, true, |engine| {
            engine.expect("engine created on demand").rebuild(documents)
        })?;
        let manifest = TenantManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            tenant_id: tenant_id.to_string(),
            index_kind: INDEX_KIND.to_string(),
            epoch,
            fingerprint: fingerprint.clone(),
            document_count: documents.len() as u64,
            updated_at: Utc::now().to_rfc3339(),
        };
        write_manifest(&directory.join("manifest.json"), &manifest)?;
        Ok(RebuildReport {
            tenant_id: tenant_id.to_string(),
            snapshot: Snapshot::new(epoch, fingerprint),
            document_count: documents.len() as u64,
        })
    }

    /// Rebuild one tenant from an authoritative source.
    ///
    /// # Errors
    /// Returns [`IndexError`] when the source cannot be read or the rebuild fails.
    pub async fn rebuild_from(
        &self,
        source: &dyn AuthoritativeSource,
        scope: &SourceScope,
    ) -> IndexResult<RebuildReport> {
        let documents = source.load(scope).await?;
        self.rebuild(&scope.tenant_id, &documents)
    }

    /// Apply one incremental change.
    ///
    /// Re-applying an identical upsert or deleting an absent version is a no-op and
    /// leaves the snapshot unchanged, so repeated delivery is safe.
    ///
    /// # Errors
    /// Returns [`IndexError`] when the tenant is invalid, the document is
    /// cross-tenant, or the index files cannot be written.
    pub fn apply(&self, change: SourceChange) -> IndexResult<ApplyOutcome> {
        match change {
            SourceChange::Upsert(document) => self.apply_upsert(*document),
            SourceChange::Delete {
                tenant_id,
                version_id,
            } => self.apply_delete(&tenant_id, &version_id),
        }
    }

    fn apply_upsert(&self, document: SourceDocument) -> IndexResult<ApplyOutcome> {
        let tenant_id = document.tenant_id.clone();
        self.check_document(&tenant_id, &document)?;
        let applied = self.engine_mut(&tenant_id, true, |engine| {
            engine.expect("engine created on demand").upsert(&document)
        })?;
        if !applied {
            let snapshot = self.current_snapshot(&tenant_id)?;
            return Ok(ApplyOutcome {
                tenant_id,
                applied: false,
                snapshot,
            });
        }
        let count = self.document_count(&tenant_id)?;
        let snapshot = self.advance_manifest(&tenant_id, &document.canonical(), count)?;
        Ok(ApplyOutcome {
            tenant_id,
            applied: true,
            snapshot,
        })
    }

    fn apply_delete(&self, tenant_id: &str, version_id: &str) -> IndexResult<ApplyOutcome> {
        tenant_dir_name(tenant_id)?;
        let applied = self.engine_mut(tenant_id, true, |engine| {
            engine.expect("engine created on demand").delete(version_id)
        })?;
        if !applied {
            return Ok(ApplyOutcome {
                tenant_id: tenant_id.to_string(),
                applied: false,
                snapshot: self.current_snapshot(tenant_id)?,
            });
        }
        let count = self.document_count(tenant_id)?;
        let snapshot = self.advance_manifest(tenant_id, version_id, count)?;
        Ok(ApplyOutcome {
            tenant_id: tenant_id.to_string(),
            applied: true,
            snapshot,
        })
    }

    /// Execute a typed search program against one tenant index.
    ///
    /// # Errors
    /// Returns a typed [`IndexError`] for validation failures (unowned channel,
    /// out-of-bounds budget, empty query), a stale requested snapshot, or an engine
    /// failure.
    pub fn search(&self, program: &SearchProgram) -> IndexResult<SearchResults> {
        program.validate()?;
        let tenant_id = &program.scope.tenant_id;
        let current = self.current_snapshot(tenant_id)?;
        if let Some(requested) = &program.snapshot {
            if requested != &current {
                return Err(IndexError::StaleSnapshot {
                    requested: requested.as_string(),
                    current: current.as_string(),
                });
            }
        }
        let query = EngineQuery {
            lexical_tokens: tokenize_alnum(&program.query),
            exact_tokens: tokenize_whitespace(&program.query),
            symbol_tokens: tokenize_whitespace(&program.query),
            channels: program.channels.clone(),
            filters: program.filters.clone(),
            workspace_id: program.scope.workspace_id.clone(),
            limit: program.budget.max_results as usize,
        };
        let outcome = self.engine_mut(tenant_id, false, |engine| match engine {
            Some(engine) => engine.search(&query),
            None => Ok(EngineOutcome {
                hits: Vec::new(),
                total_matches: 0,
            }),
        })?;
        Ok(self.pack(outcome, program, current))
    }

    fn pack(
        &self,
        outcome: EngineOutcome,
        program: &SearchProgram,
        snapshot: Snapshot,
    ) -> SearchResults {
        let mut results: Vec<SearchResult> = Vec::new();
        let mut tokens_used: u64 = 0;
        let mut tokens_truncated = false;
        for hit in outcome.hits {
            let cost = estimate_tokens(&hit.title) + estimate_tokens(&hit.snippet);
            if tokens_used.saturating_add(cost) > program.budget.max_tokens {
                tokens_truncated = true;
                break;
            }
            tokens_used += cost;
            results.push(SearchResult {
                source_id: hit.source_id,
                locator: hit.locator,
                snapshot: snapshot.as_string(),
                score: hit.score,
                title: hit.title,
                snippet: hit.snippet,
                media_type: hit.media_type,
                content_digest: hit.content_digest,
            });
        }
        let returned = results.len() as u64;
        let truncation = Truncation {
            results_truncated: outcome.total_matches > u64::from(program.budget.max_results),
            tokens_truncated,
            dropped_results: outcome.total_matches.saturating_sub(returned),
        };
        SearchResults {
            results,
            snapshot,
            tokens_used,
            total_matches: outcome.total_matches,
            truncation,
        }
    }

    /// Current snapshot for a tenant, or `None` when the tenant has never indexed.
    ///
    /// # Errors
    /// Returns [`IndexError`] when the tenant is invalid or its manifest is corrupt.
    pub fn snapshot(&self, tenant_id: &str) -> IndexResult<Option<Snapshot>> {
        let name = tenant_dir_name(tenant_id)?;
        Ok(
            read_manifest(&self.tenant_dir(&name).join("manifest.json"))?
                .map(|manifest| Snapshot::new(manifest.epoch, manifest.fingerprint)),
        )
    }

    fn current_snapshot(&self, tenant_id: &str) -> IndexResult<Snapshot> {
        Ok(self
            .snapshot(tenant_id)?
            .unwrap_or_else(|| Snapshot::new(0, EMPTY_FINGERPRINT)))
    }

    /// Number of indexed documents for a tenant.
    ///
    /// # Errors
    /// Returns [`IndexError`] on engine failure.
    pub fn document_count(&self, tenant_id: &str) -> IndexResult<u64> {
        self.engine_mut(tenant_id, false, |engine| match engine {
            Some(engine) => engine.document_count(),
            None => Ok(0),
        })
    }

    fn advance_manifest(
        &self,
        tenant_id: &str,
        change: &str,
        document_count: u64,
    ) -> IndexResult<Snapshot> {
        let name = tenant_dir_name(tenant_id)?;
        let directory = self.tenant_dir(&name);
        fs::create_dir_all(&directory)?;
        let previous = read_manifest(&directory.join("manifest.json"))?;
        let (epoch, previous_fingerprint) = match &previous {
            Some(manifest) => (
                manifest.epoch.saturating_add(1),
                manifest.fingerprint.clone(),
            ),
            None => (1, EMPTY_FINGERPRINT.to_string()),
        };
        let fingerprint =
            quansio_core::Digest::of(format!("{previous_fingerprint}\u{1e}{change}").as_bytes())
                .to_string();
        let manifest = TenantManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            tenant_id: tenant_id.to_string(),
            index_kind: INDEX_KIND.to_string(),
            epoch,
            fingerprint: fingerprint.clone(),
            document_count,
            updated_at: Utc::now().to_rfc3339(),
        };
        write_manifest(&directory.join("manifest.json"), &manifest)?;
        Ok(Snapshot::new(epoch, fingerprint))
    }

    fn check_document(&self, tenant_id: &str, document: &SourceDocument) -> IndexResult<()> {
        if document.tenant_id != tenant_id {
            return Err(IndexError::CrossTenantDocument {
                index_tenant: tenant_id.to_string(),
                document_tenant: document.tenant_id.clone(),
            });
        }
        Ok(())
    }

    fn tenant_dir(&self, name: &str) -> PathBuf {
        self.root.join("tenants").join(name)
    }

    fn engine_mut<R>(
        &self,
        tenant_id: &str,
        create: bool,
        operation: impl FnOnce(Option<&mut Box<dyn SearchEngine>>) -> IndexResult<R>,
    ) -> IndexResult<R> {
        let name = tenant_dir_name(tenant_id)?;
        let mut cache = self
            .engines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !cache.contains_key(tenant_id) {
            let index_dir = self.tenant_dir(&name).join("tantivy");
            if create {
                fs::create_dir_all(&index_dir)?;
                cache.insert(
                    tenant_id.to_string(),
                    Box::new(TantivyEngine::create(&index_dir)?),
                );
            } else if index_dir.exists() {
                cache.insert(
                    tenant_id.to_string(),
                    Box::new(TantivyEngine::open(&index_dir)?),
                );
            }
        }
        let engine = cache.get_mut(tenant_id);
        operation(engine)
    }
}

/// Validate a tenant id and return its filesystem-safe directory name.
fn tenant_dir_name(tenant_id: &str) -> IndexResult<String> {
    let safe = !tenant_id.is_empty()
        && tenant_id.len() <= 128
        && !tenant_id.contains("..")
        && tenant_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if safe {
        Ok(tenant_id.to_string())
    } else {
        Err(IndexError::InvalidTenant {
            tenant_id: tenant_id.to_string(),
        })
    }
}

fn read_manifest(path: &Path) -> IndexResult<Option<TenantManifest>> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(serde_json::from_str(&contents)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(IndexError::Io(error.to_string())),
    }
}

fn write_manifest(path: &Path, manifest: &TenantManifest) -> IndexResult<()> {
    let encoded = serde_json::to_string_pretty(manifest)?;
    fs::write(path, encoded)?;
    Ok(())
}
