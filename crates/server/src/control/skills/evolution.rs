//! Controlled skill evolution (CAP-008): verified experience becomes CANDIDATE versions.
//!
//! The registry side of the loop. [`SkillEvolution::propose`] reads what the runtime knows as
//! canonical fact — runs that reached a verified terminal state, the evidence each one captured,
//! and the skill versions their recorded context projections gave them — and hands it to the
//! intelligence plane (`ProposeSkillCandidates`), which detects repeated patterns, proposes a patch
//! per pattern and evaluates it against the skill's suite. Every candidate that comes back is
//! stored as a `CANDIDATE` version with provenance naming its evidence and its evaluation.
//!
//! Nothing here can change production behaviour: a candidate never resolves, and
//! [`super::store::evolution_gate`] refuses to promote one past EVALUATING without the passing
//! evaluation its provenance records. Promotion stays a person's governed decision
//! (`PromoteSkillVersion`, tenant administrators only).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::{json, Value};
use sqlx::{PgPool, Row};

use super::store::{NewSkillVersion, SkillAdmin, SkillStore, SkillVersion};
use super::{SkillControlError, SkillStatus};
use crate::control::schema;
use crate::intelligence::{CallScope, IntelligenceClient, IntelligenceError};

/// What one evolution pass did.
#[derive(Debug, Clone, PartialEq)]
pub struct EvolutionOutcome {
    /// Verified run records the plane was given.
    pub records: usize,
    /// Candidates stored as `CANDIDATE` versions.
    pub registered: Vec<SkillVersion>,
    /// Candidates not stored because that semver already exists (`skill_id@semver`).
    pub already_proposed: Vec<String>,
}

/// Why an evolution pass failed.
#[derive(Debug, thiserror::Error)]
pub enum SkillEvolutionError {
    /// The registry refused.
    #[error(transparent)]
    Registry(#[from] SkillControlError),
    /// Canonical state could not be read.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    /// The tenant context could not be set.
    #[error(transparent)]
    Schema(#[from] crate::control::schema::SchemaError),
    /// The intelligence plane refused or could not be reached.
    #[error(transparent)]
    Plane(#[from] IntelligenceError),
    /// The plane returned something that is not a candidate.
    #[error("the plane returned a malformed candidate: {0}")]
    Malformed(String),
}

impl SkillEvolutionError {
    /// The DOMAIN.md §15 code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Registry(error) => error.code(),
            Self::Plane(IntelligenceError::Refused { code, .. }) if code == "VALIDATION_SCHEMA" => {
                "VALIDATION_SCHEMA"
            }
            Self::Plane(_) => "TARGET_UNAVAILABLE",
            Self::Database(_) | Self::Schema(_) | Self::Malformed(_) => "INTERNAL",
        }
    }
}

/// The evolution pass over one tenant's registry.
#[derive(Debug, Clone)]
pub struct SkillEvolution {
    pool: PgPool,
    client: IntelligenceClient,
    deadline: Duration,
}

impl SkillEvolution {
    /// A pass reading and writing through `pool`, asking the plane through `client`.
    #[must_use]
    pub const fn new(pool: PgPool, client: IntelligenceClient, deadline: Duration) -> Self {
        Self {
            pool,
            client,
            deadline,
        }
    }

    /// Gather verified experience, ask for candidates, store each as `CANDIDATE`.
    ///
    /// # Errors
    /// A registry refusal, unreadable canonical state, or a plane failure.
    pub async fn propose(
        &self,
        admin: &SkillAdmin,
        workspace_id: &str,
    ) -> Result<EvolutionOutcome, SkillEvolutionError> {
        let store = SkillStore::new(self.pool.clone());
        let active = store.active_versions(&admin.tenant_id).await?;
        let versions: Vec<String> = active
            .iter()
            .map(|(skill, version)| {
                json!({
                    "skill_id": skill.id.to_string(),
                    "version_id": version.id.to_string(),
                    "name": skill.name,
                    "semver": version.semver,
                    "status": version.status.as_str(),
                    "manifest": version.manifest,
                })
                .to_string()
            })
            .collect();
        let evolvable: BTreeSet<String> = active
            .iter()
            .map(|(skill, _)| skill.id.to_string())
            .collect();
        let records: Vec<String> = self
            .records(&admin.tenant_id)
            .await?
            .into_iter()
            .filter(|record| {
                record["skill_id"]
                    .as_str()
                    .is_some_and(|skill| evolvable.contains(skill))
            })
            .map(|record| record.to_string())
            .collect();
        let mut outcome = EvolutionOutcome {
            records: records.len(),
            registered: Vec::new(),
            already_proposed: Vec::new(),
        };
        if records.is_empty() {
            return Ok(outcome);
        }
        let scope = CallScope {
            tenant_id: admin.tenant_id.clone(),
            workspace_id: workspace_id.to_string(),
            correlation_id: admin.correlation_id.to_string(),
            deadline: self.deadline,
            // An administrator's command, authorized by RBAC rather than a run's projection.
            capability_projection_id: "rbac:manage_tenant".to_string(),
        };
        let mut client = self.client.clone();
        let candidates = client
            .propose_skill_candidates(&scope, records, versions)
            .await?;
        for raw in candidates {
            let candidate: Value = serde_json::from_str(&raw)
                .map_err(|error| SkillEvolutionError::Malformed(error.to_string()))?;
            let text = |key: &str| candidate[key].as_str().map(str::to_string);
            let (Some(skill_id), Some(semver)) = (text("skill_id"), text("semver")) else {
                return Err(SkillEvolutionError::Malformed(raw));
            };
            let skill =
                quansio_core::CanonicalId::parse_typed(&skill_id, quansio_core::Prefix::Skill)
                    .map_err(|error| SkillEvolutionError::Malformed(error.to_string()))?;
            if self
                .semver_exists(&admin.tenant_id, &skill_id, &semver)
                .await?
            {
                outcome
                    .already_proposed
                    .push(format!("{skill_id}@{semver}"));
                continue;
            }
            let draft = store
                .create_version(
                    admin,
                    &skill,
                    NewSkillVersion {
                        semver,
                        manifest: candidate["manifest"].clone(),
                        provenance: Some(candidate["provenance"].to_string()),
                    },
                )
                .await?;
            let promotion = store
                .promote(admin, &draft.id, SkillStatus::Candidate)
                .await?;
            outcome.registered.push(promotion.version);
        }
        Ok(outcome)
    }

