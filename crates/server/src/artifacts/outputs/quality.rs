//! The quality record an output version carries (ADP-006, DOMAIN.md §10.4 A).
//!
//! The record is built here, by the authority, from checks the authority ran. The shape is the
//! one DOMAIN.md names: `{record_version, adapter, adapter_version, ok, inputs[], checks[],
//! environment?, trust, evidence_ids[]}`. `ok` is derived, never set: a record whose checks fail
//! cannot say it passed.

use serde_json::{json, Map, Value};

/// One check: the rule that ran, whether it held, and what it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// `output.<adapter>.<check>`.
    pub rule: String,
    /// Whether the check held.
    pub ok: bool,
    /// What it found, bounded.
    pub detail: String,
    /// What it was about (`slide 3`), when that helps.
    pub subject: Option<String>,
}

const MAX_DETAIL: usize = 400;

fn bounded(text: impl Into<String>) -> String {
    let text = text.into();
    if text.chars().count() <= MAX_DETAIL {
        text
    } else {
        let mut cut: String = text.chars().take(MAX_DETAIL - 1).collect();
        cut.push('…');
        cut
    }
}

impl Check {
    /// A check that held.
    #[must_use]
    pub fn passed(rule: &str, detail: impl Into<String>) -> Self {
        Self {
            rule: rule.to_string(),
            ok: true,
            detail: bounded(detail),
            subject: None,
        }
    }

    /// A check that did not.
    #[must_use]
    pub fn failed(rule: &str, detail: impl Into<String>) -> Self {
        Self {
            rule: rule.to_string(),
            ok: false,
            detail: bounded(detail),
            subject: None,
        }
    }

    /// A check with the result decided by a condition.
    #[must_use]
    pub fn of(rule: &str, ok: bool, detail: impl Into<String>) -> Self {
        Self {
            rule: rule.to_string(),
            ok,
            detail: bounded(detail),
            subject: None,
        }
    }

    /// Name what the check was about.
    #[must_use]
    pub fn about(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// The JSON the record stores.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut value = json!({ "rule": self.rule, "ok": self.ok, "detail": self.detail });
        if let Some(subject) = &self.subject {
            value["subject"] = Value::String(subject.clone());
        }
        value
    }
}

/// How far the content of an output may be trusted (DOMAIN.md §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// A deterministic transformation of workspace sources.
    AgentGenerated,
    /// Content that came from outside: a transcript, a rendered page, a suggestion.
    UntrustedExternal,
}

impl Trust {
    /// The label as the trust classifier spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentGenerated => "AGENT_GENERATED",
            Self::UntrustedExternal => "UNTRUSTED_EXTERNAL",
        }
    }
}

/// The version an output was made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    /// The artifact.
    pub artifact_id: String,
    /// The version of it that was read.
    pub version_id: String,
    /// Its content digest.
    pub content_digest: String,
}

/// A quality record, before it is written.
#[derive(Debug, Clone, PartialEq)]
pub struct QualityRecord {
    /// The adapter (`deck.render`, `deck.export`, …).
    pub adapter: String,
    /// The adapter's version.
    pub adapter_version: String,
    /// What it was made from.
    pub inputs: Vec<Input>,
    /// What it checked.
    pub checks: Vec<Check>,
    /// What laid the output out or produced it (`browser`, `ffmpeg`, `provider`).
    pub environment: Map<String, Value>,
    /// How far to trust the content.
    pub trust: Trust,
    /// Evidence rows the checks produced.
    pub evidence_ids: Vec<String>,
}

impl QualityRecord {
    /// A record with no checks yet.
    #[must_use]
    pub fn new(adapter: &str, trust: Trust) -> Self {
        Self {
            adapter: adapter.to_string(),
            adapter_version: "v1".to_string(),
            inputs: Vec::new(),
            checks: Vec::new(),
            environment: Map::new(),
            trust,
            evidence_ids: Vec::new(),
        }
    }

    /// Whether every check held. A record with no checks does not.
    #[must_use]
    pub fn ok(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|check| check.ok)
    }

    /// The checks that did not hold.
    #[must_use]
    pub fn failures(&self) -> Vec<&Check> {
        self.checks.iter().filter(|check| !check.ok).collect()
    }

    /// The JSON stored in `artifact_versions.quality_record`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut value = json!({
            "record_version": 1,
            "adapter": self.adapter,
            "adapter_version": self.adapter_version,
            "ok": self.ok(),
            "inputs": self.inputs.iter().map(|input| json!({
                "artifact_id": input.artifact_id,
                "version_id": input.version_id,
                "content_digest": input.content_digest,
            })).collect::<Vec<_>>(),
            "checks": self.checks.iter().map(Check::to_json).collect::<Vec<_>>(),
            "trust": self.trust.as_str(),
            "evidence_ids": self.evidence_ids,
        });
        if !self.environment.is_empty() {
            value["environment"] = Value::Object(self.environment.clone());
        }
        value
    }

    /// The first failing check, as one line for a refusal.
    #[must_use]
    pub fn first_failure(&self) -> Option<String> {
        self.failures()
            .first()
            .map(|check| format!("{}: {}", check.rule, check.detail))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_is_derived_from_the_checks_and_an_empty_record_is_not_ok() {
        let mut record = QualityRecord::new("deck.export", Trust::AgentGenerated);
        assert!(!record.ok(), "a record that checked nothing proves nothing");
        record
            .checks
            .push(Check::passed("output.deck.pdf.pages", "3 pages"));
        assert!(record.ok());
        record
            .checks
            .push(Check::failed("output.deck.pdf.structure", "no EOF"));
        assert!(!record.ok());
        assert_eq!(record.failures().len(), 1);
        assert_eq!(
            record.first_failure().as_deref(),
            Some("output.deck.pdf.structure: no EOF")
        );
        let json = record.to_json();
        assert_eq!(json["ok"], false);
        assert_eq!(json["record_version"], 1);
        assert_eq!(json["trust"], "AGENT_GENERATED");
        assert_eq!(json["checks"].as_array().map(Vec::len), Some(2));
        assert!(json.get("environment").is_none());
    }

    #[test]
    fn a_detail_is_bounded_and_a_subject_is_kept() {
        let long = "x".repeat(2000);
        let check = Check::failed("output.x.y", long).about("slide 4");
        assert!(check.detail.chars().count() <= MAX_DETAIL);
        assert!(check.detail.ends_with('…'));
        assert_eq!(check.to_json()["subject"], "slide 4");
    }

    #[test]
    fn the_environment_is_recorded_when_there_is_one() {
        let mut record = QualityRecord::new("deck.render", Trust::UntrustedExternal);
        record
            .environment
            .insert("browser".into(), json!("HeadlessChrome/154"));
        record.checks.push(Check::passed("a", "b"));
        let json = record.to_json();
        assert_eq!(json["environment"]["browser"], "HeadlessChrome/154");
        assert_eq!(json["trust"], "UNTRUSTED_EXTERNAL");
    }
}
