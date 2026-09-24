//! OIDC id_token verification against the issuer's JWKS (OPS-001, DOMAIN.md §2).
//!
//! Only RS256 and ES256 are accepted. `none`, the HMAC family and every other algorithm are
//! refused before a key is looked at, so a token cannot choose a weaker check. The key is the JWKS
//! entry the header's `kid` names, and its type and declared `alg` must agree with the header.
//! The claims must then match what this sign-in expects: the exact issuer, an audience holding the
//! client id (`azp` when there are several), times within the configured skew, the nonce this
//! request sent, and a subject. `ring` does the signature arithmetic, as it does for the access
//! tokens this server mints.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::digest::{digest, SHA256};
use ring::signature::{
    RsaPublicKeyComponents, UnparsedPublicKey, ECDSA_P256_SHA256_FIXED, RSA_PKCS1_2048_8192_SHA256,
};
use serde::Deserialize;
use serde_json::{Map, Value};

/// One JSON Web Key (RFC 7517) as an issuer publishes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Jwk {
    /// `RSA` or `EC`.
    pub kty: String,
    /// Key id.
    #[serde(default)]
    pub kid: Option<String>,
    /// Declared algorithm, when the issuer states one.
    #[serde(default)]
    pub alg: Option<String>,
    /// Declared use; only `sig` keys verify tokens.
    #[serde(default, rename = "use")]
    pub key_use: Option<String>,
    /// RSA modulus (base64url, big-endian).
    #[serde(default)]
    pub n: Option<String>,
    /// RSA exponent (base64url, big-endian).
    #[serde(default)]
    pub e: Option<String>,
    /// EC curve.
    #[serde(default)]
    pub crv: Option<String>,
    /// EC x coordinate (base64url).
    #[serde(default)]
    pub x: Option<String>,
    /// EC y coordinate (base64url).
    #[serde(default)]
    pub y: Option<String>,
}

/// A JWK set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Jwks {
    /// The keys.
    #[serde(default)]
    pub keys: Vec<Jwk>,
}

/// What this sign-in expects of the token.
#[derive(Debug, Clone, Copy)]
pub struct Expectations<'a> {
    /// The configured issuer; `iss` must equal it exactly.
    pub issuer: &'a str,
    /// The client id; `aud` must contain it.
    pub client_id: &'a str,
    /// SHA-256 (hex) of the nonce this request sent, when it sent one.
    pub nonce_sha256: Option<&'a str>,
    /// Now, in unix seconds.
    pub now: i64,
    /// Allowed clock difference, in seconds.
    pub skew: i64,
}

/// A verified id_token.
#[derive(Debug, Clone, PartialEq)]
pub struct IdToken {
    /// `sub`.
    pub subject: String,
    /// `email`, when asserted.
    pub email: Option<String>,
    /// `email_verified`, when asserted (a string `"true"`/`"false"` is read as the boolean).
    pub email_verified: Option<bool>,
    /// `name`, when asserted.
    pub name: Option<String>,
    /// Every claim, for the configured groups claim.
    pub claims: Map<String, Value>,
}

/// Why a token was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JwtError {
    /// Not a compact JWS, or not JSON inside.
    #[error("id_token malformed: {0}")]
    Malformed(String),
    /// An algorithm this server does not accept.
    #[error("id_token algorithm refused: {0}")]
    Algorithm(String),
    /// No key in the JWKS matches; the caller may fetch the JWKS again once (rotation).
    #[error("id_token key unknown: {0}")]
    UnknownKey(String),
    /// The signature does not verify under the key.
    #[error("id_token signature does not verify")]
    Signature,
    /// A claim is missing or does not match.
    #[error("id_token claim refused: {0}")]
    Claims(String),
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

fn decode(part: &str, what: &str) -> Result<Vec<u8>, JwtError> {
    URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| JwtError::Malformed(format!("{what} is not base64url")))
}

/// SHA-256 of `value`, hex.
#[must_use]
pub fn sha256_hex(value: &str) -> String {
    hex::encode(digest(&SHA256, value.as_bytes()))
}

