//! The install report and its diagnostics (ADP-008, DOMAIN.md 11.6 C).
//!
//! One report per install attempt, keyed by the command id that asked: `{request id, pack, revision, state, phase,
//! started, finished, diagnostics[], outcome}`. Diagnostics are redacted and bounded where they enter
//! ([`Diagnostic::sanitized`]), so nothing a plane, a filesystem or a database wrote can reach the stored row or an
//! API answer unredacted.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::config::ReportsConfig;
use super::redact::{bound, redact_diagnostic};

/// Where an install is, or where it stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// Getting the content.
    Fetch,
    /// Integrity, quarantine record and credentials.
    Verify,
    /// Tools, evaluation gate and resolution.
    Compile,
    /// Moving the active revision.
    Activate,
    /// Finished.
    Done,
}

impl Phase {
    /// The durable spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::Verify => "verify",
            Self::Compile => "compile",
            Self::Activate => "activate",
            Self::Done => "done",
        }
    }

    /// Parse the durable spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Fetch,
            Self::Verify,
            Self::Compile,
            Self::Activate,
            Self::Done,
        ]
        .into_iter()
        .find(|phase| phase.as_str() == value)
    }
}

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Stops the install.
    Error,
    /// Worth knowing; does not stop it.
    Warning,
}

impl Severity {
    /// The durable spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// One finding of an install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The phase that found it.
    pub phase: Phase,
    /// A stable rule id such as `pack.tool.host_only`.
    pub rule: String,
    /// How serious it is.
    pub severity: Severity,
    /// What it says. Redacted and bounded by [`Diagnostic::sanitized`] before it is stored.
    pub message: String,
    /// Where in the content, as a JSON pointer.
    pub pointer: Option<String>,
}

impl Diagnostic {
    /// An error found in `phase`.
    #[must_use]
    pub fn error(phase: Phase, rule: &str, message: impl Into<String>) -> Self {
        Self {
            phase,
            rule: rule.to_string(),
            severity: Severity::Error,
            message: message.into(),
            pointer: None,
        }
    }

    /// A warning found in `phase`.
    #[must_use]
    pub fn warning(phase: Phase, rule: &str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(phase, rule, message)
        }
    }

    /// With a JSON pointer.
    #[must_use]
    pub fn at(mut self, pointer: impl Into<String>) -> Self {
        self.pointer = Some(pointer.into());
        self
    }

    /// The finding as it may be stored and returned: the rule id is a plain identifier, the message and pointer
    /// are redacted, then bounded.
    #[must_use]
    pub fn sanitized(&self, limits: &ReportsConfig) -> Self {
        let rule_ok = !self.rule.is_empty()
            && self.rule.len() <= 64
            && self
                .rule
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_');
        Self {
            phase: self.phase,
            rule: if rule_ok {
                self.rule.clone()
            } else {
                "pack.unknown".to_string()
            },
            severity: self.severity,
            message: bound(
                &redact_diagnostic(&self.message),
                limits.max_diagnostic_bytes,
            ),
            pointer: self
                .pointer
                .as_ref()
                .map(|pointer| bound(&redact_diagnostic(pointer), 256)),
        }
    }

    /// The stored and returned shape.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut out = json!({
            "phase": self.phase.as_str(),
            "rule": self.rule,
            "severity": self.severity.as_str(),
            "message": self.message,
        });
        if let Some(pointer) = &self.pointer {
            out["pointer"] = json!(pointer);
        }
        out
    }

    /// Read a stored diagnostic back.
    #[must_use]
    pub fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            phase: Phase::parse(value.get("phase")?.as_str()?)?,
            rule: value.get("rule")?.as_str()?.to_string(),
            severity: match value.get("severity")?.as_str()? {
                "warning" => Severity::Warning,
                _ => Severity::Error,
            },
            message: value.get("message")?.as_str()?.to_string(),
            pointer: value
                .get("pointer")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }
}

/// Sanitize a list of findings and keep at most `max_diagnostics` of them, errors first: what is returned and
/// how many were left out.
#[must_use]
pub fn sanitize_all(
    diagnostics: &[Diagnostic],
    limits: &ReportsConfig,
) -> (Vec<Diagnostic>, usize) {
    let mut ordered: Vec<&Diagnostic> = diagnostics.iter().collect();
    // Errors before warnings; the order the plane found them in is otherwise kept.
    ordered.sort_by_key(|diagnostic| diagnostic.severity == Severity::Warning);
    let kept: Vec<Diagnostic> = ordered
        .iter()
        .take(limits.max_diagnostics)
        .map(|diagnostic| diagnostic.sanitized(limits))
        .collect();
    let omitted = ordered.len().saturating_sub(kept.len());
    (kept, omitted)
}

