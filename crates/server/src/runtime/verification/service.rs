//! The contract verifier: the runtime's answer to a model's completion claim (DOMAIN.md §4.4).
//!
//! The verifier is a [`VerificationPort`], so the engine reaches `SUCCEEDED` only through it.
//! It refuses to certify a claim when the contract cannot be read, when the contract binds
//! nothing, when any deterministic check fails, when a human signoff is required, or when the
//! independent semantic verifier disagrees — and every refusal carries actionable feedback
//! naming what failed and which task owns the missing authority.

use std::sync::Arc;

use async_trait::async_trait;
use quansio_core::CanonicalId;
use serde_json::Value;

use super::checks::{run_check, CheckResult, CheckScope};
use super::contract::CompletionContract;
use super::VerificationError;
use crate::control::schema;
use crate::effects::EffectLedger;
use crate::runtime::state_machine::{
    CompletionClaim, RuntimeError, RuntimeIdentity, RuntimeStore, VerificationContext,
    VerificationOutcome, VerificationPort,
};

/// What an independent semantic verifier is asked to judge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticVerificationRequest {
    /// Run the claim belongs to.
    pub run_id: String,
    /// WorkNode the completion is claimed for.
    pub work_node_id: String,
    /// The model's own summary of what it did.
    pub claim_summary: String,
    /// Rubric the contract names, when it names one.
    pub rubric_id: Option<String>,
    /// Whether the verdict must come from a different model than the claimant.
    pub independent_model: bool,
    /// Evidence the claimant cites.
    pub evidence_ids: Vec<String>,
}

/// An independent semantic verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticVerdict {
    /// Whether the verifier agrees the work is complete.
    pub agrees: bool,
    /// Why, in the verifier's own words; used verbatim as feedback.
    pub critique: String,
    /// Which model produced the verdict.
    pub model: String,
}

/// The optional independent semantic verifier (DOMAIN.md §4.4, §11.1).
#[async_trait]
pub trait SemanticVerifierPort: Send + Sync {
    /// Judge the claim independently of the model that made it.
    ///
    /// # Errors
    /// Returns a [`VerificationError`] when the verifier cannot produce a verdict; the caller
    /// treats that as a refusal, never as agreement.
    async fn verify(
        &self,
        request: SemanticVerificationRequest,
    ) -> Result<SemanticVerdict, VerificationError>;
}

/// The default verifier: the gateway-backed one belongs to [`super::SEMANTIC_VERIFIER_OWNER`],
/// so a contract that requires it fails closed instead of self-certifying.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableSemanticVerifier;

#[async_trait]
impl SemanticVerifierPort for UnavailableSemanticVerifier {
    async fn verify(
        &self,
        _request: SemanticVerificationRequest,
    ) -> Result<SemanticVerdict, VerificationError> {
        Err(VerificationError::UnknownPredicate {
            expr: format!(
                "semantic verification (owned by {})",
                super::SEMANTIC_VERIFIER_OWNER
            ),
        })
    }
}

/// The contract verifier.
#[derive(Clone)]
pub struct ContractVerifier {
    pool: sqlx::PgPool,
    runs: RuntimeStore,
    ledger: EffectLedger,
    semantic: Arc<dyn SemanticVerifierPort>,
}

impl ContractVerifier {
    /// Build the verifier over the runtime's own stores.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Schema`] when the tenant id is not canonical.
    pub fn new(pool: sqlx::PgPool, identity: RuntimeIdentity) -> Result<Self, RuntimeError> {
        Ok(Self {
            runs: RuntimeStore::new(pool.clone(), identity.clone())?,
            ledger: EffectLedger::new(pool.clone(), identity)
                .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?,
            pool,
            semantic: Arc::new(UnavailableSemanticVerifier),
        })
    }

    /// Install the independent semantic verifier.
    #[must_use]
    pub fn with_semantic_verifier(mut self, port: Arc<dyn SemanticVerifierPort>) -> Self {
        self.semantic = port;
        self
    }

    /// The Effect Ledger the `effects_settled` check reads.
    #[must_use]
    pub fn ledger(&self) -> &EffectLedger {
        &self.ledger
    }

