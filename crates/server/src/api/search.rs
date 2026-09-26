//! `GET /v1/search` (DOMAIN.md §14, §11.3): global search across threads, artifacts,
//! knowledge and evidence as one typed SearchProgram.
//!
//! The query is data, never a predicate: `q` is bound as a parameter of every SQL
//! statement and passed to INT-004's typed [`SearchProgram`] for artifact content. Channels
//! are an enumeration, budgets are bounded, and every hit carries provenance
//! `(source_id, locator, snapshot)` so a caller can cite it.
//!
//! The artifacts channel has two sources: titles from the canonical table (always) and
//! content from the derived Tantivy index when a `SearchIndex` is configured
//! (`QUANSIO_SEARCH_INDEX_ROOT`). The index is INT-004's rebuildable projection; the
//! service rebuilds a tenant's index when the canonical corpus fingerprint changed since
//! it last looked, and `CreateArtifact` applies its own document incrementally.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use quansio_indexer::{
    AuthoritativeSource, Channel, IndexResult, ObjectTextProvider, PostgresArtifactSource,
    SearchBudget, SearchIndex, SearchProgram, SearchScope, SourceChange, SourceScope,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::artifacts::{is_text_media, MAX_INDEXED_TEXT_BYTES};
use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::artifacts::{ObjectKey, ObjectStore};
use crate::control::schema;

/// Longest query accepted.
pub const MAX_QUERY_CHARS: usize = 200;
/// Largest `max_results` accepted per channel.
pub const MAX_RESULTS: u32 = 50;
/// Snippet bound for the Postgres-backed channels.
const SNIPPET_CHARS: usize = 240;

/// The channels `/v1/search` serves; anything else is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchChannel {
    /// Artifact titles (canonical) and content (derived index).
    Artifacts,
    /// Messages in the workspace's threads.
    Threads,
    /// Knowledge entries (INT-006).
    Knowledge,
    /// Evidence rows (CORE-007).
    Evidence,
}

impl SearchChannel {
    /// Every channel, in the order results are grouped.
    pub const ALL: [Self; 4] = [
        Self::Artifacts,
        Self::Threads,
        Self::Knowledge,
        Self::Evidence,
    ];

    /// Wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Artifacts => "artifacts",
            Self::Threads => "threads",
            Self::Knowledge => "knowledge",
            Self::Evidence => "evidence",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "artifacts" => Some(Self::Artifacts),
            "threads" => Some(Self::Threads),
            "knowledge" => Some(Self::Knowledge),
            "evidence" => Some(Self::Evidence),
            _ => None,
        }
    }
}

/// The index plus what the service remembers about each tenant's corpus.
pub struct SearchService {
    index: Arc<SearchIndex>,
    fingerprints: Mutex<HashMap<String, CorpusState>>,
}

impl SearchService {
    /// A service over an opened index.
    #[must_use]
    pub fn new(index: Arc<SearchIndex>) -> Self {
        Self {
            index,
            fingerprints: Mutex::new(HashMap::new()),
        }
    }

    /// The index itself.
    #[must_use]
    pub fn index(&self) -> &Arc<SearchIndex> {
        &self.index
    }

