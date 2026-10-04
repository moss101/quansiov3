//! From a finished recording to a `DRAFT` skill (ADP-003, DOMAIN.md §11.7 D, E).
//!
//! Compilation is the plane's proposal and the server's decision. [`RecordingCompiler::compile`] reads the
//! archive back out of the artifact store (digest-verified), validates it again, hands the plane only the
//! text-only projection (no pixel, no screen point) and the tools the registry offers, and keeps the plane's
//! proposal on the recording row only after [`super::proposal::validate`] accepted it.
//! [`RecordingCompiler::submit`] turns the person's decisions into the manifest and registers one `DRAFT`
//! version through the Skill Registry's own store: nothing here can create any other state.

use std::time::Duration;

use quansio_core::CorrelationId;
use quansio_events::Actor;
use quansio_tools::ToolRegistry;
use serde_json::{json, Value};
use sqlx::PgPool;

use super::archive::{self, Expected};
use super::config::RecordingConfig;
use super::proposal::{self, SlotChoice};
use super::store::{Recording, RecordingCaller, RecordingStatus, RecordingStore};
use super::RecordingError;
use crate::artifacts::{ArtifactStore, ArtifactVersionId, ObjectStore};
use crate::control::skills::{
    NewSkill, NewSkillVersion, SkillAdmin, SkillScope, SkillStatus, SkillStore, SkillVersion,
};
use crate::intelligence::{CallScope, IntelligenceClient};

/// What a compile produced.
#[derive(Debug, Clone, PartialEq)]
pub struct CompileOutcome {
    /// The recording, now `compiled`.
    pub recording: Recording,
    /// The proposal the plane made and the server accepted.
    pub proposal: Value,
}

/// The person's decision for one slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotDecision {
    /// The slot.
    pub slot_id: String,
    /// `input` or `constant`.
    pub classification: String,
}

/// What a submission produced.
#[derive(Debug, Clone, PartialEq)]
pub struct SubmitOutcome {
    /// The recording, now `submitted`.
    pub recording: Recording,
    /// The skill.
    pub skill_id: String,
    /// Its `DRAFT` version.
    pub version: SkillVersion,
}

const SKILL_NAME_MAX: usize = 64;

/// Compiles and submits recordings.
#[derive(Debug, Clone)]
pub struct RecordingCompiler {
    store: RecordingStore,
    client: Option<IntelligenceClient>,
    deadline: Duration,
    registry: &'static ToolRegistry,
    config: &'static RecordingConfig,
}

impl RecordingCompiler {
    /// A compiler over `pool`. Without a `client` there is no plane to ask and a compile is refused.
    #[must_use]
    pub fn new(pool: PgPool, client: Option<IntelligenceClient>, deadline: Duration) -> Self {
        Self {
            store: RecordingStore::new(pool),
            client,
            deadline,
            registry: ToolRegistry::builtin(),
            config: RecordingConfig::committed(),
        }
    }

    /// The session store this compiler works through.
    #[must_use]
    pub fn store(&self) -> &RecordingStore {
        &self.store
    }

