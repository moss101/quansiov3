//! A pack's content, its content-hash revision and where it is fetched from (ADP-008, DOMAIN.md 11.6).
//!
//! The revision is `sha256:` and the hex digest of the canonical text of
//! `{"files": {path: text}, "manifest": {...}}`, where the manifest is taken **without its governance record**
//! (`quarantine`, `review`, `normalization`, `evaluation`, `approved`) and without `provenance.digest`: a
//! re-review changes what was recorded, not what executes, so it does not change the revision. Numbers must be
//! integers, so this function and the plane's (`python/intelligence/capability_compiler/install.py`) cannot
//! disagree about a float; both are pinned to `tests/contract/fixtures/packs/revision_corpus.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use quansio_tools::canonical_json;
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use super::config::InstallConfig;
use super::report::{Diagnostic, Phase};

/// The sections of a manifest that record its governance, not its content.
pub const GOVERNANCE: [&str; 5] = [
    "quarantine",
    "review",
    "normalization",
    "evaluation",
    "approved",
];

/// The manifest file of a pack on disk.
pub const MANIFEST_FILE: &str = "pack.yaml";

/// Why content cannot be put in canonical form.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{pointer}: {detail}")]
pub struct ContentError {
    /// Where.
    pub pointer: String,
    /// What is wrong.
    pub detail: String,
}

/// One pack's content: its manifest and its files.
#[derive(Debug, Clone, PartialEq)]
pub struct PackContent {
    /// The manifest, as JSON.
    pub manifest: Value,
    /// Relative path to text.
    pub files: BTreeMap<String, String>,
}

fn reject_floats(value: &Value, pointer: &str) -> Result<(), ContentError> {
    match value {
        Value::Number(number) if number.is_f64() => Err(ContentError {
            pointer: if pointer.is_empty() { "/" } else { pointer }.to_string(),
            detail: "numbers in a pack must be integers".to_string(),
        }),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .try_for_each(|(index, item)| reject_floats(item, &format!("{pointer}/{index}"))),
        Value::Object(map) => map
            .iter()
            .try_for_each(|(key, item)| reject_floats(item, &format!("{pointer}/{key}"))),
        _ => Ok(()),
    }
}

impl PackContent {
    /// The manifest as content: without its governance record and without `provenance.digest`.
    #[must_use]
    pub fn content_manifest(&self) -> Value {
        let Value::Object(map) = &self.manifest else {
            return self.manifest.clone();
        };
        let mut content: Map<String, Value> = map
            .iter()
            .filter(|(key, _)| !GOVERNANCE.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        if let Some(Value::Object(provenance)) = content.get("provenance").cloned() {
            let without: Map<String, Value> = provenance
                .into_iter()
                .filter(|(key, _)| key != "digest")
                .collect();
            content.insert("provenance".to_string(), Value::Object(without));
        }
        Value::Object(content)
    }

    /// The canonical text the revision is the digest of.
    ///
    /// # Errors
    /// [`ContentError`] when a number is not an integer.
    pub fn canonical(&self) -> Result<String, ContentError> {
        reject_floats(&self.manifest, "/manifest")?;
        let files: Map<String, Value> = self
            .files
            .iter()
            .map(|(path, text)| (path.clone(), Value::String(text.clone())))
            .collect();
        let mut object = Map::new();
        object.insert("files".to_string(), Value::Object(files));
        object.insert("manifest".to_string(), self.content_manifest());
        Ok(canonical_json(&Value::Object(object)))
    }

    /// The content-hash revision: `sha256:<64 hex digits>`.
    ///
    /// # Errors
    /// [`ContentError`] when a number is not an integer.
    pub fn revision(&self) -> Result<String, ContentError> {
        Ok(format!(
            "sha256:{}",
            hex::encode(Sha256::digest(self.canonical()?.as_bytes()))
        ))
    }

    /// What `provenance.digest` must name: the manifest's content without its record.
    ///
    /// # Errors
    /// [`ContentError`] when a number is not an integer.
    pub fn manifest_digest(&self) -> Result<String, ContentError> {
        reject_floats(&self.manifest, "/manifest")?;
        Ok(format!(
            "sha256:{}",
            hex::encode(Sha256::digest(
                canonical_json(&self.content_manifest()).as_bytes()
            ))
        ))
    }

    /// The pack's `name`, when the manifest has a string one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.manifest.get("name").and_then(Value::as_str)
    }