    /// Make sure the tenant's index reflects the canonical artifact corpus. When only new versions
    /// arrived since the last look, only they are indexed; the whole corpus is re-read when the
    /// tenant has never been indexed here, when an artifact was deleted, or when the new versions
    /// do not account for the change (so nothing is ever missed).
    async fn refresh(&self, state: &ApiState, tenant_id: &str) -> Result<(), ApiError> {
        let Some(objects) = state.objects().cloned() else {
            return Ok(());
        };
        let current = corpus_state(state, tenant_id).await?;
        let known = self
            .fingerprints
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(tenant_id)
            .cloned();
        let has_snapshot = self
            .index
            .snapshot(tenant_id)
            .map_err(index_error)?
            .is_some();
        if known.as_ref() == Some(&current) && has_snapshot {
            return Ok(());
        }
        let source = PostgresArtifactSource::new(state.pool().clone(), ObjectText { objects });
        let caught_up = match known {
            Some(known)
                if has_snapshot
                    && current.deleted == known.deleted
                    && current.versions > known.versions =>
            {
                let fresh = source
                    .load(&SourceScope::tenant(tenant_id).after(known.newest.clone()))
                    .await
                    .map_err(index_error)?;
                if i64::try_from(fresh.len()).unwrap_or(i64::MAX)
                    == current.versions - known.versions
                {
                    for document in fresh {
                        self.index
                            .apply(SourceChange::upsert(document))
                            .map_err(index_error)?;
                    }
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if !caught_up {
            self.index
                .rebuild_from(&source, &SourceScope::tenant(tenant_id))
                .await
                .map_err(index_error)?;
        }
        self.fingerprints
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(tenant_id.to_string(), current);
        Ok(())
    }
}

/// Text for the index from the object store: text media only, bounded, lossy UTF-8.
struct ObjectText {
    objects: Arc<dyn ObjectStore>,
}

#[async_trait::async_trait]
impl ObjectTextProvider for ObjectText {
    async fn text_for(&self, object_key: &str, media_type: &str) -> IndexResult<Option<String>> {
        if !is_text_media(media_type) {
            return Ok(None);
        }
        let key = ObjectKey::parse(object_key)
            .map_err(|error| quansio_indexer::IndexError::Source(error.to_string()))?;
        let bytes = self
            .objects
            .get(&key)
            .await
            .map_err(|error| quansio_indexer::IndexError::Source(error.to_string()))?;
        if bytes.len() > MAX_INDEXED_TEXT_BYTES {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }
}

/// What the index last saw of a tenant's artifact corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CorpusState {
    versions: i64,
    newest: String,
    deleted: i64,
}

async fn corpus_state(state: &ApiState, tenant_id: &str) -> Result<CorpusState, ApiError> {
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, tenant_id)
        .await
        .map_err(projection_error)?;
    let row = sqlx::query(
        "SELECT count(*) AS versions, COALESCE(max(v.id), '') AS newest, \
                count(*) FILTER (WHERE a.deleted_at IS NOT NULL) AS deleted \
         FROM artifact_versions v JOIN artifacts a ON a.id = v.artifact_id WHERE v.tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    Ok(CorpusState {
        versions: row.get::<i64, _>("versions"),
        newest: row.get::<String, _>("newest"),
        deleted: row.get::<i64, _>("deleted"),
    })
}

fn projection_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), "search")
}

fn index_error(error: quansio_indexer::IndexError) -> ApiError {
    ApiError::new(
        ApiErrorCode::Internal,
        format!("search index: {error}"),
        "search",
    )
}

/// `?q=&workspace_id=[&channels=a,b][&max_results=]`.
#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    q: String,
    workspace_id: String,
    #[serde(default)]
    channels: Option<String>,
    #[serde(default)]
    max_results: Option<u32>,
}

