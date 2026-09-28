//! The independent semantic verifier over the intelligence plane (RUN-008, DOMAIN.md §4.4).
//!
//! [`GatewaySemanticVerifier`] is the [`SemanticVerifierPort`] a deployment with the intelligence
//! plane composes: it asks the plane's `VerifyCompletion` RPC, which routes the judgement to a
//! model that is not the claimant (`python/intelligence/evaluation/semantic_verifier`). The plane
//! only proposes a verdict. Every refusal it answers with — a malformed verdict, an unavailable
//! model, unprovable independence — comes back as [`VerificationError::SemanticRefused`], which
//! the contract verifier turns into a rejected claim, never a pass.

use std::time::Duration;

use async_trait::async_trait;
use quansio_contracts::generated::intelligence::CompletionVerificationRequest;
use serde::Deserialize;

use super::service::{SemanticVerdict, SemanticVerificationRequest, SemanticVerifierPort};
use super::VerificationError;
use crate::intelligence::{CallScope, IntelligenceClient, IntelligenceError};

const INTELLIGENCE_YAML: &str = include_str!("../../../../../config/intelligence.yaml");

/// How the runtime bounds one semantic verification (`config/intelligence.yaml`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticVerificationSettings {
    /// How long the plane's verifier call may take.
    pub deadline: Duration,
    /// Output tokens the verdict may use.
    pub max_output_tokens: u32,
}

#[derive(Deserialize)]
struct RawDocument {
    semantic_verification: RawSettings,
}

#[derive(Deserialize)]
struct RawSettings {
    deadline_ms: u64,
    max_output_tokens: u32,
}

impl SemanticVerificationSettings {
    /// The repository's `config/intelligence.yaml`, compiled in.
    ///
    /// # Errors
    /// The file has no `semantic_verification` block of the documented shape, or a bound is zero.
    pub fn load() -> Result<Self, String> {
        Self::parse(INTELLIGENCE_YAML)
    }

    /// Parse a settings document.
    ///
    /// # Errors
    /// The document has no `semantic_verification` block of the documented shape, or a bound is
    /// zero.
    pub fn parse(document: &str) -> Result<Self, String> {
        let raw: RawDocument = serde_yaml::from_str(document)
            .map_err(|error| format!("config/intelligence.yaml: {error}"))?;
        let raw = raw.semantic_verification;
        if raw.deadline_ms == 0 || raw.max_output_tokens == 0 {
            return Err(
                "config/intelligence.yaml: every semantic_verification bound must be positive"
                    .to_string(),
            );
        }
        Ok(Self {
            deadline: Duration::from_millis(raw.deadline_ms),
            max_output_tokens: raw.max_output_tokens,
        })
    }
}

/// [`SemanticVerifierPort`] over the plane's `VerifyCompletion`.
#[derive(Debug, Clone)]
pub struct GatewaySemanticVerifier {
    client: IntelligenceClient,
    settings: SemanticVerificationSettings,
}

impl GatewaySemanticVerifier {
    /// A verifier calling the plane through `client`.
    #[must_use]
    pub const fn new(client: IntelligenceClient, settings: SemanticVerificationSettings) -> Self {
        Self { client, settings }
    }
}

/// The DOMAIN.md §15 code a plane refusal is recorded under; anything unrecognised is the
/// plane being unable to answer.
fn refusal_code(code: &str) -> &'static str {
    match code {
        "POLICY_DENIED" => "POLICY_DENIED",
        "VALIDATION_SCHEMA" => "VALIDATION_SCHEMA",
        "ROUTE_UNAVAILABLE" => "ROUTE_UNAVAILABLE",
        "PROVIDER_REFUSAL" => "PROVIDER_REFUSAL",
        "PROVIDER_RATE_LIMITED" => "PROVIDER_RATE_LIMITED",
        "DLP_DENIED" => "DLP_DENIED",
        _ => "PROVIDER_UNAVAILABLE",
    }
}