    /// Compile a finished recording into a proposal under review.
    ///
    /// # Errors
    /// The fence and state refusals, a plane failure, or [`RecordingError::ProposalRejected`].
    pub async fn compile(
        &self,
        caller: &RecordingCaller,
        objects: &dyn ObjectStore,
        recording_id: &str,
    ) -> Result<CompileOutcome, RecordingError> {
        let Some(client) = &self.client else {
            return Err(RecordingError::Plane(crate::intelligence::IntelligenceError::Unreachable(
                "compiling a recording needs the intelligence plane (QUANSIO_INTELLIGENCE_ENDPOINT)".to_string(),
            )));
        };
        let row = self
            .store
            .require_finished(
                caller,
                recording_id,
                &[RecordingStatus::Finished, RecordingStatus::Compiled],
            )
            .await?;
        let (Some(version_id), Some(_)) = (&row.artifact_version_id, &row.content_digest) else {
            return Err(RecordingError::Malformed(
                "a finished recording lost its archive".to_string(),
            ));
        };
        let version = ArtifactVersionId::parse(version_id)?;
        let mut tx = self.store.pool().begin().await?;
        let bytes = ArtifactStore::new(objects)
            .fetch_bytes(&mut tx, &caller.tenant_id, &caller.user_id, &version)
            .await?;
        tx.commit().await?;
        // The archive is checked again on its way out: what is sent is what passes the same gate that
        // stored it, whatever happened to the bytes in between.
        let report = archive::validate(
            &bytes,
            &Expected {
                recording_id: &row.id,
                tenant_id: &caller.tenant_id,
                user_id: &caller.user_id,
                epoch: row.epoch,
            },
            self.config,
        )?;
        let tools = proposal::tool_summaries(self.registry);
        let scope = CallScope {
            tenant_id: caller.tenant_id.clone(),
            workspace_id: row.workspace_id.clone(),
            correlation_id: caller.correlation_id.to_string(),
            deadline: self.deadline,
            capability_projection_id: "rbac:author_work".to_string(),
        };
        let mut client = client.clone();
        let raw = client
            .compile_skill_recording(
                &scope,
                &row.id,
                report.projection.to_string(),
                tools
                    .iter()
                    .map(|tool| tool.to_json().to_string())
                    .collect(),
                self.config.settings_json().to_string(),
            )
            .await?;
        let proposal: Value =
            serde_json::from_str(&raw).map_err(|error| RecordingError::ProposalRejected {
                rule: "proposal.shape",
                detail: format!("the plane returned something that is not JSON: {error}"),
            })?;
        proposal::validate(&proposal, &row.id, &tools, self.config)?;
        let recording = self
            .store
            .record_proposal(caller, &row.id, &proposal)
            .await?;
        Ok(CompileOutcome {
            recording,
            proposal,
        })
    }