/// Verify `token` against `jwks` and `expect`.
///
/// # Errors
/// [`JwtError`] naming the first check that failed; [`JwtError::UnknownKey`] means a fresh JWKS
/// might hold the key.
pub fn verify(token: &str, jwks: &Jwks, expect: &Expectations<'_>) -> Result<IdToken, JwtError> {
    let mut parts = token.split('.');
    let (Some(header_b64), Some(claims_b64), Some(signature_b64), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(JwtError::Malformed("not a compact JWS".to_string()));
    };
    let header: Header = serde_json::from_slice(&decode(header_b64, "header")?)
        .map_err(|_| JwtError::Malformed("header is not JSON".to_string()))?;
    let kty = match header.alg.as_str() {
        "RS256" => "RSA",
        "ES256" => "EC",
        other => return Err(JwtError::Algorithm(other.to_string())),
    };
    let key = select_key(jwks, &header, kty)?;
    let signature = decode(signature_b64, "signature")?;
    let signing_input = format!("{header_b64}.{claims_b64}");
    verify_signature(key, &header.alg, signing_input.as_bytes(), &signature)?;
    let claims: Map<String, Value> = serde_json::from_slice(&decode(claims_b64, "claims")?)
        .map_err(|_| JwtError::Malformed("claims are not a JSON object".to_string()))?;
    check_claims(claims, expect)
}

fn select_key<'a>(jwks: &'a Jwks, header: &Header, kty: &str) -> Result<&'a Jwk, JwtError> {
    let usable = |key: &&Jwk| {
        key.kty == kty
            && key.key_use.as_deref().is_none_or(|value| value == "sig")
            && key.alg.as_deref().is_none_or(|alg| alg == header.alg)
    };
    match header.kid.as_deref() {
        Some(kid) => jwks
            .keys
            .iter()
            .filter(usable)
            .find(|key| key.kid.as_deref() == Some(kid))
            .ok_or_else(|| JwtError::UnknownKey(kid.to_string())),
        // No kid: only an unambiguous key is acceptable.
        None => {
            let mut candidates = jwks.keys.iter().filter(usable);
            match (candidates.next(), candidates.next()) {
                (Some(key), None) => Ok(key),
                (None, _) => Err(JwtError::UnknownKey("(no kid)".to_string())),
                (Some(_), Some(_)) => Err(JwtError::UnknownKey(
                    "the token names no kid and the JWKS has several keys".to_string(),
                )),
            }
        }
    }
}

fn verify_signature(
    key: &Jwk,
    alg: &str,
    message: &[u8],
    signature: &[u8],
) -> Result<(), JwtError> {
    let field = |value: &Option<String>, name: &str| {
        value
            .as_deref()
            .ok_or_else(|| JwtError::Malformed(format!("JWK has no {name}")))
            .and_then(|value| decode(value, name))
    };
    let verified = match alg {
        "RS256" => {
            let n = field(&key.n, "n")?;
            let e = field(&key.e, "e")?;
            RsaPublicKeyComponents { n: &n, e: &e }
                .verify(&RSA_PKCS1_2048_8192_SHA256, message, signature)
                .is_ok()
        }
        "ES256" => {
            if key.crv.as_deref() != Some("P-256") {
                return Err(JwtError::Algorithm("ES256 needs a P-256 key".to_string()));
            }
            let x = field(&key.x, "x")?;
            let y = field(&key.y, "y")?;
            if x.len() != 32 || y.len() != 32 {
                return Err(JwtError::Malformed(
                    "P-256 coordinates are 32 bytes".to_string(),
                ));
            }
            let mut point = Vec::with_capacity(65);
            point.push(0x04);
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &point)
                .verify(message, signature)
                .is_ok()
        }
        other => return Err(JwtError::Algorithm(other.to_string())),
    };
    if verified {
        Ok(())
    } else {
        Err(JwtError::Signature)
    }
}

fn number(claims: &Map<String, Value>, name: &str) -> Result<Option<i64>, JwtError> {
    match claims.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .or_else(|| value.as_f64().map(|float| float as i64))
            .map(Some)
            .ok_or_else(|| JwtError::Claims(format!("{name} is not a number"))),
    }
}

fn text(claims: &Map<String, Value>, name: &str) -> Option<String> {
    claims.get(name).and_then(Value::as_str).map(str::to_string)
}