/// `GET /v1/search`: execute the typed program and return grouped, budgeted hits.
pub async fn search(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let q = query.q.trim();
    if q.is_empty() || q.chars().count() > MAX_QUERY_CHARS {
        return Err(ApiError::new(
            ApiErrorCode::ValidationBounds,
            format!("q must be 1..={MAX_QUERY_CHARS} characters"),
            "search",
        ));
    }
    let max_results = query.max_results.unwrap_or(20);
    if max_results == 0 || max_results > MAX_RESULTS {
        return Err(ApiError::new(
            ApiErrorCode::ValidationBounds,
            format!("max_results must be 1..={MAX_RESULTS}"),
            "search",
        ));
    }
    let channels: Vec<SearchChannel> = match query.channels.as_deref() {
        None | Some("") => SearchChannel::ALL.to_vec(),
        Some(list) => {
            let mut parsed = Vec::new();
            for item in list.split(',') {
                let channel = SearchChannel::parse(item).ok_or_else(|| {
                    ApiError::new(
                        ApiErrorCode::ValidationSchema,
                        format!("{item:?} is not a search channel (artifacts, threads, knowledge, evidence)"),
                        "search",
                    )
                })?;
                if !parsed.contains(&channel) {
                    parsed.push(channel);
                }
            }
            parsed
        }
    };

    let mut hits: Vec<Value> = Vec::new();
    let mut truncated = false;
    let mut engines: Vec<Value> = Vec::new();
    for channel in &channels {
        let (mut found, more) = match channel {
            SearchChannel::Artifacts => {
                let (titles, more_titles) = artifact_titles(
                    &state,
                    &scope.tenant_id,
                    &query.workspace_id,
                    q,
                    max_results,
                )
                .await?;
                let mut found = titles;
                let mut more = more_titles;
                if let Some(service) = state.search() {
                    service.refresh(&state, &scope.tenant_id).await?;
                    let (content, more_content, snapshot) = artifact_content(
                        service,
                        &scope.tenant_id,
                        &query.workspace_id,
                        q,
                        max_results,
                    )?;
                    engines.push(json!({ "channel": "artifacts", "engine": "quansio-indexer", "snapshot": snapshot }));
                    // A content hit for an artifact already found by title keeps the title hit's
                    // slot and gains the snippet; anything else is appended.
                    for hit in content {
                        let id = hit["id"].as_str().unwrap_or_default().to_string();
                        if let Some(existing) =
                            found.iter_mut().find(|existing| existing["id"] == id)
                        {
                            existing["snippet"] = hit["snippet"].clone();
                            existing["provenance"] = hit["provenance"].clone();
                            existing["score"] = hit["score"].clone();
                        } else {
                            found.push(hit);
                        }
                    }
                    more |= more_content;
                } else {
                    engines.push(json!({ "channel": "artifacts", "engine": "postgres-titles" }));
                }
                (found, more)
            }
            SearchChannel::Threads => {
                thread_messages(
                    &state,
                    &scope.tenant_id,
                    &query.workspace_id,
                    q,
                    max_results,
                )
                .await?
            }
            SearchChannel::Knowledge => {
                knowledge(
                    &state,
                    &scope.tenant_id,
                    &query.workspace_id,
                    q,
                    max_results,
                )
                .await?
            }
            SearchChannel::Evidence => {
                evidence(
                    &state,
                    &scope.tenant_id,
                    &query.workspace_id,
                    q,
                    max_results,
                )
                .await?
            }
        };
        if found.len() > max_results as usize {
            found.truncate(max_results as usize);
            truncated = true;
        }
        truncated |= more;
        hits.extend(found);
    }
    Ok(Json(json!({
        "program": {
            "q": q,
            "scope": { "tenant_id": scope.tenant_id, "workspace_id": query.workspace_id },
            "channels": channels.iter().map(|channel| channel.as_str()).collect::<Vec<_>>(),
            "budget": { "max_results": max_results },
        },
        "hits": hits,
        "truncated": truncated,
        "engines": engines,
    })))
}

fn snippet(text: &str, q: &str) -> String {
    let lower = text.to_lowercase();
    let needle = q.to_lowercase();
    let start = lower.find(&needle).unwrap_or(0);
    let begin = text
        .char_indices()
        .map(|(index, _)| index)
        .filter(|index| *index <= start)
        .rev()
        .nth(60)
        .unwrap_or(0);
    let excerpt: String = text[begin..].chars().take(SNIPPET_CHARS).collect();
    excerpt.replace(['\n', '\r'], " ")
}

