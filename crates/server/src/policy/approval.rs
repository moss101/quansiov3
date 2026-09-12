//! Approval requests, consequence previews and server-signed receipts (DOMAIN.md §7.3).
//!
//! An `ApprovalRequest` is bound to one EffectRecord and one `params_digest`; if the
//! effect's parameters change after the preview, the pending request is *superseded*
//! and a new request is required. An `ApprovalReceipt` is the only thing that can
//! authorize a tier ≥ 3 dispatch, and it is bound to request, effect, approver,
//! `params_digest`, `generation`, a single-use scope and an expiry.
//!
//! The receipt signature is an HMAC-SHA256 over those bound fields under a server-held
//! key read from configuration. A client-supplied signature is never trusted: the
//! verifier always recomputes under the server key, and an absent key fails closed.

use core::fmt;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use quansio_core::{CanonicalId, Digest, Generation, Prefix, UlidGenerator};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::policy::error::ApprovalFailure;
use crate::policy::escalation::TrustLevel;
use crate::policy::guards::DataClass;

/// Environment variable that carries the server-held approval signing key.
///
/// The key is configuration, never a client value and never a source constant; when it
/// is absent no receipt can be signed or verified, which fails closed.
pub const APPROVAL_SIGNING_KEY_ENV: &str = "QUANSIO_APPROVAL_SIGNING_KEY";

/// Signature scheme version, mixed into the signed message so a scheme change cannot
/// leave old receipts valid under new rules.
const SIGNATURE_VERSION: &str = "v1";

type HmacSha256 = Hmac<Sha256>;

/// A monetary amount in the consequence preview.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Amount {
    /// ISO 4217 currency code.
    pub currency: String,
    /// Amount in minor units (cents), never a float.
    pub minor_units: i64,
}

impl Amount {
    /// Build an amount.
    #[must_use]
    pub fn new(currency: impl Into<String>, minor_units: i64) -> Self {
        Self {
            currency: currency.into(),
            minor_units,
        }
    }
}

/// The untrusted origin a preview must show when the proposal derives from external data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UntrustedOrigin {
    /// Always [`TrustLevel::UntrustedExternal`]; the type makes the other levels
    /// unrepresentable here.
    trust: TrustLevel,
    /// Context segments the derivation came from.
    pub source_refs: Vec<String>,
}

impl UntrustedOrigin {
    /// Build an untrusted origin from the referenced context segments.
    #[must_use]
    pub fn new(source_refs: Vec<String>) -> Self {
        Self {
            trust: TrustLevel::UntrustedExternal,
            source_refs,
        }
    }

    /// The trust level (always untrusted external).
    #[must_use]
    pub const fn trust(&self) -> TrustLevel {
        self.trust
    }
}

/// The typed consequence preview shown before an effect is approved (DOMAIN.md §7.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConsequencePreview {
    /// Who would receive the effect (addresses, channel ids, role names).
    pub recipients: Vec<String>,
    /// Amounts the effect would move.
    pub amounts: Vec<Amount>,
    /// Targets (execution targets, resources, records) the effect would touch.
    pub targets: Vec<String>,
    /// Data classes the effect would move.
    pub data_classes: Vec<DataClass>,
    /// The untrusted origin, present if and only if the proposal is escalated.
    pub untrusted_origin: Option<UntrustedOrigin>,
}

impl ConsequencePreview {
    /// Build a preview.
    #[must_use]
    pub fn new(
        recipients: Vec<String>,
        amounts: Vec<Amount>,
        targets: Vec<String>,
        data_classes: Vec<DataClass>,
        untrusted_origin: Option<UntrustedOrigin>,
    ) -> Self {
        Self {
            recipients,
            amounts,
            targets,
            data_classes,
            untrusted_origin,
        }
    }

    /// Whether the preview shows an untrusted origin.
    #[must_use]
    pub fn shows_untrusted_origin(&self) -> bool {
        self.untrusted_origin.is_some()
    }
}