fn check_claims(
    claims: Map<String, Value>,
    expect: &Expectations<'_>,
) -> Result<IdToken, JwtError> {
    if text(&claims, "iss").as_deref() != Some(expect.issuer) {
        return Err(JwtError::Claims(
            "iss is not the configured issuer".to_string(),
        ));
    }
    let audiences: Vec<String> = match claims.get("aud") {
        Some(Value::String(one)) => vec![one.clone()],
        Some(Value::Array(many)) => many
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => return Err(JwtError::Claims("aud is missing".to_string())),
    };
    if !audiences.iter().any(|aud| aud == expect.client_id) {
        return Err(JwtError::Claims(
            "aud does not name this client".to_string(),
        ));
    }
    let azp = text(&claims, "azp");
    if (audiences.len() > 1 && azp.is_none())
        || azp.as_deref().is_some_and(|azp| azp != expect.client_id)
    {
        return Err(JwtError::Claims("azp is not this client".to_string()));
    }
    let exp =
        number(&claims, "exp")?.ok_or_else(|| JwtError::Claims("exp is missing".to_string()))?;
    if expect.now >= exp.saturating_add(expect.skew) {
        return Err(JwtError::Claims("the token has expired".to_string()));
    }
    let iat =
        number(&claims, "iat")?.ok_or_else(|| JwtError::Claims("iat is missing".to_string()))?;
    if iat > expect.now.saturating_add(expect.skew) {
        return Err(JwtError::Claims("iat is in the future".to_string()));
    }
    if number(&claims, "nbf")?.is_some_and(|nbf| nbf > expect.now.saturating_add(expect.skew)) {
        return Err(JwtError::Claims("the token is not valid yet".to_string()));
    }
    if let Some(expected) = expect.nonce_sha256 {
        let presented = text(&claims, "nonce")
            .ok_or_else(|| JwtError::Claims("nonce is missing".to_string()))?;
        if sha256_hex(&presented) != expected {
            return Err(JwtError::Claims("nonce is not this request's".to_string()));
        }
    }
    let subject = text(&claims, "sub")
        .filter(|sub| !sub.is_empty())
        .ok_or_else(|| JwtError::Claims("sub is missing".to_string()))?;
    let email_verified = match claims.get("email_verified") {
        Some(Value::Bool(flag)) => Some(*flag),
        Some(Value::String(flag)) => Some(flag.eq_ignore_ascii_case("true")),
        _ => None,
    };
    Ok(IdToken {
        subject,
        email: text(&claims, "email"),
        email_verified,
        name: text(&claims, "name"),
        claims,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
    use serde_json::json;

    struct Signer {
        pair: EcdsaKeyPair,
        rng: SystemRandom,
    }

    impl Signer {
        fn new() -> Self {
            let rng = SystemRandom::new();
            let pkcs8 =
                EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).expect("key");
            let pair =
                EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                    .expect("pair");
            Self { pair, rng }
        }

        fn jwk(&self, kid: &str) -> Jwk {
            let public = self.pair.public_key().as_ref();
            Jwk {
                kty: "EC".into(),
                kid: Some(kid.into()),
                crv: Some("P-256".into()),
                x: Some(URL_SAFE_NO_PAD.encode(&public[1..33])),
                y: Some(URL_SAFE_NO_PAD.encode(&public[33..65])),
                ..Jwk::default()
            }
        }

        fn sign(&self, header: &Value, claims: &Value) -> String {
            let input = format!(
                "{}.{}",
                URL_SAFE_NO_PAD.encode(header.to_string()),
                URL_SAFE_NO_PAD.encode(claims.to_string())
            );
            let signature = self.pair.sign(&self.rng, input.as_bytes()).expect("sign");
            format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()))
        }
    }

    const NOW: i64 = 1_790_000_000;

    fn claims() -> Value {
        json!({
            "iss": "https://idp.test", "aud": "client-1", "sub": "u-1", "email": "a@corp.test",
            "iat": NOW - 10, "exp": NOW + 300, "nonce": "n-1",
        })
    }

    fn expect(nonce: &str) -> (String, i64) {
        (sha256_hex(nonce), NOW)
    }

    fn run(token: &str, jwks: &Jwks, nonce_hash: &str) -> Result<IdToken, JwtError> {
        verify(
            token,
            jwks,
            &Expectations {
                issuer: "https://idp.test",
                client_id: "client-1",
                nonce_sha256: Some(nonce_hash),
                now: NOW,
                skew: 60,
            },
        )
    }

    #[test]
    fn a_well_formed_es256_token_verifies_and_every_claim_rule_refuses() {
        let signer = Signer::new();
        let jwks = Jwks {
            keys: vec![signer.jwk("k1")],
        };
        let header = json!({"alg": "ES256", "kid": "k1"});
        let (nonce_hash, _) = expect("n-1");
        let token = run(&signer.sign(&header, &claims()), &jwks, &nonce_hash).expect("verifies");
        assert_eq!(token.subject, "u-1");
        assert_eq!(token.email.as_deref(), Some("a@corp.test"));

        let mutate = |key: &str, value: Value| {
            let mut claims = claims();
            claims[key] = value;
            signer.sign(&header, &claims)
        };
        for (token, why) in [
            (mutate("iss", json!("https://evil.test")), "issuer"),
            (mutate("aud", json!("other")), "audience"),
            (
                mutate("aud", json!(["client-1", "other"])),
                "several audiences without azp",
            ),
            (mutate("azp", json!("other")), "azp"),
            (mutate("exp", json!(NOW - 61)), "expired"),
            (mutate("iat", json!(NOW + 120)), "issued in the future"),
            (mutate("nbf", json!(NOW + 120)), "not yet valid"),
            (mutate("nonce", json!("n-2")), "another request's nonce"),
            (mutate("sub", json!("")), "no subject"),
        ] {
            assert!(
                matches!(run(&token, &jwks, &nonce_hash), Err(JwtError::Claims(_))),
                "{why} must be refused"
            );
        }
        // Inside the skew is still valid.
        assert!(run(&mutate("exp", json!(NOW - 30)), &jwks, &nonce_hash).is_ok());
    }

    #[test]
    fn algorithm_key_and_signature_substitutions_are_refused() {
        let signer = Signer::new();
        let other = Signer::new();
        let jwks = Jwks {
            keys: vec![signer.jwk("k1")],
        };
        let (nonce_hash, _) = expect("n-1");
        for alg in ["none", "HS256", "RS512", "PS256"] {
            let token = signer.sign(&json!({"alg": alg, "kid": "k1"}), &claims());
            assert!(
                matches!(run(&token, &jwks, &nonce_hash), Err(JwtError::Algorithm(_))),
                "{alg}"
            );
        }
        // Signed by a key the issuer does not publish, under the published kid.
        let forged = other.sign(&json!({"alg": "ES256", "kid": "k1"}), &claims());
        assert_eq!(run(&forged, &jwks, &nonce_hash), Err(JwtError::Signature));
        // A kid the JWKS does not hold: the caller may refetch.
        let rotated = signer.sign(&json!({"alg": "ES256", "kid": "k2"}), &claims());
        assert!(matches!(
            run(&rotated, &jwks, &nonce_hash),
            Err(JwtError::UnknownKey(_))
        ));
        // A tampered payload under a genuine signature.
        let genuine = signer.sign(&json!({"alg": "ES256", "kid": "k1"}), &claims());
        let mut parts: Vec<&str> = genuine.split('.').collect();
        let mut tampered = claims();
        tampered["sub"] = json!("someone-else");
        let body = URL_SAFE_NO_PAD.encode(tampered.to_string());
        parts[1] = &body;
        assert_eq!(
            run(&parts.join("."), &jwks, &nonce_hash),
            Err(JwtError::Signature)
        );
        // A key declared for another algorithm or use is not a candidate.
        let mut declared = signer.jwk("k1");
        declared.alg = Some("RS256".into());
        let token = signer.sign(&json!({"alg": "ES256", "kid": "k1"}), &claims());
        assert!(matches!(
            run(
                &token,
                &Jwks {
                    keys: vec![declared]
                },
                &nonce_hash
            ),
            Err(JwtError::UnknownKey(_))
        ));
        let mut encryption = signer.jwk("k1");
        encryption.key_use = Some("enc".into());
        assert!(matches!(
            run(
                &token,
                &Jwks {
                    keys: vec![encryption]
                },
                &nonce_hash
            ),
            Err(JwtError::UnknownKey(_))
        ));
        assert!(matches!(
            run("a.b", &jwks, &nonce_hash),
            Err(JwtError::Malformed(_))
        ));
    }
}