async fn artifact_titles(
    state: &ApiState,
    tenant_id: &str,
    workspace_id: &str,
    q: &str,
    max_results: u32,
) -> Result<(Vec<Value>, bool), ApiError> {
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, tenant_id)
        .await
        .map_err(projection_error)?;
    let pattern = format!("%{}%", like_escape(q));
    let rows = sqlx::query(
        "SELECT a.id, a.title, a.kind, a.current_version_id, v.media_type, v.content_digest, a.updated_at \
         FROM artifacts a LEFT JOIN artifact_versions v ON v.id = a.current_version_id \
         WHERE a.tenant_id = $1 AND a.workspace_id = $2 AND a.deleted_at IS NULL AND a.title ILIKE $3 ESCAPE '\\' \
         ORDER BY a.updated_at DESC LIMIT $4",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .bind(&pattern)
    .bind(i64::from(max_results) + 1)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    let more = rows.len() > max_results as usize;
    Ok((
        rows.iter()
            .take(max_results as usize)
            .map(|row| {
                let version: Option<String> = row.get("current_version_id");
                json!({
                    "channel": "artifacts",
                    "id": row.get::<String, _>("id"),
                    "title": row.get::<String, _>("title"),
                    "kind": row.get::<String, _>("kind"),
                    "snippet": row.get::<String, _>("title"),
                    "media_type": row.get::<Option<String>, _>("media_type"),
                    "ref": { "artifact_id": row.get::<String, _>("id"), "version_id": version },
                    "provenance": {
                        "source_id": row.get::<String, _>("id"),
                        "locator": version.as_ref().map_or_else(|| "title".to_string(), |v| format!("artifact_version:{v}")),
                        "snapshot": "canonical",
                    },
                    "score": 1.0,
                    "matched": "title",
                })
            })
            .collect(),
        more,
    ))
}

fn artifact_content(
    service: &SearchService,
    tenant_id: &str,
    workspace_id: &str,
    q: &str,
    max_results: u32,
) -> Result<(Vec<Value>, bool, String), ApiError> {
    let program = SearchProgram::new(SearchScope::workspace(tenant_id, workspace_id), q)
        .with_channels(vec![Channel::Exact, Channel::Lexical])
        .with_budget(SearchBudget::new(max_results, 4_096));
    let results = service.index.search(&program).map_err(index_error)?;
    let snapshot = format!("{:?}", results.snapshot);
    let hits = results
        .results
        .iter()
        .map(|hit| {
            json!({
                "channel": "artifacts",
                "id": hit.source_id,
                "title": hit.title,
                "snippet": hit.snippet,
                "media_type": hit.media_type,
                "ref": { "artifact_id": hit.source_id, "version_id": hit.locator.strip_prefix("artifact_version:") },
                "provenance": { "source_id": hit.source_id, "locator": hit.locator, "snapshot": hit.snapshot },
                "score": hit.score,
                "matched": "content",
            })
        })
        .collect();
    Ok((hits, results.truncation.is_truncated(), snapshot))
}

async fn thread_messages(
    state: &ApiState,
    tenant_id: &str,
    workspace_id: &str,
    q: &str,
    max_results: u32,
) -> Result<(Vec<Value>, bool), ApiError> {
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, tenant_id)
        .await
        .map_err(projection_error)?;
    let pattern = format!("%{}%", like_escape(q));
    let rows = sqlx::query(
        "SELECT m.id, m.thread_id, m.seq, m.author_kind, m.run_id, m.created_at, t.title AS thread_title, \
                (SELECT string_agg(b->>'text', ' ') FROM jsonb_array_elements(m.content_blocks) b) AS text \
         FROM messages m JOIN threads t ON t.id = m.thread_id \
         WHERE m.tenant_id = $1 AND m.workspace_id = $2 AND m.deleted_at IS NULL \
           AND (SELECT string_agg(b->>'text', ' ') FROM jsonb_array_elements(m.content_blocks) b) ILIKE $3 ESCAPE '\\' \
         ORDER BY m.created_at DESC LIMIT $4",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .bind(&pattern)
    .bind(i64::from(max_results) + 1)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    let more = rows.len() > max_results as usize;
    Ok((
        rows.iter()
            .take(max_results as usize)
            .map(|row| {
                let text: Option<String> = row.get("text");
                json!({
                    "channel": "threads",
                    "id": row.get::<String, _>("id"),
                    "title": row.get::<Option<String>, _>("thread_title").unwrap_or_else(|| row.get::<String, _>("thread_id")),
                    "snippet": snippet(text.as_deref().unwrap_or(""), q),
                    "ref": { "thread_id": row.get::<String, _>("thread_id"), "message_id": row.get::<String, _>("id"), "run_id": row.get::<Option<String>, _>("run_id") },
                    "provenance": { "source_id": row.get::<String, _>("thread_id"), "locator": format!("message:{}", row.get::<String, _>("id")), "snapshot": "canonical" },
                    "score": 1.0,
                    "matched": "message",
                })
            })
            .collect(),
        more,
    ))
}