/// Who a request was asked of (DOMAIN.md §7.3 `requested_of`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum RequestedOf {
    /// A specific user.
    User(String),
    /// Every holder of a workspace role.
    Role(String),
}

/// The lifecycle status of an approval request (DOMAIN.md §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalRequestStatus {
    /// Awaiting a decision.
    Requested,
    /// A receipt was issued.
    Granted,
    /// A human refused.
    Denied,
    /// The request passed its expiry without a decision.
    Expired,
    /// The effect parameters changed, so this request can no longer authorize it.
    Superseded,
}

impl ApprovalRequestStatus {
    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::Superseded => "superseded",
        }
    }

    /// Parse a canonical status.
    ///
    /// # Errors
    /// Returns the rejected value when it is not a canonical status.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "requested" => Ok(Self::Requested),
            "granted" => Ok(Self::Granted),
            "denied" => Ok(Self::Denied),
            "expired" => Ok(Self::Expired),
            "superseded" => Ok(Self::Superseded),
            other => Err(other.to_string()),
        }
    }

    /// Whether the request is still awaiting a decision.
    #[must_use]
    pub const fn is_pending(self) -> bool {
        matches!(self, Self::Requested)
    }
}

impl fmt::Display for ApprovalRequestStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A request to approve one effect (DOMAIN.md §7.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewApprovalRequest {
    /// Run the effect belongs to.
    pub run_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// EffectRecord the request authorizes.
    pub effect_id: String,
    /// Who the request is asked of.
    pub requested_of: Vec<RequestedOf>,
    /// Human-readable summary.
    pub summary: String,
    /// The typed consequence preview.
    pub consequence_preview: ConsequencePreview,
    /// Digest of the exact effect parameters.
    pub params_digest: Digest,
    /// Capability projection the effect was authorized against.
    pub capability_projection_id: String,
    /// When the request expires.
    pub expires_at: DateTime<Utc>,
}

impl NewApprovalRequest {
    /// Build a request.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        run_id: impl Into<String>,
        workspace_id: impl Into<String>,
        effect_id: impl Into<String>,
        requested_of: Vec<RequestedOf>,
        summary: impl Into<String>,
        consequence_preview: ConsequencePreview,
        params_digest: Digest,
        capability_projection_id: impl Into<String>,
        expires_at: DateTime<Utc>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            workspace_id: workspace_id.into(),
            effect_id: effect_id.into(),
            requested_of,
            summary: summary.into(),
            consequence_preview,
            params_digest,
            capability_projection_id: capability_projection_id.into(),
            expires_at,
        }
    }

    /// Generate the canonical `apr_…` id for this request.
    #[must_use]
    pub fn generate_id() -> CanonicalId {
        let mut generator = UlidGenerator::new();
        CanonicalId::generate(Prefix::ApprovalRequest, &mut generator)
    }
}

/// A persisted approval request row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequestRecord {
    /// The `apr_…` request id.
    pub id: CanonicalId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Run the effect belongs to.
    pub run_id: String,
    /// EffectRecord the request authorizes.
    pub effect_id: String,
    /// Who the request is asked of.
    pub requested_of: Vec<RequestedOf>,
    /// Human-readable summary.
    pub summary: String,
    /// The typed consequence preview.
    pub consequence_preview: ConsequencePreview,
    /// Digest of the exact effect parameters at preview time.
    pub params_digest: Digest,
    /// Capability projection the effect was authorized against.
    pub capability_projection_id: String,
    /// Current status.
    pub status: ApprovalRequestStatus,
    /// When the request expires.
    pub expires_at: DateTime<Utc>,
}

impl ApprovalRequestRecord {
    /// Whether the request can still be granted at `now`.
    #[must_use]
    pub fn is_grantable(&self, now: DateTime<Utc>) -> bool {
        self.status.is_pending() && self.expires_at > now
    }

