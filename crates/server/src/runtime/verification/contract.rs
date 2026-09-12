//! The CompletionContract (DOMAIN.md §4.4) and its fail-closed parsing.
//!
//! A contract is the *only* thing that can make a completion claim true. Parsing is strict:
//! an unknown check kind, a missing bound, or a contract that binds nothing at all is refused
//! rather than skipped, because a skipped check is exactly how a model would certify itself.

use serde_json::Value;

use super::VerificationError;

/// One deterministic check the contract requires.
#[derive(Debug, Clone, PartialEq)]
pub enum DeterministicCheck {
    /// An artifact with the given role and at least `min_versions` versions exists.
    ArtifactExists {
        /// Artifact role the deliverable carries.
        artifact_role: String,
        /// Minimum version count on one artifact of that role.
        min_versions: u32,
    },
    /// A command run through the execution boundary exits with `expect_exit`.
    TestCommand {
        /// Command the contract requires.
        command: String,
        /// Expected exit code.
        expect_exit: i32,
    },
    /// A typed predicate holds.
    Assertion {
        /// Predicate id.
        expr: String,
        /// Predicate arguments.
        args: Value,
    },
    /// Every named effect class has a settled EffectRecord for this run.
    EffectsSettled {
        /// Effect classes that must be settled; empty means every effect of the run.
        effect_classes: Vec<String>,
    },
    /// The run's citations cover at least `min_coverage` of its claims.
    CitationsValid {
        /// Required citation coverage in `0.0..=1.0`.
        min_coverage: f64,
    },
}

impl DeterministicCheck {
    /// The DOMAIN.md §4.4 spelling of this check.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::ArtifactExists { .. } => "artifact_exists",
            Self::TestCommand { .. } => "test_command",
            Self::Assertion { .. } => "assertion",
            Self::EffectsSettled { .. } => "effects_settled",
            Self::CitationsValid { .. } => "citations_valid",
        }
    }

    /// The task that owns executing this check kind, when the runtime cannot do so yet.
    ///
    /// `None` means the check is implementable from state the runtime already owns.
    #[must_use]
    pub const fn owner(&self) -> Option<&'static str> {
        match self {
            Self::ArtifactExists { .. } | Self::EffectsSettled { .. } => None,
            // Running a contract command is the tool host's job (DOMAIN.md §7.5 `terminal.exec`).
            Self::TestCommand { .. } => Some(super::COMMAND_CHECK_OWNER),
            // Cite coverage is computed by the research/citation pipeline.
            Self::CitationsValid { .. } => Some(super::CITATION_CHECK_OWNER),
            // A typed predicate needs the predicate registry; the check is unusable without it.
            Self::Assertion { .. } => Some(super::PREDICATE_OWNER),
        }
    }
}

/// The optional independent semantic verification (DOMAIN.md §4.4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SemanticVerification {
    /// Whether the contract requires a semantic verdict.
    pub required: bool,
    /// Rubric the independent verifier applies.
    pub rubric_id: Option<String>,
    /// Whether the verdict must come from a model other than the one that did the work.
    pub independent_model: bool,
}

/// A WorkNode's CompletionContract.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionContract {
    /// Deterministic checks, in declaration order.
    pub deterministic_checks: Vec<DeterministicCheck>,
    /// Optional semantic verification.
    pub semantic_verification: SemanticVerification,
    /// Whether a human must sign off before the work is done.
    pub human_signoff_required: bool,
}

