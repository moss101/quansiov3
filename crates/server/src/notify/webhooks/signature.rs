//! The webhook signature (DOMAIN.md §13.5).
//!
//! `quansio-signature: t=<unix seconds>,v1=<hex HMAC-SHA256(secret, "<t>.<body>")>`. The
//! timestamp is inside the signed bytes, so a captured delivery cannot be replayed later with
//! a fresh time; a receiver rejects a timestamp outside its tolerance. The secret is the
//! `whsec_…` string the subscription's creator was given, used as the HMAC key as UTF-8.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// The header that carries the signature.
pub const SIGNATURE_HEADER: &str = "quansio-signature";
/// The header that carries the event id consumers dedupe on.
pub const EVENT_ID_HEADER: &str = "quansio-event-id";
/// The header that carries the event type.
pub const EVENT_TYPE_HEADER: &str = "quansio-event-type";
/// The header that carries the delivery id.
pub const DELIVERY_ID_HEADER: &str = "quansio-delivery-id";
/// The signature scheme version in the header.
pub const SIGNATURE_SCHEME: &str = "v1";
/// How far a verifier accepts a timestamp from its own clock by default.
pub const SIGNATURE_TOLERANCE_SECONDS: i64 = 300;
/// The prefix of a signing secret.
pub const SECRET_PREFIX: &str = "whsec_";

type HmacSha256 = Hmac<Sha256>;

fn mac(secret: &[u8], timestamp: i64, body: &[u8]) -> HmacSha256 {
    // HMAC accepts a key of any length; `new_from_slice` cannot fail for it.
    let mut mac = HmacSha256::new_from_slice(secret).unwrap_or_else(|_| unreachable!());
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    mac
}

/// The header value for `body` signed with `secret` at `timestamp`.
#[must_use]
pub fn sign(secret: &[u8], timestamp: i64, body: &[u8]) -> String {
    let digest = mac(secret, timestamp, body).finalize().into_bytes();
    format!("t={timestamp},{SIGNATURE_SCHEME}={}", hex::encode(digest))
}

/// Why a signature was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    /// The header has no `t=` or no `v1=`.
    #[error("the signature header is malformed")]
    Malformed,
    /// The timestamp is outside the tolerance.
    #[error("the signature timestamp is outside the tolerance")]
    Stale,
    /// No `v1` signature matches.
    #[error("the signature does not match")]
    Mismatch,
}

/// Verify a header against `body` at `now` (unix seconds), as a receiver would.
///
/// # Errors
/// Returns the [`SignatureError`] that refused it. The comparison is constant-time.
pub fn verify(
    secret: &[u8],
    header: &str,
    body: &[u8],
    now: i64,
    tolerance_seconds: i64,
) -> Result<(), SignatureError> {
    let mut timestamp = None;
    let mut candidates = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", value)) => timestamp = value.parse::<i64>().ok(),
            Some((SIGNATURE_SCHEME, value)) => candidates.push(value.to_string()),
            _ => {}
        }
    }
    let timestamp = timestamp.ok_or(SignatureError::Malformed)?;
    if candidates.is_empty() {
        return Err(SignatureError::Malformed);
    }
    if (now - timestamp).abs() > tolerance_seconds {
        return Err(SignatureError::Stale);
    }
    for candidate in candidates {
        let Ok(bytes) = hex::decode(candidate) else {
            continue;
        };
        if mac(secret, timestamp, body).verify_slice(&bytes).is_ok() {
            return Ok(());
        }
    }
    Err(SignatureError::Mismatch)
}

/// A new signing secret: `whsec_` and 32 random bytes, base64url without padding.
#[must_use]
pub fn generate_secret(entropy: &dyn quansio_core::EntropySource) -> String {
    use base64::Engine as _;
    let mut bytes = [0u8; 32];
    entropy.fill(&mut bytes);
    format!(
        "{SECRET_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cross-language vector: the SDK tests (`sdk/typescript/tests/webhooks.test.ts`,
    /// `sdk/python/tests/test_webhooks.py`) verify this exact header.
    const VECTOR_SECRET: &str = "whsec_dGVzdC1vbmx5LXNpZ25pbmctc2VjcmV0LTMyYnl0ZXMh";
    const VECTOR_BODY: &str =
        r#"{"event_id":"evt_01J8Z3K6F1N8VQ2X5W9Y0ABCDE","type":"run.succeeded"}"#;
    const VECTOR_TIMESTAMP: i64 = 1_790_000_000;
    const VECTOR_HEADER: &str =
        "t=1790000000,v1=9852831d03e3f0aa4d104a985bb8b53967170389ff827d1d3a6492fe2d2b4adb";

    #[test]
    fn a_signature_verifies_and_anything_else_does_not() {
        let header = sign(b"whsec_a", 100, b"{}");
        assert!(verify(b"whsec_a", &header, b"{}", 100, 300).is_ok());
        assert_eq!(
            verify(b"whsec_b", &header, b"{}", 100, 300),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verify(b"whsec_a", &header, b"{ }", 100, 300),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verify(b"whsec_a", &header, b"{}", 401, 300),
            Err(SignatureError::Stale)
        );
        assert_eq!(
            verify(b"whsec_a", "v1=00", b"{}", 100, 300),
            Err(SignatureError::Malformed)
        );
        // A second signature (a rotation window) is accepted when either matches.
        let rotated = format!("{header},v1={}", "0".repeat(64));
        assert!(verify(b"whsec_a", &rotated, b"{}", 100, 300).is_ok());
    }

    #[test]
    fn the_cross_language_vector_is_what_the_server_signs() {
        assert_eq!(
            sign(
                VECTOR_SECRET.as_bytes(),
                VECTOR_TIMESTAMP,
                VECTOR_BODY.as_bytes()
            ),
            VECTOR_HEADER
        );
    }

    #[test]
    fn a_secret_is_prefixed_and_random() {
        let one = generate_secret(&quansio_core::OsEntropy);
        let two = generate_secret(&quansio_core::OsEntropy);
        assert!(one.starts_with(SECRET_PREFIX));
        assert_eq!(one.len(), SECRET_PREFIX.len() + 43);
        assert_ne!(one, two);
    }
}