/// What the plane said, readable as feedback: the envelope's message and the verifier's own
/// refusal code when the status carries the canonical error envelope, else the status text.
fn refusal_detail(message: &str) -> String {
    let Ok(envelope) = serde_json::from_str::<serde_json::Value>(message) else {
        return message.to_string();
    };
    let Some(text) = envelope.get("message").and_then(serde_json::Value::as_str) else {
        return message.to_string();
    };
    let refusal = envelope
        .get("details")
        .and_then(serde_json::Value::as_array)
        .and_then(|details| {
            details.iter().find(|detail| {
                detail.get("key").and_then(serde_json::Value::as_str) == Some("refusal")
            })
        })
        .and_then(|detail| detail.get("value").and_then(serde_json::Value::as_str));
    match refusal {
        Some(refusal) => format!("{text} ({refusal})"),
        None => text.to_string(),
    }
}

#[async_trait]
impl SemanticVerifierPort for GatewaySemanticVerifier {
    async fn verify(
        &self,
        request: SemanticVerificationRequest,
    ) -> Result<SemanticVerdict, VerificationError> {
        let scope = CallScope {
            tenant_id: request.tenant_id.clone(),
            workspace_id: request.workspace_id.clone(),
            correlation_id: format!("{}:verify", request.run_id),
            deadline: self.settings.deadline,
            // Verification is the runtime's own authority, not the run's: it proposes nothing
            // and reads only what the claim cites.
            capability_projection_id: "runtime:verification".to_string(),
        };
        let wire = CompletionVerificationRequest {
            run_id: request.run_id,
            work_node_id: request.work_node_id,
            claim_summary: request.claim_summary,
            rubric_id: request.rubric_id.unwrap_or_default(),
            evidence_ids: request.evidence_ids,
            independent_model: request.independent_model,
            claimant_model: request.claimant_model.unwrap_or_default(),
            max_output_tokens: self.settings.max_output_tokens,
            ..CompletionVerificationRequest::default()
        };
        let mut client = self.client.clone();
        match client.verify_completion(&scope, wire).await {
            Ok(verdict) => Ok(SemanticVerdict {
                agrees: verdict.agrees,
                critique: verdict.critique,
                model: verdict.model,
            }),
            Err(IntelligenceError::Refused { code, message }) => {
                Err(VerificationError::SemanticRefused {
                    code: refusal_code(&code),
                    detail: refusal_detail(&message),
                })
            }
            Err(error) => Err(VerificationError::SemanticRefused {
                code: "PROVIDER_UNAVAILABLE",
                detail: error.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{refusal_code, SemanticVerificationSettings};

    #[test]
    fn the_repository_settings_parse_and_zero_bounds_are_refused() {
        let settings = SemanticVerificationSettings::load().expect("config/intelligence.yaml");
        assert!(settings.max_output_tokens > 0);
        let base = "semantic_verification: {deadline_ms: 1000, max_output_tokens: 64}";
        assert!(SemanticVerificationSettings::parse(base).is_ok());
        assert!(SemanticVerificationSettings::parse(&base.replace("1000", "0")).is_err());
        assert!(SemanticVerificationSettings::parse("model_call: {}").is_err());
    }

    #[test]
    fn a_plane_envelope_reads_as_its_message_and_refusal() {
        let envelope = r#"{"code":"POLICY_DENIED","message":"m cannot verify its own work","details":[{"key":"refusal","value":"INDEPENDENCE_UNPROVABLE"}]}"#;
        assert_eq!(
            super::refusal_detail(envelope),
            "m cannot verify its own work (INDEPENDENCE_UNPROVABLE)"
        );
        assert_eq!(super::refusal_detail("plain status"), "plain status");
    }

    #[test]
    fn an_unrecognised_plane_code_is_an_unavailable_verifier() {
        assert_eq!(refusal_code("POLICY_DENIED"), "POLICY_DENIED");
        assert_eq!(refusal_code("Unavailable"), "PROVIDER_UNAVAILABLE");
    }
}