    /// Whether a changed parameter digest supersedes this request.
    #[must_use]
    pub fn is_superseded_by(&self, params_digest: &Digest) -> bool {
        self.status.is_pending() && &self.params_digest != params_digest
    }
}

/// The single-use scope of a receipt (DOMAIN.md §7.3). Only one value exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptScope {
    /// The receipt authorizes exactly one dispatch.
    SingleUse,
}

impl ReceiptScope {
    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SingleUse => "single_use",
        }
    }
}

/// A server-signed approval receipt (DOMAIN.md §7.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalReceipt {
    /// The `rcp_…` receipt id.
    pub id: CanonicalId,
    /// The `apr_…` request that produced it.
    pub request_id: String,
    /// The EffectRecord it authorizes.
    pub effect_id: String,
    /// The user who approved.
    pub approver_user_id: String,
    /// Digest of the exact authorized parameters.
    pub params_digest: Digest,
    /// Scope; always single-use.
    pub scope: ReceiptScope,
    /// Controller generation the receipt was granted under.
    pub generation: Generation,
    /// When it was granted.
    pub granted_at: DateTime<Utc>,
    /// When it expires.
    pub expires_at: DateTime<Utc>,
    /// Server HMAC over the bound fields.
    pub signature: String,
}

impl ApprovalReceipt {
    /// The canonical signed message for the receipt's bound fields.
    ///
    /// The signature covers exactly these values, so changing any bound field — or
    /// replaying the signature onto a different receipt — invalidates verification.
    #[must_use]
    pub fn signing_message(&self) -> String {
        format!(
            "{SIGNATURE_VERSION}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.id,
            self.request_id,
            self.effect_id,
            self.approver_user_id,
            self.params_digest.as_str(),
            self.scope.as_str(),
            self.generation.get(),
            self.granted_at.to_rfc3339(),
            self.expires_at.to_rfc3339(),
        )
    }

    /// Whether the receipt is past `expires_at`.
    #[must_use]
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }
}

/// The server-held HMAC key used to sign approval receipts.
#[derive(Clone)]
pub struct ApprovalSigner {
    key: Vec<u8>,
}

impl fmt::Debug for ApprovalSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never render key material.
        f.write_str("ApprovalSigner(<redacted>)")
    }
}

impl ApprovalSigner {
    /// Build a signer from an explicit server key.
    ///
    /// An empty key is rejected so an accidental empty configuration cannot sign.
    ///
    /// # Errors
    /// Returns [`ApprovalFailure::SignatureKeyMissing`] when the key is empty.
    pub fn new(key: impl Into<Vec<u8>>) -> Result<Self, ApprovalFailure> {
        let key = key.into();
        if key.is_empty() {
            return Err(ApprovalFailure::SignatureKeyMissing);
        }
        Ok(Self { key })
    }

    /// Read the server key from [`APPROVAL_SIGNING_KEY_ENV`].
    ///
    /// Returns `None` when the variable is unset or empty: no key means no receipt can
    /// be signed or verified, which fails closed.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let value = std::env::var(APPROVAL_SIGNING_KEY_ENV).ok()?;
        if value.is_empty() {
            return None;
        }
        Self::new(value.into_bytes()).ok()
    }

    /// Sign a receipt's bound fields.
    ///
    /// # Errors
    /// Returns [`ApprovalFailure::SignatureKeyMissing`] if the key cannot be used.
    pub fn sign(&self, receipt: &ApprovalReceipt) -> Result<String, ApprovalFailure> {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(&self.key)
            .map_err(|_| ApprovalFailure::SignatureKeyMissing)?;
        mac.update(receipt.signing_message().as_bytes());
        Ok(hex::encode(mac.finalize().into_bytes()))
    }

    /// Verify a receipt's signature in constant time.
    ///
    /// # Errors
    /// Returns [`ApprovalFailure::SignatureInvalid`] when the signature does not match
    /// the server key; the receipt is never trusted on a mismatch.
    pub fn verify(&self, receipt: &ApprovalReceipt) -> Result<(), ApprovalFailure> {
        let expected =
            hex::decode(&receipt.signature).map_err(|_| ApprovalFailure::SignatureInvalid)?;
        let mut mac = <HmacSha256 as Mac>::new_from_slice(&self.key)
            .map_err(|_| ApprovalFailure::SignatureKeyMissing)?;
        mac.update(receipt.signing_message().as_bytes());
        mac.verify_slice(&expected)
            .map_err(|_| ApprovalFailure::SignatureInvalid)
    }
}

