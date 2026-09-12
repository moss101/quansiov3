//! Authoritative source documents projected into the derived index.
//!
//! A [`SourceDocument`] is a read-only projection of an authoritative row (today an
//! `ArtifactVersion` plus the text extracted from its stored bytes). The index never
//! writes back to these fields, so the projection can always be recomputed.

use std::collections::BTreeSet;

/// Kind of authoritative source a document was projected from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// An immutable artifact version (DOMAIN.md §10.1).
    Artifact,
}

impl SourceKind {
    /// Canonical wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Artifact => "artifact",
        }
    }
}

/// One authoritative row projected into the index.
///
/// `source_id` is the artifact identity, `version_id` the immutable version identity
/// and `locator` the stable within-source locator returned as provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDocument {
    /// Owning tenant (`tn_…`).
    pub tenant_id: String,
    /// Owning workspace (`ws_…`).
    pub workspace_id: String,
    /// What the row was projected from.
    pub source_kind: SourceKind,
    /// Artifact identity.
    pub source_id: String,
    /// Immutable artifact version identity.
    pub version_id: String,
    /// Stable locator within the source (provenance).
    pub locator: String,
    /// Human title used for lexical matching.
    pub title: String,
    /// Extracted text body.
    pub body: String,
    /// Explicit symbols extracted from the body.
    pub symbols: Vec<String>,
    /// Media type of the stored bytes.
    pub media_type: String,
    /// Artifact kind (`document`, `code`, …), when known.
    pub artifact_kind: Option<String>,
    /// SHA-256 digest of the stored bytes.
    pub content_digest: String,
}

impl SourceDocument {
    /// Build an artifact-version projection.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn artifact(
        tenant_id: impl Into<String>,
        workspace_id: impl Into<String>,
        source_id: impl Into<String>,
        version_id: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
        media_type: impl Into<String>,
        content_digest: impl Into<String>,
    ) -> Self {
        let version_id = version_id.into();
        Self {
            tenant_id: tenant_id.into(),
            workspace_id: workspace_id.into(),
            source_kind: SourceKind::Artifact,
            source_id: source_id.into(),
            locator: format!("artifact_version:{version_id}"),
            version_id,
            title: title.into(),
            body: body.into(),
            symbols: Vec::new(),
            media_type: media_type.into(),
            artifact_kind: None,
            content_digest: content_digest.into(),
        }
    }

    /// Set the explicit symbols carried by this projection.
    #[must_use]
    pub fn with_symbols(mut self, symbols: Vec<String>) -> Self {
        self.symbols = symbols;
        self
    }

    /// Set the artifact kind.
    #[must_use]
    pub fn with_artifact_kind(mut self, kind: impl Into<String>) -> Self {
        self.artifact_kind = Some(kind.into());
        self
    }

    /// The stable key `(tenant, version)` used for upsert/delete.
    #[must_use]
    pub fn key(&self) -> (&str, &str) {
        (&self.tenant_id, &self.version_id)
    }

    /// Lexical/exact tokens: lowercased alphanumeric runs, matching Tantivy's
    /// default tokenizer so a query term addresses exactly what was indexed.
    #[must_use]
    pub fn body_tokens(&self) -> Vec<String> {
        tokenize_alnum(&format!("{} {}", self.title, self.body))
    }

    /// Symbols projected for the symbol channel (explicit plus extracted).
    #[must_use]
    pub fn symbol_terms(&self) -> Vec<String> {
        let mut symbols: BTreeSet<String> = self
            .symbols
            .iter()
            .map(|symbol| symbol.to_lowercase())
            .filter(|symbol| !symbol.is_empty())
            .collect();
        symbols.extend(extract_symbols(&self.body));
        symbols.into_iter().collect()
    }

    /// Canonical, stable encoding used for corpus fingerprints and idempotency.
    #[must_use]
    pub fn canonical(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.tenant_id,
            self.workspace_id,
            self.source_kind.as_str(),
            self.source_id,
            self.version_id,
            self.locator,
            self.media_type,
            self.content_digest,
            self.body,
        )
    }
}

/// Split text into lowercased alphanumeric tokens.
///
/// This mirrors Tantivy's `default` tokenizer (simple + lower-casing) so query
/// terms are not interpreted as a query language and cannot inject predicates.
#[must_use]
pub fn tokenize_alnum(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() {
            current.extend(character.to_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Split a query into whitespace-delimited tokens without further splitting, so
/// identifiers containing `_` survive as one symbol.
#[must_use]
pub fn tokenize_whitespace(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|token| {
            token
                .trim_matches(|c: char| c.is_ascii_punctuation() && c != '_')
                .to_lowercase()
        })
        .filter(|token| !token.is_empty())
        .collect()
}

/// Deterministically extract code-symbol-like identifiers from text.
///
/// A token qualifies when it contains an underscore or an internal uppercase
/// transition, or when it is immediately followed by a call parenthesis.
#[must_use]
pub fn extract_symbols(text: &str) -> Vec<String> {
    let mut symbols = BTreeSet::new();
    let characters: Vec<char> = text.chars().collect();
    let mut start: Option<usize> = None;
    for (index, character) in characters.iter().enumerate() {
        let is_ident = character.is_alphanumeric() || *character == '_';
        if is_ident {
            start.get_or_insert(index);
            continue;
        }
        if let Some(begin) = start.take() {
            if let Some(symbol) = classify_symbol(&characters, begin, index) {
                symbols.insert(symbol);
            }
        }
    }
    if let Some(begin) = start {
        if let Some(symbol) = classify_symbol(&characters, begin, characters.len()) {
            symbols.insert(symbol);
        }
    }
    symbols.into_iter().collect()
}

fn classify_symbol(characters: &[char], begin: usize, end: usize) -> Option<String> {
    let token: String = characters[begin..end].iter().collect();
    if token.len() < 3 || !token.chars().next()?.is_alphabetic() {
        return None;
    }
    let snake = token.contains('_');
    let camel = token
        .char_indices()
        .any(|(index, character)| index > 0 && character.is_uppercase());
    let called = characters
        .get(end)
        .is_some_and(|character| *character == '(');
    (snake || camel || called).then(|| token.to_lowercase())
}

/// Deterministic fingerprint of a corpus, independent of input order.
#[must_use]
pub fn corpus_fingerprint(documents: &[SourceDocument]) -> String {
    let mut canonical: Vec<String> = documents.iter().map(SourceDocument::canonical).collect();
    canonical.sort();
    let mut hasher = quansio_core::Digest::of(b"quansio-indexer-corpus-v1");
    for entry in &canonical {
        hasher = quansio_core::Digest::of(format!("{hasher}\u{1e}{entry}").as_bytes());
    }
    hasher.to_string()
}