async fn knowledge(
    state: &ApiState,
    tenant_id: &str,
    workspace_id: &str,
    q: &str,
    max_results: u32,
) -> Result<(Vec<Value>, bool), ApiError> {
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, tenant_id)
        .await
        .map_err(projection_error)?;
    let pattern = format!("%{}%", like_escape(q));
    let rows = sqlx::query(
        "SELECT id, kind, scope, status, content_ref, content, confidence, provenance, updated_at \
         FROM knowledge_entries \
         WHERE tenant_id = $1 AND (workspace_id = $2 OR workspace_id IS NULL) \
           AND status IN ('active', 'verified') \
           AND (COALESCE(content_ref, '') ILIKE $3 ESCAPE '\\' OR COALESCE(content::text, '') ILIKE $3 ESCAPE '\\') \
         ORDER BY updated_at DESC LIMIT $4",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .bind(&pattern)
    .bind(i64::from(max_results) + 1)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    let more = rows.len() > max_results as usize;
    Ok((
        rows.iter()
            .take(max_results as usize)
            .map(|row| {
                let content: Option<Value> = row.get("content");
                let text = content.as_ref().map(Value::to_string).unwrap_or_default();
                json!({
                    "channel": "knowledge",
                    "id": row.get::<String, _>("id"),
                    "title": row.get::<Option<String>, _>("content_ref").unwrap_or_else(|| row.get::<String, _>("kind")),
                    "snippet": snippet(&text, q),
                    "ref": { "knowledge_id": row.get::<String, _>("id"), "scope": row.get::<String, _>("scope"), "status": row.get::<String, _>("status") },
                    "provenance": { "source_id": row.get::<String, _>("id"), "locator": "knowledge_entry", "snapshot": "canonical", "sources": row.get::<Value, _>("provenance") },
                    "score": row.get::<f64, _>("confidence"),
                    "matched": "knowledge",
                })
            })
            .collect(),
        more,
    ))
}

async fn evidence(
    state: &ApiState,
    tenant_id: &str,
    workspace_id: &str,
    q: &str,
    max_results: u32,
) -> Result<(Vec<Value>, bool), ApiError> {
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, tenant_id)
        .await
        .map_err(projection_error)?;
    let pattern = format!("%{}%", like_escape(q));
    let rows = sqlx::query(
        "SELECT id, run_id, step_id, effect_id, kind, inline_summary, content_digest, captured_at \
         FROM evidence WHERE tenant_id = $1 AND workspace_id = $2 \
           AND (COALESCE(inline_summary, '') ILIKE $3 ESCAPE '\\' OR kind = $4) \
         ORDER BY captured_at DESC LIMIT $5",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .bind(&pattern)
    .bind(q)
    .bind(i64::from(max_results) + 1)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    let more = rows.len() > max_results as usize;
    Ok((
        rows.iter()
            .take(max_results as usize)
            .map(|row| {
                json!({
                    "channel": "evidence",
                    "id": row.get::<String, _>("id"),
                    "title": row.get::<String, _>("kind"),
                    "snippet": snippet(row.get::<Option<String>, _>("inline_summary").as_deref().unwrap_or(""), q),
                    "ref": { "evidence_id": row.get::<String, _>("id"), "run_id": row.get::<Option<String>, _>("run_id"), "step_id": row.get::<Option<String>, _>("step_id"), "effect_id": row.get::<Option<String>, _>("effect_id") },
                    "provenance": { "source_id": row.get::<String, _>("id"), "locator": format!("digest:{}", row.get::<String, _>("content_digest")), "snapshot": "canonical" },
                    "score": 1.0,
                    "matched": "evidence",
                })
            })
            .collect(),
        more,
    ))
}

/// Escape LIKE metacharacters so the query stays data.
fn like_escape(q: &str) -> String {
    q.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
