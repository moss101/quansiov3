//! The deterministic checks a CompletionContract can require (DOMAIN.md §4.4).
//!
//! Each check reads state the trusted runtime already owns — the workspace's artifact metadata
//! and the run's EffectRecords — or fails closed naming the task that owns the missing
//! authority. A check never consults the model that made the claim.

use quansio_core::CanonicalId;
use sqlx::PgPool;

use super::{DeterministicCheck, VerificationError};
use crate::control::schema;
use crate::effects::EffectLedger;

/// What a check needs in order to run.
pub struct CheckScope<'a> {
    /// Database the metadata checks read.
    pub pool: &'a PgPool,
    /// Owning tenant; every read is scoped to it.
    pub tenant_id: &'a str,
    /// Workspace the deliverable belongs to.
    pub workspace_id: &'a str,
    /// Run whose effects are checked.
    pub run_id: &'a str,
    /// The Effect Ledger the run's records are read from.
    pub ledger: &'a EffectLedger,
}

/// The result of one check, with the owner named when the runtime cannot run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    /// The DOMAIN.md §4.4 check kind.
    pub kind: &'static str,
    /// Whether the check is satisfied.
    pub passed: bool,
    /// Human-readable detail, used as actionable feedback.
    pub detail: String,
    /// The task that owns the missing authority, when the check could not run.
    pub owner: Option<&'static str>,
}

impl CheckResult {
    fn passed(kind: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind,
            passed: true,
            detail: detail.into(),
            owner: None,
        }
    }

    fn failed(kind: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind,
            passed: false,
            detail: detail.into(),
            owner: None,
        }
    }

    fn unimplementable(kind: &'static str, owner: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind,
            passed: false,
            detail: detail.into(),
            owner: Some(owner),
        }
    }
}

/// Run one deterministic check.
///
/// # Errors
/// Returns [`VerificationError::Database`] when the metadata read fails; a check that is simply
/// unsatisfied returns `Ok(CheckResult { passed: false, .. })` so the verdict can list every
/// failure at once.
pub async fn run_check(
    check: &DeterministicCheck,
    scope: &CheckScope<'_>,
) -> Result<CheckResult, VerificationError> {
    match check {
        DeterministicCheck::ArtifactExists {
            artifact_role,
            min_versions,
        } => artifact_exists(scope, artifact_role, *min_versions).await,
        DeterministicCheck::EffectsSettled { effect_classes } => {
            effects_settled(scope, effect_classes).await
        }
        DeterministicCheck::TestCommand { command, .. } => Ok(CheckResult::unimplementable(
            check.kind(),
            super::COMMAND_CHECK_OWNER,
            format!(
                "the contract requires running `{command}` through the execution boundary, which \
                 is not wired yet"
            ),
        )),
        DeterministicCheck::Assertion { expr, .. } => Ok(CheckResult::unimplementable(
            check.kind(),
            super::PREDICATE_OWNER,
            format!("the typed predicate {expr:?} has no registered evaluator"),
        )),
        DeterministicCheck::CitationsValid { min_coverage } => Ok(CheckResult::unimplementable(
            check.kind(),
            super::CITATION_CHECK_OWNER,
            format!(
                "the contract requires citation coverage of at least {min_coverage}, which is not \
                 computable yet"
            ),
        )),
    }
}

/// `artifact_exists`: one artifact with the required role carries at least `min_versions`.
async fn artifact_exists(
    scope: &CheckScope<'_>,
    artifact_role: &str,
    min_versions: u32,
) -> Result<CheckResult, VerificationError> {
    let mut tx = scope.pool.begin().await?;
    schema::set_tenant_context(&mut tx, scope.tenant_id)
        .await
        .map_err(|error| VerificationError::Database(sqlx::Error::Protocol(error.to_string())))?;
    let highest: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(versions), 0) FROM ( \
             SELECT a.id, COUNT(v.id) AS versions \
               FROM artifacts a \
               LEFT JOIN artifact_versions v ON v.artifact_id = a.id \
              WHERE a.tenant_id = $1 AND a.workspace_id = $2 AND a.role = $3 \
              GROUP BY a.id \
         ) counted",
    )
    .bind(scope.tenant_id)
    .bind(scope.workspace_id)
    .bind(artifact_role)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let highest = u32::try_from(highest).unwrap_or(0);
    if highest >= min_versions {
        Ok(CheckResult::passed(
            "artifact_exists",
            format!("an artifact with role {artifact_role:?} has {highest} version(s)"),
        ))
    } else {
        Ok(CheckResult::failed(
            "artifact_exists",
            format!(
                "no artifact with role {artifact_role:?} has {min_versions} version(s); \
                 the best candidate has {highest}"
            ),
        ))
    }
}