    /// Verified run outcomes with their evidence and the skill versions their context named.
    async fn records(&self, tenant_id: &str) -> Result<Vec<Value>, SkillEvolutionError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, tenant_id).await?;
        let rows = sqlx::query(
            "SELECT DISTINCT r.id AS run_id, r.status, r.terminal_reason, v.skill_id, \
                    e.id AS evidence_id, e.content_digest, e.inline_summary, m.content_blocks, \
                    EXISTS (SELECT 1 FROM tool_calls c JOIN steps s ON s.id = c.step_id \
                            JOIN turns t ON t.id = s.turn_id \
                            WHERE t.run_id = r.id AND c.status IN ('failed', 'rejected', 'unknown')) \
                        AS stumbled \
             FROM runs r \
             JOIN context_projections p ON p.tenant_id = r.tenant_id AND p.run_id = r.id \
             CROSS JOIN LATERAL jsonb_array_elements(p.segments) segment \
             JOIN skill_versions v ON v.tenant_id = r.tenant_id AND v.id = segment->>'source' \
             JOIN LATERAL (SELECT id, content_digest, inline_summary FROM evidence \
                           WHERE tenant_id = r.tenant_id AND run_id = r.id \
                           ORDER BY captured_at DESC, id DESC LIMIT 1) e ON true \
             LEFT JOIN messages m ON m.tenant_id = r.tenant_id AND m.id = r.trigger_ref \
             WHERE r.tenant_id = $1 AND r.status IN ('SUCCEEDED', 'FAILED') \
               AND segment->>'kind' = 'skill' \
             ORDER BY r.id, v.skill_id",
        )
        .bind(tenant_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut records = BTreeMap::new();
        for row in rows {
            let run_id: String = row.try_get("run_id")?;
            let skill_id: String = row.try_get("skill_id")?;
            let status: String = row.try_get("status")?;
            let stumbled: bool = row.try_get("stumbled")?;
            let kind = match (status.as_str(), stumbled) {
                ("FAILED", _) => "failure",
                (_, true) => "recovery",
                _ => "success",
            };
            let detail = row
                .try_get::<Option<String>, _>("inline_summary")?
                .or(row.try_get::<Option<String>, _>("terminal_reason")?)
                .unwrap_or_default();
            let task = row
                .try_get::<Option<Value>, _>("content_blocks")?
                .and_then(|blocks| {
                    blocks.as_array().map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|block| block.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                })
                .unwrap_or_default();
            records.insert(
                (run_id.clone(), skill_id.clone()),
                json!({
                    "evidence_id": row.try_get::<String, _>("evidence_id")?,
                    "skill_id": skill_id,
                    "run_id": run_id,
                    "kind": kind,
                    "digest": row.try_get::<String, _>("content_digest")?,
                    "detail": detail,
                    "task": task,
                }),
            );
        }
        Ok(records.into_values().collect())
    }

    async fn semver_exists(
        &self,
        tenant_id: &str,
        skill_id: &str,
        semver: &str,
    ) -> Result<bool, SkillEvolutionError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, tenant_id).await?;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM skill_versions \
             WHERE tenant_id = $1 AND skill_id = $2 AND semver = $3)",
        )
        .bind(tenant_id)
        .bind(skill_id)
        .bind(semver)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(exists)
    }
}