    /// Register the person's reviewed proposal as one `DRAFT` skill version.
    ///
    /// Idempotent: a skill this recording already produced (a submission that stopped between the
    /// registry write and the row update) is adopted rather than refused.
    ///
    /// # Errors
    /// The fence and state refusals, [`RecordingError::Invalid`] for an undecided or unsound slot or a bad
    /// name, and a registry refusal.
    pub async fn submit(
        &self,
        caller: &RecordingCaller,
        recording_id: &str,
        name: &str,
        semver: &str,
        decisions: &[SlotDecision],
    ) -> Result<SubmitOutcome, RecordingError> {
        validate_name(name)?;
        validate_semver(semver)?;
        let skills = SkillStore::new(self.store.pool().clone());
        let row = self
            .store
            .require_finished(
                caller,
                recording_id,
                &[RecordingStatus::Compiled, RecordingStatus::Submitted],
            )
            .await?;
        if row.status == RecordingStatus::Submitted {
            // A repeated submission answers with what the first one made.
            let version = adopt(&skills, &caller.tenant_id, &row.id, name)
                .await?
                .ok_or_else(|| RecordingError::State {
                    id: row.id.clone(),
                    status: "submitted".to_string(),
                    detail: "this recording became a skill under another name".to_string(),
                })?;
            return Ok(SubmitOutcome {
                recording: row,
                skill_id: version.skill_id.to_string(),
                version,
            });
        }
        let stored = row.proposal.clone().ok_or_else(|| {
            RecordingError::Malformed("a compiled recording lost its proposal".to_string())
        })?;
        let mut choices = Vec::with_capacity(decisions.len());
        for decision in decisions {
            let constant = match decision.classification.as_str() {
                "input" => false,
                "constant" => true,
                other => {
                    return Err(RecordingError::Invalid(format!(
                        "slot `{}`: `{other}` is not a classification (input or constant)",
                        decision.slot_id
                    )))
                }
            };
            choices.push(SlotChoice {
                slot_id: decision.slot_id.clone(),
                constant,
            });
        }
        let built = proposal::build_manifest(&stored, &choices)?;
        let provenance = json!({
            "origin": "recording",
            "recording_id": row.id,
            "artifact_id": row.artifact_id,
            "artifact_version_id": row.artifact_version_id,
            "content_digest": row.content_digest,
            "slots": built.slots,
            "validator": { "rule_set": super::RULE_SET, "passed": true },
        });
        let admin = SkillAdmin::new(
            caller.tenant_id.clone(),
            Actor::user(&caller.user_id),
            CorrelationId::generate(&mut quansio_core::UlidGenerator::new()),
        );
        let version = match skills.skill_by_name(&caller.tenant_id, name).await? {
            Some(existing) => {
                // The name is taken: it is this recording's own earlier submission, or a conflict.
                match adopt(&skills, &caller.tenant_id, &row.id, name).await? {
                    Some(version) => version,
                    None => {
                        return Err(RecordingError::State {
                            id: row.id.clone(),
                            status: row.status.as_str().to_string(),
                            detail: format!(
                                "a skill named `{}` is already registered",
                                existing.name
                            ),
                        })
                    }
                }
            }
            None => {
                let skill = skills
                    .create_skill(
                        &admin,
                        NewSkill {
                            scope: SkillScope::Workspace,
                            name: name.to_string(),
                            owner: caller.user_id.clone(),
                        },
                    )
                    .await?;
                skills
                    .create_version(
                        &admin,
                        &skill.id,
                        NewSkillVersion {
                            semver: semver.to_string(),
                            manifest: built.manifest,
                            provenance: Some(provenance.to_string()),
                        },
                    )
                    .await?
            }
        };
        debug_assert_eq!(version.status, SkillStatus::Draft);
        let recording = self
            .store
            .record_submission(caller, &row.id, &version.id.to_string())
            .await?;
        Ok(SubmitOutcome {
            recording,
            skill_id: version.skill_id.to_string(),
            version,
        })
    }
}

/// The version a recording already produced under `name`, if there is one.
async fn adopt(
    skills: &SkillStore,
    tenant_id: &str,
    recording_id: &str,
    name: &str,
) -> Result<Option<SkillVersion>, RecordingError> {
    let Some(skill) = skills.skill_by_name(tenant_id, name).await? else {
        return Ok(None);
    };
    Ok(skills
        .versions_of(tenant_id, &skill.id)
        .await?
        .into_iter()
        .find(|version| {
            version
                .provenance
                .as_deref()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .is_some_and(|document| {
                    document["origin"] == "recording" && document["recording_id"] == recording_id
                })
        }))
}

fn validate_name(name: &str) -> Result<(), RecordingError> {
    let valid = !name.is_empty()
        && name.len() <= SKILL_NAME_MAX
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if valid {
        Ok(())
    } else {
        Err(RecordingError::Invalid(
            "a skill name is lowercase, 1-64 characters, letters, digits, dots, underscores, dashes".to_string(),
        ))
    }
}

fn validate_semver(semver: &str) -> Result<(), RecordingError> {
    let core = semver.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    let valid = parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        && semver.len() <= 64;
    if valid {
        Ok(())
    } else {
        Err(RecordingError::Invalid(
            "a version is semver (major.minor.patch)".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_versions_are_checked_before_anything_is_written() {
        assert!(validate_name("make-a-note").is_ok());
        for bad in ["", "Make", "-x", "a b", &"a".repeat(65)] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_semver("1.0.0").is_ok());
        assert!(validate_semver("1.0.0-rc.1").is_ok());
        for bad in ["1.0", "v1.0.0", "1.0.x", ""] {
            assert!(validate_semver(bad).is_err(), "{bad:?}");
        }
    }
}