    /// The pack's `semver`, when the manifest has a string one.
    #[must_use]
    pub fn semver(&self) -> Option<&str> {
        self.manifest.get("semver").and_then(Value::as_str)
    }

    /// Bytes the content holds: the manifest's JSON and every file's text.
    #[must_use]
    pub fn size(&self) -> usize {
        self.manifest.to_string().len() + self.files.values().map(String::len).sum::<usize>()
    }
}

/// Where a pack's content comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum PackSource {
    /// Sent with the command.
    Inline(PackContent),
    /// A pack the repository ships, by name (`packs/capabilities/<name>`).
    Builtin(String),
}

impl PackSource {
    /// A name for the report before the manifest has been read.
    #[must_use]
    pub fn name_hint(&self) -> String {
        match self {
            Self::Inline(content) => content
                .name()
                .map_or_else(|| "(unnamed)".to_string(), report_name),
            Self::Builtin(name) => report_name(name),
        }
    }
}

fn fetch_failure(rule: &str, message: impl Into<String>) -> Vec<Diagnostic> {
    vec![Diagnostic::error(Phase::Fetch, rule, message)]
}

/// What a report calls a pack: its name when that is a plain name, and a fixed word otherwise, so text a person
/// chose for a name that is not one never reaches a stored report.
#[must_use]
pub fn report_name(name: &str) -> String {
    if valid_name(name) {
        name.to_string()
    } else {
        "(invalid name)".to_string()
    }
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Fetch a pack's content from its source, within the configured bounds. Nothing is trusted yet: the
/// plane verifies and compiles what comes back.
///
/// # Errors
/// The `fetch`-phase diagnostics that say why the content could not be had.
pub fn fetch(
    source: &PackSource,
    config: &InstallConfig,
    builtin_root: &Path,
) -> Result<PackContent, Vec<Diagnostic>> {
    let content = match source {
        PackSource::Inline(content) => content.clone(),
        PackSource::Builtin(name) => read_builtin(name, builtin_root, config)?,
    };
    if content.files.len() > config.max_files {
        return Err(fetch_failure(
            "pack.fetch.bounds",
            format!("a pack holds at most {} files", config.max_files),
        ));
    }
    if let Some((path, _)) = content
        .files
        .iter()
        .find(|(_, text)| text.len() > config.max_file_bytes)
    {
        return Err(fetch_failure(
            "pack.fetch.bounds",
            format!("{path} is over {} bytes", config.max_file_bytes),
        ));
    }
    if content.size() > config.max_content_bytes {
        return Err(fetch_failure(
            "pack.fetch.bounds",
            format!("a pack is at most {} bytes", config.max_content_bytes),
        ));
    }
    Ok(content)
}

fn read_builtin(
    name: &str,
    root: &Path,
    config: &InstallConfig,
) -> Result<PackContent, Vec<Diagnostic>> {
    if !valid_name(name) {
        return Err(fetch_failure(
            "pack.fetch.source",
            "a built-in pack is named by lowercase letters, digits, - or _",
        ));
    }
    let directory = root.join(name);
    let manifest_path = directory.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&manifest_path).map_err(|error| {
        fetch_failure(
            "pack.fetch.source",
            format!("cannot read {}: {error}", manifest_path.display()),
        )
    })?;
    let manifest: Value = serde_yaml::from_str(&text).map_err(|error| {
        fetch_failure(
            "pack.fetch.source",
            format!(
                "{} is not a pack manifest: {error}",
                manifest_path.display()
            ),
        )
    })?;
    let mut files = BTreeMap::new();
    let mut total = 0_usize;
    let mut pending: Vec<PathBuf> = vec![directory.clone()];
    while let Some(dir) = pending.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|error| {
            fetch_failure(
                "pack.fetch.source",
                format!("cannot list {}: {error}", dir.display()),
            )
        })?;
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).map_err(|error| {
                fetch_failure(
                    "pack.fetch.source",
                    format!("cannot stat {}: {error}", path.display()),
                )
            })?;
            if meta.file_type().is_symlink() {
                return Err(fetch_failure(
                    "pack.fetch.source",
                    format!(
                        "{} is a link; a pack holds regular files only",
                        path.display()
                    ),
                ));
            }
            if meta.is_dir() {
                pending.push(path);
                continue;
            }
            if path == manifest_path {
                continue;
            }
            let relative = path.strip_prefix(&directory).map_or_else(
                |_| String::new(),
                |rel| rel.to_string_lossy().replace('\\', "/"),
            );
            let size = usize::try_from(meta.len()).unwrap_or(usize::MAX);
            total = total.saturating_add(size);
            if files.len() >= config.max_files
                || size > config.max_file_bytes
                || total > config.max_content_bytes
            {
                return Err(fetch_failure(
                    "pack.fetch.bounds",
                    format!("the pack on disk is over its bounds at {relative}"),
                ));
            }
            let body = std::fs::read_to_string(&path).map_err(|error| {
                fetch_failure(
                    "pack.fetch.source",
                    format!("cannot read {} as text: {error}", path.display()),
                )
            })?;
            files.insert(relative, body);
        }
    }
    Ok(PackContent { manifest, files })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn content(manifest: Value, files: &[(&str, &str)]) -> PackContent {
        PackContent {
            manifest,
            files: files
                .iter()
                .map(|(path, text)| ((*path).to_string(), (*text).to_string()))
                .collect(),
        }
    }

    #[test]
    fn a_re_review_does_not_change_the_revision_and_a_change_to_what_executes_does() {
        let base = json!({
            "name": "p", "semver": "1.0.0",
            "provenance": {"digest": "sha256:aa", "ref": "x"},
            "review": {"outcome": "approved", "reviewed_at": "t1"},
            "contents": {"tool_names": ["fs.read"]},
        });
        let a = content(base.clone(), &[("a.txt", "x")]);
        let mut reviewed = base.clone();
        reviewed["review"]["reviewed_at"] = json!("t2");
        reviewed["provenance"]["digest"] = json!("sha256:bb");
        assert_eq!(
            a.revision().unwrap(),
            content(reviewed, &[("a.txt", "x")]).revision().unwrap()
        );
        let mut edited = base;
        edited["contents"]["tool_names"] = json!(["fs.read", "fs.write"]);
        assert_ne!(
            a.revision().unwrap(),
            content(edited, &[("a.txt", "x")]).revision().unwrap()
        );
        assert_ne!(
            a.revision().unwrap(),
            content(a.manifest.clone(), &[("a.txt", "y")])
                .revision()
                .unwrap()
        );
    }

    #[test]
    fn a_float_is_refused_because_two_implementations_would_disagree() {
        let pack = content(json!({"name": "p", "contents": {"n": [1.5]}}), &[]);
        let error = pack.revision().unwrap_err();
        assert_eq!(error.pointer, "/manifest/contents/n/0");
        assert!(content(json!({"n": 3}), &[]).revision().is_ok());
    }

    #[test]
    fn names_for_a_built_in_pack_cannot_climb_out_of_the_root() {
        for bad in ["../x", "a/b", "", "A", "-x", ".hidden", &"a".repeat(65)] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(valid_name("business-ops") && valid_name("cloud_devops"));
    }
}