impl CompletionContract {
    /// Parse a contract from a WorkNode's stored JSON.
    ///
    /// # Errors
    /// Returns [`VerificationError::MalformedContract`] for a structurally invalid contract
    /// and [`VerificationError::UnknownCheck`] for a check kind this runtime cannot bind.
    pub fn parse(value: &Value) -> Result<Self, VerificationError> {
        let malformed = |detail: &str| VerificationError::MalformedContract {
            detail: detail.to_string(),
        };
        let Some(object) = value.as_object() else {
            return Err(malformed("a CompletionContract must be a JSON object"));
        };
        let checks = match object.get("deterministic_checks") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(parse_check)
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => return Err(malformed("deterministic_checks must be an array")),
        };
        let semantic = match object.get("semantic_verification") {
            None | Some(Value::Null) => SemanticVerification::default(),
            Some(Value::Object(fields)) => SemanticVerification {
                required: fields
                    .get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                rubric_id: fields
                    .get("rubric_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                independent_model: fields
                    .get("independent_model")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
            Some(_) => return Err(malformed("semantic_verification must be an object")),
        };
        Ok(Self {
            deterministic_checks: checks,
            semantic_verification: semantic,
            human_signoff_required: object
                .get("human_signoff_required")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    /// Whether the contract binds nothing and therefore can certify nothing.
    #[must_use]
    pub fn binds_nothing(&self) -> bool {
        self.deterministic_checks.is_empty()
            && !self.semantic_verification.required
            && !self.human_signoff_required
    }

    /// Whether every check is implementable by the runtime it was loaded into.
    #[must_use]
    pub fn unimplementable_checks(&self) -> Vec<(&'static str, &'static str)> {
        self.deterministic_checks
            .iter()
            .filter_map(|check| check.owner().map(|owner| (check.kind(), owner)))
            .collect()
    }
}

fn parse_check(value: &Value) -> Result<DeterministicCheck, VerificationError> {
    let malformed = |detail: String| VerificationError::MalformedContract { detail };
    let Some(object) = value.as_object() else {
        return Err(malformed(
            "a deterministic check must be an object".to_string(),
        ));
    };
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed("a deterministic check needs a kind".to_string()))?;
    match kind {
        "artifact_exists" => {
            let role = object
                .get("artifact_role")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed("artifact_exists needs artifact_role".to_string()))?;
            Ok(DeterministicCheck::ArtifactExists {
                artifact_role: role.to_string(),
                min_versions: u32::try_from(
                    object
                        .get("min_versions")
                        .and_then(Value::as_u64)
                        .unwrap_or(1),
                )
                .unwrap_or(1)
                .max(1),
            })
        }
        "test_command" => {
            let command = object
                .get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed("test_command needs a command".to_string()))?;
            Ok(DeterministicCheck::TestCommand {
                command: command.to_string(),
                expect_exit: i32::try_from(
                    object
                        .get("expect_exit")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                )
                .unwrap_or(0),
            })
        }
        "assertion" => {
            let expr = object
                .get("expr")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed("assertion needs an expr".to_string()))?;
            Ok(DeterministicCheck::Assertion {
                expr: expr.to_string(),
                args: object.get("args").cloned().unwrap_or(Value::Null),
            })
        }
        "effects_settled" => Ok(DeterministicCheck::EffectsSettled {
            effect_classes: object
                .get("effect_classes")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        }),
        "citations_valid" => {
            let coverage = object
                .get("min_coverage")
                .and_then(Value::as_f64)
                .ok_or_else(|| malformed("citations_valid needs min_coverage".to_string()))?;
            if !(0.0..=1.0).contains(&coverage) {
                return Err(malformed(
                    "citations_valid min_coverage must be between 0 and 1".to_string(),
                ));
            }
            Ok(DeterministicCheck::CitationsValid {
                min_coverage: coverage,
            })
        }
        other => Err(VerificationError::UnknownCheck {
            kind: other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::super::VerificationError;
    use super::*;
    use serde_json::json;

    #[test]
    fn every_documented_check_kind_parses() {
        let contract = CompletionContract::parse(&json!({
            "deterministic_checks": [
                {"kind": "artifact_exists", "artifact_role": "report", "min_versions": 2},
                {"kind": "test_command", "command": "cargo test -p x", "expect_exit": 0},
                {"kind": "assertion", "expr": "no_open_questions", "args": {}},
                {"kind": "effects_settled", "effect_classes": ["message.send"]},
                {"kind": "citations_valid", "min_coverage": 0.95}
            ],
            "semantic_verification": {"required": true, "rubric_id": "r1", "independent_model": true},
            "human_signoff_required": false
        }))
        .expect("contract");
        assert_eq!(contract.deterministic_checks.len(), 5);
        assert!(contract.semantic_verification.required);
        assert_eq!(contract.deterministic_checks[0].kind(), "artifact_exists");
        assert_eq!(contract.deterministic_checks[0].owner(), None);
        assert_eq!(contract.deterministic_checks[1].owner(), Some("EXEC-006"));
    }

    #[test]
    fn an_unknown_check_kind_is_refused_not_skipped() {
        let error = CompletionContract::parse(&json!({
            "deterministic_checks": [{"kind": "vibes_check"}]
        }))
        .expect_err("unknown kind");
        assert!(matches!(error, VerificationError::UnknownCheck { .. }));
        assert_eq!(error.code(), "VALIDATION_SCHEMA");
    }

    #[test]
    fn a_contract_that_binds_nothing_is_recognised() {
        let contract = CompletionContract::parse(&json!({})).expect("empty contract");
        assert!(contract.binds_nothing());
        let contract = CompletionContract::parse(&json!({
            "deterministic_checks": [{"kind": "artifact_exists", "artifact_role": "report"}]
        }))
        .expect("contract");
        assert!(!contract.binds_nothing());
        assert_eq!(
            contract.deterministic_checks[0],
            DeterministicCheck::ArtifactExists {
                artifact_role: "report".to_string(),
                min_versions: 1
            }
        );
    }

    #[test]
    fn a_malformed_bound_is_refused() {
        let error = CompletionContract::parse(&json!({
            "deterministic_checks": [{"kind": "citations_valid", "min_coverage": 2.0}]
        }))
        .expect_err("coverage out of range");
        assert!(matches!(error, VerificationError::MalformedContract { .. }));
        let error = CompletionContract::parse(&json!({
            "deterministic_checks": [{"kind": "artifact_exists"}]
        }))
        .expect_err("missing role");
        assert!(matches!(error, VerificationError::MalformedContract { .. }));
    }
}