    /// Load the CompletionContract of the WorkNode a run is executing.
    async fn load_contract(
        &self,
        tenant_id: &str,
        work_node_id: &str,
    ) -> Result<Option<Value>, VerificationError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, tenant_id)
            .await
            .map_err(|error| {
                VerificationError::Database(sqlx::Error::Protocol(error.to_string()))
            })?;
        let contract: Option<Value> = sqlx::query_scalar(
            "SELECT completion_contract FROM work_nodes WHERE id = $1 AND tenant_id = $2",
        )
        .bind(work_node_id)
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(contract)
    }
}

/// Render check failures as actionable feedback.
fn feedback(checks: &[CheckResult]) -> String {
    let mut lines = Vec::new();
    for check in checks.iter().filter(|check| !check.passed) {
        match check.owner {
            Some(owner) => lines.push(format!(
                "{}: {} (not implemented yet; owned by {})",
                check.kind, check.detail, owner
            )),
            None => lines.push(format!("{}: {}", check.kind, check.detail)),
        }
    }
    lines.join("; ")
}

#[async_trait]
impl VerificationPort for ContractVerifier {
    async fn verify(
        &self,
        claim: CompletionClaim,
        context: VerificationContext,
    ) -> Result<VerificationOutcome, RuntimeError> {
        let tenant_id = self.runs.identity().tenant_id.clone();
        let run_id = CanonicalId::parse_typed(&context.run_id, quansio_core::Prefix::Run)
            .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
        let run = self.runs.load_run(&run_id).await?;

        // A claim with no readable contract cannot certify anything.
        let Some(raw) = self
            .load_contract(&tenant_id, &run.work_node_id.to_string())
            .await
            .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?
        else {
            return Ok(VerificationOutcome::Rejected {
                feedback: format!(
                    "WorkNode {} has no CompletionContract, so completion cannot be certified",
                    run.work_node_id
                ),
            });
        };
        let contract = match CompletionContract::parse(&raw) {
            Ok(contract) => contract,
            Err(error) => {
                return Ok(VerificationOutcome::Rejected {
                    feedback: format!(
                        "the CompletionContract cannot be evaluated ({}): {error}",
                        error.code()
                    ),
                })
            }
        };
        if contract.binds_nothing() {
            return Ok(VerificationOutcome::Rejected {
                feedback: "the CompletionContract binds no check, test, effect or signoff, so a \
                           model claim alone cannot complete this WorkNode"
                    .to_string(),
            });
        }

        // Deterministic checks first, in declaration order, so feedback lists every failure.
        let scope = CheckScope {
            pool: &self.pool,
            tenant_id: &tenant_id,
            workspace_id: &run.workspace_id,
            run_id: &run.id.to_string(),
            ledger: &self.ledger,
        };
        let mut results = Vec::with_capacity(contract.deterministic_checks.len());
        for check in &contract.deterministic_checks {
            let result = run_check(check, &scope)
                .await
                .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
            results.push(result);
        }
        if results.iter().any(|result| !result.passed) {
            return Ok(VerificationOutcome::Rejected {
                feedback: feedback(&results),
            });
        }

        // A model may never sign off on a human's behalf.
        if contract.human_signoff_required {
            return Ok(VerificationOutcome::Rejected {
                feedback: "the CompletionContract requires human signoff before this WorkNode can \
                           be done"
                    .to_string(),
            });
        }

        // Optional independent semantic verification: a disagreement, or a verifier that cannot
        // answer, keeps the work incomplete.
        if contract.semantic_verification.required {
            let request = SemanticVerificationRequest {
                run_id: run.id.to_string(),
                work_node_id: run.work_node_id.to_string(),
                claim_summary: claim.summary.clone(),
                rubric_id: contract.semantic_verification.rubric_id.clone(),
                independent_model: contract.semantic_verification.independent_model,
                evidence_ids: claim.evidence_ids.clone(),
            };
            match self.semantic.verify(request).await {
                Ok(verdict) if verdict.agrees => {}
                Ok(verdict) => {
                    return Ok(VerificationOutcome::Rejected {
                        feedback: format!(
                            "the independent semantic verifier ({}) disagrees: {}",
                            verdict.model, verdict.critique
                        ),
                    })
                }
                Err(error) => {
                    return Ok(VerificationOutcome::Rejected {
                        feedback: format!(
                            "the contract requires independent semantic verification, which is \
                             unavailable ({}): {error}",
                            error.code()
                        ),
                    })
                }
            }
        }

        let mut evidence_ids = claim.evidence_ids.clone();
        for result in &results {
            evidence_ids.push(format!("verification:{}", result.kind));
        }
        Ok(VerificationOutcome::Verified { evidence_ids })
    }
}