/// What a dispatch is asking the receipt to authorize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchBinding {
    /// The EffectRecord being dispatched.
    pub effect_id: String,
    /// The exact parameters of the dispatch.
    pub params_digest: Digest,
    /// The controller generation of the dispatch.
    pub generation: Generation,
}

/// Verify a receipt against a dispatch binding (DOMAIN.md §7.3, §16).
///
/// Every failure is typed and fails closed; the caller must not dispatch on `Err`.
///
/// # Errors
/// Returns the first failed binding: signature, effect, parameters, generation or expiry.
pub fn verify_receipt(
    signer: &ApprovalSigner,
    receipt: &ApprovalReceipt,
    binding: &DispatchBinding,
    now: DateTime<Utc>,
) -> Result<(), ApprovalFailure> {
    signer.verify(receipt)?;
    if receipt.effect_id != binding.effect_id {
        return Err(ApprovalFailure::EffectMismatch {
            expected: binding.effect_id.clone(),
            receipt: receipt.effect_id.clone(),
        });
    }
    if receipt.params_digest != binding.params_digest {
        return Err(ApprovalFailure::ParamsChanged {
            expected: binding.params_digest.as_str().to_string(),
            receipt: receipt.params_digest.as_str().to_string(),
        });
    }
    if receipt.generation != binding.generation {
        return Err(ApprovalFailure::GenerationStale {
            expected: binding.generation.get(),
            receipt: receipt.generation.get(),
        });
    }
    if receipt.is_expired(now) {
        return Err(ApprovalFailure::Expired);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(value: &str) -> Digest {
        Digest::of(value.as_bytes())
    }

    fn signed_receipt(signer: &ApprovalSigner) -> ApprovalReceipt {
        let mut generator = UlidGenerator::new();
        let mut signed = ApprovalReceipt {
            id: CanonicalId::generate(Prefix::ApprovalReceipt, &mut generator),
            request_id: "apr_x".to_string(),
            effect_id: "eff_x".to_string(),
            approver_user_id: "usr_x".to_string(),
            params_digest: digest("params"),
            scope: ReceiptScope::SingleUse,
            generation: Generation::new(1).expect("generation"),
            granted_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::seconds(600),
            signature: String::new(),
        };
        signed.signature = signer.sign(&signed).expect("sign");
        signed
    }

    #[test]
    fn a_valid_receipt_verifies_for_its_exact_binding() {
        let signer = ApprovalSigner::new(b"server-key".to_vec()).expect("signer");
        let receipt = signed_receipt(&signer);
        let binding = DispatchBinding {
            effect_id: "eff_x".to_string(),
            params_digest: digest("params"),
            generation: Generation::new(1).expect("generation"),
        };
        assert!(verify_receipt(&signer, &receipt, &binding, Utc::now()).is_ok());
    }

    #[test]
    fn a_tampered_receipt_never_verifies() {
        let signer = ApprovalSigner::new(b"server-key".to_vec()).expect("signer");
        let mut receipt = signed_receipt(&signer);
        receipt.effect_id = "eff_other".to_string();
        assert_eq!(
            signer.verify(&receipt),
            Err(ApprovalFailure::SignatureInvalid)
        );

        let mut forged = signed_receipt(&signer);
        forged.signature = hex::encode([0u8; 32]);
        assert_eq!(
            signer.verify(&forged),
            Err(ApprovalFailure::SignatureInvalid)
        );
    }

    #[test]
    fn a_signature_from_another_key_never_verifies() {
        let signer = ApprovalSigner::new(b"server-key".to_vec()).expect("signer");
        let other = ApprovalSigner::new(b"other-key".to_vec()).expect("signer");
        let receipt = signed_receipt(&signer);
        assert_eq!(
            other.verify(&receipt),
            Err(ApprovalFailure::SignatureInvalid)
        );
    }

    #[test]
    fn binding_mismatches_are_typed_failures() {
        let signer = ApprovalSigner::new(b"server-key".to_vec()).expect("signer");
        let receipt = signed_receipt(&signer);
        let now = Utc::now();
        let wrong_params = DispatchBinding {
            effect_id: "eff_x".to_string(),
            params_digest: digest("changed"),
            generation: Generation::new(1).expect("generation"),
        };
        assert!(matches!(
            verify_receipt(&signer, &receipt, &wrong_params, now),
            Err(ApprovalFailure::ParamsChanged { .. })
        ));
        let wrong_generation = DispatchBinding {
            effect_id: "eff_x".to_string(),
            params_digest: digest("params"),
            generation: Generation::new(2).expect("generation"),
        };
        assert!(matches!(
            verify_receipt(&signer, &receipt, &wrong_generation, now),
            Err(ApprovalFailure::GenerationStale { .. })
        ));
        let wrong_effect = DispatchBinding {
            effect_id: "eff_y".to_string(),
            params_digest: digest("params"),
            generation: Generation::new(1).expect("generation"),
        };
        assert!(matches!(
            verify_receipt(&signer, &receipt, &wrong_effect, now),
            Err(ApprovalFailure::EffectMismatch { .. })
        ));
    }

    #[test]
    fn an_expired_receipt_is_refused() {
        let signer = ApprovalSigner::new(b"server-key".to_vec()).expect("signer");
        let mut receipt = signed_receipt(&signer);
        receipt.expires_at = Utc::now() - chrono::Duration::seconds(1);
        receipt.signature = signer.sign(&receipt).expect("sign");
        let binding = DispatchBinding {
            effect_id: "eff_x".to_string(),
            params_digest: digest("params"),
            generation: Generation::new(1).expect("generation"),
        };
        assert_eq!(
            verify_receipt(&signer, &receipt, &binding, Utc::now()),
            Err(ApprovalFailure::Expired)
        );
    }

    #[test]
    fn an_empty_key_is_refused() {
        assert_eq!(
            ApprovalSigner::new(Vec::new()).err(),
            Some(ApprovalFailure::SignatureKeyMissing)
        );
    }

    #[test]
    fn request_status_round_trips_and_supersession_detects_changed_params() {
        for status in [
            ApprovalRequestStatus::Requested,
            ApprovalRequestStatus::Granted,
            ApprovalRequestStatus::Denied,
            ApprovalRequestStatus::Expired,
            ApprovalRequestStatus::Superseded,
        ] {
            assert_eq!(
                ApprovalRequestStatus::parse(status.as_str()).expect("status"),
                status
            );
        }
        let mut generator = UlidGenerator::new();
        let request = ApprovalRequestRecord {
            id: CanonicalId::generate(Prefix::ApprovalRequest, &mut generator),
            tenant_id: "tn_x".to_string(),
            workspace_id: "ws_x".to_string(),
            run_id: "run_x".to_string(),
            effect_id: "eff_x".to_string(),
            requested_of: vec![RequestedOf::User("usr_x".to_string())],
            summary: "summary".to_string(),
            consequence_preview: ConsequencePreview::default(),
            params_digest: digest("params"),
            capability_projection_id: "cap_x".to_string(),
            status: ApprovalRequestStatus::Requested,
            expires_at: Utc::now() + chrono::Duration::seconds(600),
        };
        assert!(request.is_superseded_by(&digest("changed")));
        assert!(!request.is_superseded_by(&digest("params")));
        assert!(request.is_grantable(Utc::now()));
    }
}