/// Whether the attempt finished, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// In flight.
    Running,
    /// Activated.
    Succeeded,
    /// Did not activate.
    Failed,
}

impl State {
    /// The durable spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    /// Parse the durable spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// What the attempt left the pack's active revision as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The new revision is active.
    Activated,
    /// The install failed and the revision that was active still is.
    RetainedPrevious,
    /// The install failed and nothing was active before.
    Failed,
}

impl Outcome {
    /// The durable spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Activated => "activated",
            Self::RetainedPrevious => "retained_previous",
            Self::Failed => "failed",
        }
    }

    /// Parse the durable spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "activated" => Some(Self::Activated),
            "retained_previous" => Some(Self::RetainedPrevious),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// One stored install report.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallReport {
    /// The command id that asked.
    pub request_id: String,
    /// The pack's name.
    pub pack_name: String,
    /// The pack's id, once it exists.
    pub pack_id: Option<String>,
    /// The content-hash revision, once computed.
    pub revision: Option<String>,
    /// The revision that was active when the attempt began.
    pub previous_revision: Option<String>,
    /// Whether it finished.
    pub state: State,
    /// The phase it is in, or stopped at (`done` after a success).
    pub phase: Phase,
    /// How it ended; absent while running.
    pub outcome: Option<Outcome>,
    /// Redacted, bounded findings.
    pub diagnostics: Vec<Diagnostic>,
    /// Findings left out for size.
    pub diagnostics_omitted: i64,
    /// When it began.
    pub started_at: DateTime<Utc>,
    /// When it ended.
    pub finished_at: Option<DateTime<Utc>>,
    /// When it is purged.
    pub expires_at: DateTime<Utc>,
}

impl InstallReport {
    /// The API shape.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "request_id": self.request_id,
            "pack": self.pack_name,
            "pack_id": self.pack_id,
            "revision": self.revision,
            "previous_revision": self.previous_revision,
            "state": self.state.as_str(),
            "phase": self.phase.as_str(),
            "outcome": self.outcome.map(Outcome::as_str),
            "diagnostics": self.diagnostics.iter().map(Diagnostic::to_json).collect::<Vec<_>>(),
            "diagnostics_omitted": self.diagnostics_omitted,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "expires_at": self.expires_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> ReportsConfig {
        ReportsConfig {
            retention_days: 30,
            max_diagnostics: 3,
            max_diagnostic_bytes: 80,
            max_listed: 50,
        }
    }

    #[test]
    fn a_diagnostic_is_redacted_then_bounded_and_its_rule_is_an_identifier() {
        let raw = Diagnostic::error(
            Phase::Fetch,
            "Bad Rule!",
            format!("cannot read /home/alice/p.yaml {}", "x".repeat(200)),
        )
        .at("/files/a/home/bob/z");
        let clean = raw.sanitized(&limits());
        assert_eq!(clean.rule, "pack.unknown");
        assert!(clean.message.len() <= 80 && !clean.message.contains("alice"));
        assert!(!clean.pointer.expect("pointer").contains("bob"));
    }

    #[test]
    fn errors_come_first_and_the_count_left_out_is_kept() {
        let findings = vec![
            Diagnostic::warning(Phase::Compile, "pack.a", "w1"),
            Diagnostic::error(Phase::Verify, "pack.b", "e1"),
            Diagnostic::warning(Phase::Compile, "pack.c", "w2"),
            Diagnostic::error(Phase::Verify, "pack.d", "e2"),
            Diagnostic::warning(Phase::Compile, "pack.e", "w3"),
        ];
        let (kept, omitted) = sanitize_all(&findings, &limits());
        assert_eq!(omitted, 2);
        assert_eq!(
            kept.iter().map(|d| d.rule.as_str()).collect::<Vec<_>>(),
            vec!["pack.b", "pack.d", "pack.a"]
        );
    }

    #[test]
    fn a_stored_diagnostic_reads_back() {
        let finding = Diagnostic::error(Phase::Compile, "pack.tool.host_only", "no")
            .at("/contents/tool_names/1");
        assert_eq!(Diagnostic::from_json(&finding.to_json()), Some(finding));
    }
}
