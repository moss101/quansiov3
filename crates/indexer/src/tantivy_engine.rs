//! Tantivy-backed [`SearchEngine`] implementation.
//!
//! # Why Tantivy
//!
//! Tantivy is an embedded, pure-Rust full-text index: it runs in-process, needs no
//! external service, writes ordinary files under the tenant directory (so the index
//! is derived and rebuildable per DOSSIER.md §9), and provides BM25 lexical scoring
//! with no query-language dependency. The exact and symbol channels reuse its
//! raw-tokenizer term queries, so a request is a set of literal terms and never a
//! parsed predicate. The whole engine sits behind [`SearchEngine`], so replacing it
//! is a one-module change.

use std::collections::HashMap;
use std::path::Path;

use tantivy::collector::{Count, TopDocs};
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Schema, Value, STORED, STRING, TEXT};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, TantivyDocument, Term};

use crate::document::{tokenize_alnum, SourceDocument};
use crate::engine::{EngineHit, EngineOutcome, EngineQuery, SearchEngine};
use crate::error::{IndexError, IndexResult};
use crate::program::Channel;

const WRITER_HEAP_BYTES: usize = 50_000_000;
const EXACT_SCORE_BASE: f64 = 100.0;
const SYMBOL_SCORE_BASE: f64 = 10.0;

/// Field handles for the tenant index schema.
#[derive(Debug, Clone, Copy)]
struct Fields {
    tenant_id: Field,
    workspace_id: Field,
    source_id: Field,
    version_id: Field,
    locator: Field,
    title: Field,
    body: Field,
    symbols: Field,
    exact: Field,
    media_type: Field,
    artifact_kind: Field,
    content_digest: Field,
}

/// Embedded Tantivy engine bound to one tenant directory.
pub struct TantivyEngine {
    schema: Schema,
    fields: Fields,
    reader: IndexReader,
    writer: IndexWriter,
}

impl TantivyEngine {
    /// Create a new empty index in `directory`.
    ///
    /// # Errors
    /// Returns [`IndexError::Io`]/[`IndexError::Engine`] when the directory or index
    /// cannot be created.
    pub fn create(directory: &Path) -> IndexResult<Self> {
        let (schema, fields) = build_schema();
        let index = Index::create_in_dir(directory, schema.clone())
            .map_err(|error| IndexError::Engine(error.to_string()))?;
        Self::from_index(index, schema, fields)
    }

    /// Open an existing index in `directory`.
    ///
    /// # Errors
    /// Returns [`IndexError::Engine`] when the index cannot be opened.
    pub fn open(directory: &Path) -> IndexResult<Self> {
        let index =
            Index::open_in_dir(directory).map_err(|error| IndexError::Engine(error.to_string()))?;
        let schema = index.schema();
        let fields = resolve_fields(&schema)?;
        Self::from_index(index, schema, fields)
    }

    fn from_index(index: Index, schema: Schema, fields: Fields) -> IndexResult<Self> {
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|error: tantivy::TantivyError| IndexError::Engine(error.to_string()))?;
        let writer = index
            .writer(WRITER_HEAP_BYTES)
            .map_err(|error| IndexError::Engine(error.to_string()))?;
        Ok(Self {
            schema,
            fields,
            reader,
            writer,
        })
    }

    /// The schema, exposed for tests that assert field-level invariants.
    #[must_use]
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    fn document(&self, document: &SourceDocument) -> TantivyDocument {
        let mut target = TantivyDocument::default();
        target.add_text(self.fields.tenant_id, &document.tenant_id);
        target.add_text(self.fields.workspace_id, &document.workspace_id);
        target.add_text(self.fields.source_id, &document.source_id);
        target.add_text(self.fields.version_id, &document.version_id);
        target.add_text(self.fields.locator, &document.locator);
        target.add_text(self.fields.title, &document.title);
        target.add_text(self.fields.body, &document.body);
        target.add_text(self.fields.media_type, &document.media_type);
        target.add_text(
            self.fields.artifact_kind,
            document.artifact_kind.as_deref().unwrap_or(""),
        );
        target.add_text(self.fields.content_digest, &document.content_digest);
        for symbol in document.symbol_terms() {
            target.add_text(self.fields.symbols, &symbol);
        }
        for token in exact_terms(document) {
            target.add_text(self.fields.exact, &token);
        }
        target
    }

    fn requery_reader(&self) -> IndexResult<()> {
        self.reader
            .reload()
            .map_err(|error| IndexError::Engine(error.to_string()))
    }

    fn searcher(&self) -> IndexResult<Searcher> {
        self.requery_reader()?;
        Ok(self.reader.searcher())
    }

    fn field_text(&self, document: &TantivyDocument, field: Field) -> String {
        document
            .get_first(field)
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default()
    }

    fn filter_query(&self, query: &EngineQuery) -> Option<Box<dyn Query>> {
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        if let Some(workspace_id) = &query.workspace_id {
            clauses.push((
                Occur::Must,
                Box::new(term_query(self.fields.workspace_id, workspace_id)),
            ));
        }
        push_filter(
            &mut clauses,
            self.fields.media_type,
            &query.filters.media_types,
        );
        push_filter(
            &mut clauses,
            self.fields.artifact_kind,
            &query.filters.artifact_kinds,
        );
        push_filter(
            &mut clauses,
            self.fields.source_id,
            &query.filters.source_ids,
        );
        (!clauses.is_empty()).then(|| Box::new(BooleanQuery::new(clauses)) as Box<dyn Query>)
    }

    fn channel_query(&self, channel: Channel, query: &EngineQuery) -> Option<Box<dyn Query>> {
        let (tokens, fields): (&[String], &[Field]) = match channel {
            Channel::Lexical => (
                &query.lexical_tokens,
                &[self.fields.body, self.fields.title],
            ),
            Channel::Exact => (&query.exact_tokens, &[self.fields.exact]),
            Channel::Symbol => (&query.symbol_tokens, &[self.fields.symbols]),
            Channel::Semantic | Channel::Graph | Channel::History | Channel::Memory => return None,
        };
        if tokens.is_empty() {
            return None;
        }
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for token in tokens {
            let alternatives: Vec<(Occur, Box<dyn Query>)> = fields
                .iter()
                .map(|field| {
                    (
                        Occur::Should,
                        Box::new(term_query(*field, token)) as Box<dyn Query>,
                    )
                })
                .collect();
            clauses.push((Occur::Must, Box::new(BooleanQuery::new(alternatives))));
        }
        Some(Box::new(BooleanQuery::new(clauses)))
    }

    fn combine(&self, filter: Option<Box<dyn Query>>, channel: Box<dyn Query>) -> Box<dyn Query> {
        match filter {
            Some(filter) => Box::new(BooleanQuery::new(vec![
                (Occur::Must, channel),
                (Occur::Must, filter),
            ])),
            None => channel,
        }
    }
}

