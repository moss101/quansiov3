//! The cloud KMS a managed or private-cloud deployment seals secrets under (EXEC-007,
//! DOSSIER.md §16 "Secrets at rest").
//!
//! `quansio_machine::secrets::KmsClient` leaves reaching a KMS to the deployment; this is the
//! deployable's implementation for AWS KMS (and any endpoint speaking its JSON protocol):
//! `TrentService.Encrypt`/`Decrypt` over HTTPS, signed with SigV4. The key-encryption key never
//! leaves the KMS — a data key is sent to be wrapped and comes back wrapped, and is unwrapped
//! there only for the moment the broker needs it.
//!
//! Selected by configuration (`QUANSIO_SECRETS_KMS_KEY_ID`), exclusive with the local master
//! key file; see [`KmsConfig::from_env`].

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine as _;
use quansio_machine::secrets::{KmsClient, SecretError};
use serde_json::{json, Value};

use crate::artifacts::sigv4::{self, CanonicalRequest, Credentials};

/// How long one KMS call may take.
const KMS_TIMEOUT: Duration = Duration::from_secs(10);

/// Where the KMS is and who calls it.
#[derive(Clone)]
pub struct KmsConfig {
    /// The key (id, ARN or alias) data keys are wrapped under; recorded on each secret.
    pub key_id: String,
    /// The KMS region.
    pub region: String,
    /// The endpoint, `https://kms.<region>.amazonaws.com` unless overridden.
    pub endpoint: String,
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
}

impl std::fmt::Debug for KmsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KmsConfig")
            .field("key_id", &self.key_id)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl KmsConfig {
    /// A configuration from explicit values.
    #[must_use]
    pub fn new(
        key_id: impl Into<String>,
        region: impl Into<String>,
        endpoint: Option<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
        session_token: Option<String>,
    ) -> Self {
        let region = region.into();
        Self {
            endpoint: endpoint
                .unwrap_or_else(|| format!("https://kms.{region}.amazonaws.com"))
                .trim_end_matches('/')
                .to_string(),
            key_id: key_id.into(),
            region,
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            session_token,
        }
    }

    /// Read the KMS selection from the environment: `QUANSIO_SECRETS_KMS_KEY_ID` selects it,
    /// `QUANSIO_SECRETS_KMS_REGION` is required with it, `QUANSIO_SECRETS_KMS_ENDPOINT`
    /// overrides the endpoint (a VPC endpoint), and the caller's credentials are the standard
    /// `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and optional `AWS_SESSION_TOKEN`.
    ///
    /// # Errors
    /// Returns `Ok(None)` when no KMS key is configured, and a message naming the missing
    /// variable when one is configured incompletely.
    pub fn from_env() -> Result<Option<Self>, String> {
        let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        let Some(key_id) = var("QUANSIO_SECRETS_KMS_KEY_ID") else {
            return Ok(None);
        };
        let required = |name: &str| {
            var(name).ok_or_else(|| format!("QUANSIO_SECRETS_KMS_KEY_ID is set but {name} is not"))
        };
        Ok(Some(Self::new(
            key_id,
            required("QUANSIO_SECRETS_KMS_REGION")?,
            var("QUANSIO_SECRETS_KMS_ENDPOINT"),
            required("AWS_ACCESS_KEY_ID")?,
            required("AWS_SECRET_ACCESS_KEY")?,
            var("AWS_SESSION_TOKEN"),
        )))
    }
}

/// The AWS KMS client behind `KmsKeyProvider`.
#[derive(Debug, Clone)]
pub struct AwsKmsClient {
    config: KmsConfig,
}

impl AwsKmsClient {
    /// A client for `config`.
    #[must_use]
    pub const fn new(config: KmsConfig) -> Self {
        Self { config }
    }

    /// The configuration in use.
    #[must_use]
    pub const fn config(&self) -> &KmsConfig {
        &self.config
    }