/// `effects_settled`: every named class of this run has a settled record.
async fn effects_settled(
    scope: &CheckScope<'_>,
    effect_classes: &[String],
) -> Result<CheckResult, VerificationError> {
    let run_id =
        CanonicalId::parse_typed(scope.run_id, quansio_core::Prefix::Run).map_err(|_| {
            VerificationError::NotFound {
                entity: "run",
                id: scope.run_id.to_string(),
            }
        })?;
    let records = scope
        .ledger
        .list_for_run(&run_id.to_string(), None)
        .await
        .map_err(|error| VerificationError::Database(sqlx::Error::Protocol(error.to_string())))?;
    let observed: Vec<(&str, &str, bool)> = records
        .iter()
        .map(|record| {
            (
                record.effect_class.as_str(),
                record.status.as_str(),
                record.status.is_terminal(),
            )
        })
        .collect();
    Ok(effects_settled_verdict(effect_classes, &observed))
}

/// Decide `effects_settled` from the run's effect records.
///
/// A named class must have at least one settled record; with no classes named, every record the
/// run made must be settled. An unsettled record means the external outcome is not known, so the
/// work cannot be certified.
#[must_use]
pub fn effects_settled_verdict(
    effect_classes: &[String],
    observed: &[(&str, &str, bool)],
) -> CheckResult {
    if effect_classes.is_empty() {
        let unsettled: Vec<String> = observed
            .iter()
            .filter(|(_, _, settled)| !settled)
            .map(|(class, status, _)| format!("{class} ({status})"))
            .collect();
        if unsettled.is_empty() {
            return CheckResult::passed(
                "effects_settled",
                format!("all {} effect(s) of this run are settled", observed.len()),
            );
        }
        let mut unsettled = unsettled;
        unsettled.sort_unstable();
        unsettled.dedup();
        return CheckResult::failed(
            "effects_settled",
            format!("effects are not settled yet: {}", unsettled.join(", ")),
        );
    }
    let mut missing: Vec<&str> = Vec::new();
    for class in effect_classes {
        let settled = observed
            .iter()
            .any(|(record_class, _, is_settled)| *record_class == class.as_str() && *is_settled);
        if !settled {
            missing.push(class.as_str());
        }
    }
    if missing.is_empty() {
        CheckResult::passed(
            "effects_settled",
            format!(
                "every required class is settled: {}",
                effect_classes.join(", ")
            ),
        )
    } else {
        CheckResult::failed(
            "effects_settled",
            format!(
                "no settled effect of class {} for this run",
                missing.join(", ")
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unsettled_effect_keeps_work_incomplete() {
        let verdict = effects_settled_verdict(
            &[],
            &[
                ("message.send", "SETTLED_SUCCESS", true),
                ("fs.write.host", "OUTCOME_UNKNOWN", false),
            ],
        );
        assert!(!verdict.passed);
        assert!(
            verdict.detail.contains("fs.write.host"),
            "{}",
            verdict.detail
        );
    }

    #[test]
    fn a_named_class_must_be_settled_itself() {
        let classes = vec!["message.send".to_string()];
        let missing = effects_settled_verdict(&classes, &[("fs.read", "SETTLED_SUCCESS", true)]);
        assert!(!missing.passed);
        assert!(missing.detail.contains("message.send"));

        let settled =
            effects_settled_verdict(&classes, &[("message.send", "SETTLED_SUCCESS", true)]);
        assert!(settled.passed);
    }

    #[test]
    fn a_run_with_no_effects_is_settled() {
        assert!(effects_settled_verdict(&[], &[]).passed);
    }

    #[test]
    fn a_status_is_reported_as_unsettled_even_when_terminal_in_another_class() {
        let classes = vec!["fs.write.host".to_string()];
        let verdict = effects_settled_verdict(
            &classes,
            &[
                ("fs.write.host", "DENIED", true),
                ("fs.write.host", "SETTLED_SUCCESS", true),
            ],
        );
        assert!(verdict.passed, "a settled record of the class is enough");
    }
}