impl SearchEngine for TantivyEngine {
    fn rebuild(&mut self, documents: &[SourceDocument]) -> IndexResult<()> {
        self.writer
            .delete_all_documents()
            .map_err(|error| IndexError::Engine(error.to_string()))?;
        for document in documents {
            let target = self.document(document);
            self.writer
                .add_document(target)
                .map_err(|error| IndexError::Engine(error.to_string()))?;
        }
        self.writer
            .commit()
            .map_err(|error| IndexError::Engine(error.to_string()))?;
        self.requery_reader()
    }

    fn upsert(&mut self, document: &SourceDocument) -> IndexResult<bool> {
        if let Some(existing) = self.digest_of(&document.version_id)? {
            if existing == document.content_digest {
                return Ok(false);
            }
        }
        self.writer
            .delete_term(term(self.fields.version_id, &document.version_id));
        let target = self.document(document);
        self.writer
            .add_document(target)
            .map_err(|error| IndexError::Engine(error.to_string()))?;
        self.writer
            .commit()
            .map_err(|error| IndexError::Engine(error.to_string()))?;
        self.requery_reader()?;
        Ok(true)
    }

    fn delete(&mut self, version_id: &str) -> IndexResult<bool> {
        if self.digest_of(version_id)?.is_none() {
            return Ok(false);
        }
        self.writer
            .delete_term(term(self.fields.version_id, version_id));
        self.writer
            .commit()
            .map_err(|error| IndexError::Engine(error.to_string()))?;
        self.requery_reader()?;
        Ok(true)
    }

    fn search(&self, query: &EngineQuery) -> IndexResult<EngineOutcome> {
        let searcher = self.searcher()?;
        let limit = query.limit.max(1);
        let mut merged: HashMap<String, EngineHit> = HashMap::new();
        let mut total_matches: u64 = 0;
        for channel in &query.channels {
            let Some(channel_query) = self.channel_query(*channel, query) else {
                continue;
            };
            let combined = self.combine(self.filter_query(query), channel_query);
            let (top, count) = searcher
                .search(
                    combined.as_ref(),
                    &(TopDocs::with_limit(limit).order_by_score(), Count),
                )
                .map_err(|error| IndexError::Engine(error.to_string()))?;
            total_matches = total_matches.max(count as u64);
            let base = match channel {
                Channel::Exact => EXACT_SCORE_BASE,
                Channel::Symbol => SYMBOL_SCORE_BASE,
                _ => 0.0,
            };
            for (score, address) in top {
                let document = searcher
                    .doc::<TantivyDocument>(address)
                    .map_err(|error| IndexError::Engine(error.to_string()))?;
                let version_id = self.field_text(&document, self.fields.version_id);
                let hit_score = base + f64::from(score);
                merged
                    .entry(version_id.clone())
                    .and_modify(|existing| {
                        if hit_score > existing.score {
                            existing.score = hit_score;
                        }
                    })
                    .or_insert_with(|| EngineHit {
                        source_id: self.field_text(&document, self.fields.source_id),
                        version_id,
                        locator: self.field_text(&document, self.fields.locator),
                        title: self.field_text(&document, self.fields.title),
                        snippet: make_snippet(
                            &self.field_text(&document, self.fields.body),
                            &query.lexical_tokens,
                        ),
                        media_type: self.field_text(&document, self.fields.media_type),
                        content_digest: self.field_text(&document, self.fields.content_digest),
                        score: hit_score,
                    });
            }
        }
        let mut hits: Vec<EngineHit> = merged.into_values().collect();
        hits.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.source_id.cmp(&right.source_id))
                .then_with(|| left.locator.cmp(&right.locator))
        });
        hits.truncate(limit);
        Ok(EngineOutcome {
            hits,
            total_matches,
        })
    }

    fn digest_of(&self, version_id: &str) -> IndexResult<Option<String>> {
        let searcher = self.searcher()?;
        let query = term_query(self.fields.version_id, version_id);
        let top = searcher
            .search(&query, &TopDocs::with_limit(1).order_by_score())
            .map_err(|error| IndexError::Engine(error.to_string()))?;
        let Some((_score, address)) = top.first() else {
            return Ok(None);
        };
        let document = searcher
            .doc::<TantivyDocument>(*address)
            .map_err(|error| IndexError::Engine(error.to_string()))?;
        Ok(Some(self.field_text(&document, self.fields.content_digest)))
    }

    fn document_count(&self) -> IndexResult<u64> {
        let searcher = self.searcher()?;
        Ok(searcher.num_docs())
    }
}