    /// The signed request for one KMS operation: `(url, headers, body)`.
    fn request(&self, operation: &str, body: &Value) -> (String, BTreeMap<String, String>, String) {
        let body = body.to_string();
        let host = self
            .config
            .endpoint
            .split("://")
            .nth(1)
            .unwrap_or(&self.config.endpoint)
            .split('/')
            .next()
            .unwrap_or_default()
            .to_string();
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), host);
        headers.insert(
            "content-type".to_string(),
            "application/x-amz-json-1.1".to_string(),
        );
        headers.insert(
            "x-amz-target".to_string(),
            format!("TrentService.{operation}"),
        );
        let payload_hash = sigv4::sha256_hex(body.as_bytes());
        headers.insert("x-amz-content-sha256".to_string(), payload_hash.clone());
        let signed = sigv4::sign_request(
            &Credentials {
                access_key: self.config.access_key.clone(),
                secret_key: self.config.secret_key.clone(),
                session_token: self.config.session_token.clone(),
            },
            &self.config.region,
            "kms",
            &CanonicalRequest {
                method: "POST",
                uri: "/",
                query: &[],
                headers: &headers,
                payload_hash: &payload_hash,
            },
            chrono::Utc::now(),
        );
        (format!("{}/", self.config.endpoint), signed, body)
    }

    /// Call one operation and return its JSON answer.
    ///
    /// `KmsClient` is synchronous and is called from async request handlers, so the call runs
    /// on its own thread with its own runtime: it never blocks a worker of the caller's
    /// runtime on a runtime-in-runtime, whatever flavour that runtime is.
    fn call(&self, operation: &str, body: &Value) -> Result<Value, SecretError> {
        let (url, headers, body) = self.request(operation, body);
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| provider(format!("KMS runtime: {error}")))?;
                    runtime.block_on(async {
                        let client = reqwest::Client::builder()
                            .timeout(KMS_TIMEOUT)
                            .build()
                            .map_err(|error| provider(format!("KMS client: {error}")))?;
                        let mut request = client.post(&url).body(body);
                        for (name, value) in &headers {
                            if name != "host" {
                                request = request.header(name, value);
                            }
                        }
                        let response = request
                            .send()
                            .await
                            .map_err(|error| provider(format!("KMS {operation}: {error}")))?;
                        let status = response.status();
                        let text = response
                            .text()
                            .await
                            .map_err(|error| provider(format!("KMS {operation}: {error}")))?;
                        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                        if !status.is_success() {
                            let kind = value
                                .get("__type")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown");
                            return Err(provider(format!(
                                "KMS {operation} refused ({status}): {kind}"
                            )));
                        }
                        Ok(value)
                    })
                })
                .join()
                .unwrap_or_else(|_| Err(provider("the KMS call panicked".to_string())))
        })
    }

    fn blob(value: &Value, field: &str) -> Result<Vec<u8>, SecretError> {
        let encoded = value
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| provider(format!("the KMS answer has no {field}")))?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| provider(format!("the KMS {field} is not base64: {error}")))
    }
}

fn provider(detail: String) -> SecretError {
    SecretError::KeyProvider { detail }
}

impl KmsClient for AwsKmsClient {
    fn encrypt(&self, key_ref: &str, plaintext: &[u8]) -> Result<Vec<u8>, SecretError> {
        let answer = self.call(
            "Encrypt",
            &json!({
                "KeyId": key_ref,
                "Plaintext": base64::engine::general_purpose::STANDARD.encode(plaintext),
            }),
        )?;
        Self::blob(&answer, "CiphertextBlob")
    }

    fn decrypt(&self, key_ref: &str, ciphertext: &[u8]) -> Result<Vec<u8>, SecretError> {
        let answer = self.call(
            "Decrypt",
            &json!({
                "KeyId": key_ref,
                "CiphertextBlob": base64::engine::general_purpose::STANDARD.encode(ciphertext),
            }),
        )?;
        Self::blob(&answer, "Plaintext")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> KmsConfig {
        KmsConfig::new(
            "alias/quansio-secrets",
            "eu-west-1",
            None,
            "AKIDEXAMPLE",
            "example-secret-key",
            Some("session".to_string()),
        )
    }

    #[test]
    fn a_request_names_the_operation_and_is_signed_for_kms() {
        let client = AwsKmsClient::new(config());
        let (url, headers, body) = client.request("Encrypt", &json!({ "KeyId": "k" }));
        assert_eq!(url, "https://kms.eu-west-1.amazonaws.com/");
        assert_eq!(headers["host"], "kms.eu-west-1.amazonaws.com");
        assert_eq!(headers["x-amz-target"], "TrentService.Encrypt");
        assert_eq!(headers["content-type"], "application/x-amz-json-1.1");
        assert_eq!(headers["x-amz-security-token"], "session");
        let authorization = &headers["authorization"];
        assert!(authorization.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"));
        assert!(authorization.contains("/eu-west-1/kms/aws4_request"));
        assert!(
            authorization.contains("x-amz-target"),
            "the operation is signed"
        );
        assert_eq!(body, r#"{"KeyId":"k"}"#);
    }

    #[test]
    fn the_configuration_never_prints_its_secret() {
        let printed = format!("{:?}", config());
        assert!(!printed.contains("example-secret") && !printed.contains("session"));
    }

    #[test]
    fn an_endpoint_override_is_used_as_given() {
        let config = KmsConfig::new(
            "k",
            "us-east-1",
            Some("https://vpce-1.kms.us-east-1.vpce.amazonaws.com/".to_string()),
            "a",
            "s",
            None,
        );
        assert_eq!(
            config.endpoint,
            "https://vpce-1.kms.us-east-1.vpce.amazonaws.com"
        );
    }
}
