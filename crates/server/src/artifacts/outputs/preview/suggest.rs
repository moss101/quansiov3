//! Visual-edit suggestions from a preview (DOMAIN.md §10.4 F).
//!
//! A page in the preview can suggest an edit (`POST /p/<token>/__quansio/suggest`). A suggestion is
//! **data from a page**: it is bounded, stored as text in the scope's suggestions Artifact and
//! returned labelled `UNTRUSTED_EXTERNAL`. Nothing in one is read as an instruction, applied to a
//! file or turned into a tool call; the agent decides what, if anything, to do about it.

use serde::{Deserialize, Serialize};

use crate::artifacts::outputs::config::Preview as Limits;

/// The media type of the suggestions Artifact.
pub const MEDIA_TYPE: &str = "application/json";
/// The role prefix: `preview_suggestions:<scope id>`.
pub const ROLE: &str = "preview_suggestions";
/// The trust label inside the Artifact and in results.
pub const TRUST: &str = "UNTRUSTED_EXTERNAL";
/// The adapter that writes the suggestions Artifact.
pub const ADAPTER: &str = "webdev.preview";

/// The role of the Artifact that holds a scope's suggestions.
#[must_use]
pub fn role_of(scope_id: &str) -> String {
    format!("{ROLE}:{scope_id}")
}

/// What a suggestion is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Different text.
    Text,
    /// Different styling.
    Style,
    /// A remark.
    Comment,
}

/// A suggestion as a page sends it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suggestion {
    /// What it is about.
    pub kind: Kind,
    /// Where in the page.
    pub selector: String,
    /// What is there now.
    #[serde(default)]
    pub before: Option<String>,
    /// What it should be.
    #[serde(default)]
    pub after: Option<String>,
    /// A remark.
    #[serde(default)]
    pub note: Option<String>,
}

fn plain(text: &str, longest: usize) -> bool {
    text.chars().count() <= longest
        && !text.chars().any(|c| {
            (c.is_control() && c != '\n' && c != '\t') || c == '\u{2028}' || c == '\u{2029}'
        })
}

impl Suggestion {
    /// Check the bounds.
    ///
    /// # Errors
    /// A sentence naming what is wrong.
    pub fn validate(&self) -> Result<(), String> {
        if self.selector.trim().is_empty() || !plain(&self.selector, 512) {
            return Err("selector is 1 to 512 plain characters".to_string());
        }
        for (name, value) in [
            ("before", &self.before),
            ("after", &self.after),
            ("note", &self.note),
        ] {
            if let Some(value) = value {
                if !plain(value, 2048) {
                    return Err(format!("{name} is at most 2048 plain characters"));
                }
            }
        }
        if self.before.is_none() && self.after.is_none() && self.note.is_none() {
            return Err("a suggestion says something: before, after or note".to_string());
        }
        Ok(())
    }
}

/// A suggestion as it is stored and returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stored {
    /// Its position, from 0.
    pub index: u64,
    /// When it was made (RFC 3339).
    pub at: String,
    /// What it is about.
    pub kind: Kind,
    /// Where in the page.
    pub selector: String,
    /// What was there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// What it should be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// A remark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A scope's suggestions Artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Log {
    /// Always `v1`.
    pub schema_version: String,
    /// Always `UNTRUSTED_EXTERNAL`.
    pub trust: String,
    /// The scope.
    pub scope_id: String,
    /// The suggestions, in the order they were made.
    pub suggestions: Vec<Stored>,
}

impl Log {
    /// An empty log.
    #[must_use]
    pub fn new(scope_id: &str) -> Self {
        Self {
            schema_version: "v1".to_string(),
            trust: TRUST.to_string(),
            scope_id: scope_id.to_string(),
            suggestions: Vec::new(),
        }
    }

    /// Parse and validate a stored log.
    ///
    /// # Errors
    /// A sentence naming what is wrong.
    pub fn parse(bytes: &[u8], limits: &Limits) -> Result<Self, String> {
        let log: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if log.schema_version != "v1" || log.trust != TRUST {
            return Err(format!("a suggestions log is v1 and labelled {TRUST}"));
        }
        if log.suggestions.len() > limits.max_suggestions {
            return Err(format!(
                "a scope holds at most {} suggestions",
                limits.max_suggestions
            ));
        }
        for (position, stored) in log.suggestions.iter().enumerate() {
            if stored.index != position as u64 {
                return Err(format!(
                    "suggestion {position} is numbered {}",
                    stored.index
                ));
            }
        }
        Ok(log)
    }

    /// The canonical bytes a version stores.
    ///
    /// # Panics
    /// Never: the structure serializes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a suggestions log serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::outputs::config::OutputsConfig;

    fn limits() -> Limits {
        OutputsConfig::shipped().preview
    }

    fn suggestion() -> Suggestion {
        Suggestion {
            kind: Kind::Text,
            selector: "h1.title".to_string(),
            before: Some("Hello".to_string()),
            after: Some("Welcome".to_string()),
            note: None,
        }
    }

    #[test]
    fn a_plain_suggestion_validates() {
        assert!(suggestion().validate().is_ok());
    }

    #[test]
    fn every_bound_is_checked() {
        let mut empty_selector = suggestion();
        empty_selector.selector = "  ".to_string();
        let mut long_selector = suggestion();
        long_selector.selector = "a".repeat(513);
        let mut long_note = suggestion();
        long_note.note = Some("x".repeat(2049));
        let mut control = suggestion();
        control.after = Some("a\u{7}b".to_string());
        let mut says_nothing = suggestion();
        says_nothing.before = None;
        says_nothing.after = None;
        for (label, broken) in [
            ("empty selector", empty_selector),
            ("long selector", long_selector),
            ("long note", long_note),
            ("control character", control),
            ("says nothing", says_nothing),
        ] {
            assert!(broken.validate().is_err(), "{label}");
        }
    }

    #[test]
    fn an_unknown_field_or_kind_is_refused() {
        assert!(serde_json::from_str::<Suggestion>(
            r#"{"kind":"text","selector":"a","after":"b","run":"rm -rf"}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Suggestion>(
            r#"{"kind":"command","selector":"a","note":"b"}"#
        )
        .is_err());
    }

    #[test]
    fn a_log_round_trips_and_checks_its_numbering() {
        let mut log = Log::new("pvs_0123456789abcdef");
        log.suggestions.push(Stored {
            index: 0,
            at: "2026-01-01T00:00:00Z".to_string(),
            kind: Kind::Comment,
            selector: "p".to_string(),
            before: None,
            after: None,
            note: Some("tighten this".to_string()),
        });
        assert_eq!(Log::parse(&log.to_bytes(), &limits()).expect("parse"), log);
        log.suggestions[0].index = 3;
        assert!(Log::parse(&log.to_bytes(), &limits()).is_err());
    }
}