/// Add one filter category as a `Must` clause whose alternatives are OR-ed.
///
/// Categories compose with AND, values inside one category with OR, so typed
/// filters can only narrow the result set.
fn push_filter(clauses: &mut Vec<(Occur, Box<dyn Query>)>, field: Field, values: &[String]) {
    if values.is_empty() {
        return;
    }
    let alternatives: Vec<(Occur, Box<dyn Query>)> = values
        .iter()
        .map(|value| {
            (
                Occur::Should,
                Box::new(term_query(field, value)) as Box<dyn Query>,
            )
        })
        .collect();
    clauses.push((Occur::Must, Box::new(BooleanQuery::new(alternatives))));
}

fn term(field: Field, value: &str) -> Term {
    Term::from_field_text(field, value)
}

fn term_query(field: Field, value: &str) -> TermQuery {
    TermQuery::new(term(field, value), IndexRecordOption::Basic)
}

fn exact_terms(document: &SourceDocument) -> Vec<String> {
    /// Bound the duplicated exact token list for very large artifacts.
    const MAX_EXACT_TERMS: usize = 16_384;
    let mut terms: Vec<String> = tokenize_alnum(&format!(
        "{} {} {}",
        document.title, document.source_id, document.locator
    ));
    terms.extend(tokenize_alnum(&document.body));
    terms.sort();
    terms.dedup();
    terms.truncate(MAX_EXACT_TERMS);
    terms
}

fn make_snippet(body: &str, tokens: &[String]) -> String {
    const WINDOW: usize = 200;
    const LEAD: usize = 40;
    let characters: Vec<char> = body.chars().collect();
    if characters.len() <= WINDOW {
        return body.to_string();
    }
    let lowered = body.to_lowercase();
    let match_at = tokens
        .iter()
        .filter_map(|token| lowered.find(token))
        .min()
        .map(|byte| lowered[..byte].chars().count());
    let start = match match_at {
        Some(position) if position > LEAD => position - LEAD,
        _ => 0,
    };
    let end = (start + WINDOW).min(characters.len());
    let snippet: String = characters[start..end].iter().collect();
    if end < characters.len() {
        format!("{snippet}…")
    } else {
        snippet
    }
}

fn build_schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let raw = || STORED | STRING;
    let tenant_id = builder.add_text_field("tenant_id", raw());
    let workspace_id = builder.add_text_field("workspace_id", raw());
    let source_id = builder.add_text_field("source_id", raw());
    let version_id = builder.add_text_field("version_id", raw());
    let locator = builder.add_text_field("locator", raw());
    let title = builder.add_text_field("title", TEXT | STORED);
    let body = builder.add_text_field("body", TEXT | STORED);
    let symbols = builder.add_text_field("symbols", STRING);
    let exact = builder.add_text_field("exact", STRING);
    let media_type = builder.add_text_field("media_type", raw());
    let artifact_kind = builder.add_text_field("artifact_kind", raw());
    let content_digest = builder.add_text_field("content_digest", raw());
    let schema = builder.build();
    (
        schema,
        Fields {
            tenant_id,
            workspace_id,
            source_id,
            version_id,
            locator,
            title,
            body,
            symbols,
            exact,
            media_type,
            artifact_kind,
            content_digest,
        },
    )
}

fn resolve_fields(schema: &Schema) -> IndexResult<Fields> {
    let field = |name: &str| {
        schema
            .get_field(name)
            .map_err(|error| IndexError::Encoding(error.to_string()))
    };
    Ok(Fields {
        tenant_id: field("tenant_id")?,
        workspace_id: field("workspace_id")?,
        source_id: field("source_id")?,
        version_id: field("version_id")?,
        locator: field("locator")?,
        title: field("title")?,
        body: field("body")?,
        symbols: field("symbols")?,
        exact: field("exact")?,
        media_type: field("media_type")?,
        artifact_kind: field("artifact_kind")?,
        content_digest: field("content_digest")?,
    })
}
