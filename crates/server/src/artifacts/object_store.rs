//! S3-compatible object storage for artifact and evidence bytes (DOSSIER.md §9).
//!
//! Object bytes are immutable per version and content-addressed: the object key ends
//! with the SHA-256 digest of the bytes it holds, so a different payload can never
//! overwrite an existing version. Keys are tenant-prefixed
//! (`tenants/<tenant_id>/…`), every write requests server-side encryption
//! (`x-amz-server-side-encryption: AES256`) and payloads above the multipart
//! threshold use the S3 multipart protocol.
//!
//! [`ObjectStore`] is intentionally narrow (put/get/exists/delete) so the metadata
//! authority can be tested without a live bucket; [`S3ObjectStore`] is the real
//! MinIO/S3 implementation.
//!
//! Boundary environment variables (dev defaults come from `config/dev.yaml` and
//! `scripts/dev/_common.sh`; the secret is a documented dev-only default):
//!
//! * `QUANSIO_TEST_MINIO_ENDPOINT` — default `http://127.0.0.1:59010`
//! * `QUANSIO_TEST_MINIO_REGION` — default `us-east-1`
//! * `QUANSIO_TEST_MINIO_BUCKET` — default `quansio-dev`
//! * `QUANSIO_TEST_MINIO_ACCESS_KEY` — default `quansio-dev`
//! * `QUANSIO_TEST_MINIO_SECRET_KEY` — default `quansio-dev-only` (dev only)
//! * `QUANSIO_TEST_MINIO_SESSION_TOKEN` — optional
//! * `QUANSIO_TEST_MINIO_MULTIPART_THRESHOLD_BYTES` — default 8 MiB
//! * `QUANSIO_TEST_MINIO_PART_SIZE_BYTES` — default 8 MiB

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::Utc;
use quansio_core::{CanonicalId, Digest, Prefix};

use crate::artifacts::sigv4::{
    canonical_query, sha256_hex, sign_request, uri_encode, CanonicalRequest, Credentials,
};

/// Tenant prefix every canonical object key starts with.
pub const OBJECT_KEY_ROOT: &str = "tenants";

/// Default payload size above which multipart upload is used (8 MiB).
pub const DEFAULT_MULTIPART_THRESHOLD: usize = 8 * 1024 * 1024;

/// Default multipart part size (8 MiB, above the S3 5 MiB minimum part size).
pub const DEFAULT_MULTIPART_PART_SIZE: usize = 8 * 1024 * 1024;

/// Dev-stack endpoint from `config/dev.yaml`.
const DEV_ENDPOINT: &str = "http://127.0.0.1:59010";
/// Dev-stack region (MinIO ignores the value but SigV4 requires one).
const DEV_REGION: &str = "us-east-1";
/// Dev-stack bucket created by `infra/compose/compose.yaml`.
const DEV_BUCKET: &str = "quansio-dev";
/// Dev-only access key from `config/dev.yaml`.
const DEV_ACCESS_KEY: &str = "quansio-dev";
/// Dev-only secret from `scripts/dev/_common.sh`; never a production credential.
const DEV_SECRET_KEY: &str = "quansio-dev-only";
/// Server-side encryption algorithm every write requests.
const SSE_ALGORITHM: &str = "AES256";

/// Errors from the object-storage boundary.
#[derive(Debug, thiserror::Error)]
pub enum ObjectStoreError {
    /// The endpoint could not be reached or the transport failed.
    #[error("object store transport failure: {0}")]
    Transport(String),
    /// The object does not exist.
    #[error("object not found: {0}")]
    NotFound(String),
    /// The store rejected the request.
    #[error("object store rejected {method} {key}: HTTP {status} {code} {message}")]
    Rejected {
        /// HTTP method.
        method: String,
        /// Object key (or bucket URI).
        key: String,
        /// HTTP status.
        status: u16,
        /// S3 error code, when the body carried one.
        code: String,
        /// S3 error message.
        message: String,
    },
    /// The key is not a canonical tenant-prefixed Quansio object key.
    #[error("invalid object key: {0}")]
    InvalidKey(String),
    /// The store replied in a shape the client cannot use.
    #[error("invalid object store response: {0}")]
    InvalidResponse(String),
    /// The client configuration is unusable.
    #[error("object store configuration: {0}")]
    Config(String),
}

/// A validated, tenant-prefixed object key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectKey(String);

impl ObjectKey {
    /// Key for one immutable artifact version's bytes.
    #[must_use]
    pub fn artifact_version(
        tenant_id: &str,
        artifact_id: &str,
        version_id: &str,
        digest: &Digest,
    ) -> Self {
        Self(format!(
            "{OBJECT_KEY_ROOT}/{tenant_id}/artifacts/{artifact_id}/versions/{version_id}/{digest}"
        ))
    }

    /// Key for one immutable evidence object's bytes.
    #[must_use]
    pub fn evidence(tenant_id: &str, evidence_id: &str, digest: &Digest) -> Self {
        Self(format!(
            "{OBJECT_KEY_ROOT}/{tenant_id}/evidence/{evidence_id}/{digest}"
        ))
    }

    /// Parse and validate a key read back from metadata.
    ///
    /// # Errors
    /// Returns [`ObjectStoreError::InvalidKey`] when the key is not tenant-prefixed,
    /// carries a non-canonical tenant id, or contains a path traversal segment.
    pub fn parse(value: &str) -> Result<Self, ObjectStoreError> {
        let invalid = || ObjectStoreError::InvalidKey(value.to_string());
        if value.starts_with('/') || value.split('/').any(|segment| segment == "..") {
            return Err(invalid());
        }
        let mut segments = value.split('/');
        if segments.next() != Some(OBJECT_KEY_ROOT) {
            return Err(invalid());
        }
        let tenant = segments.next().ok_or_else(invalid)?;
        CanonicalId::parse_typed(tenant, Prefix::Tenant)
            .map_err(|_| ObjectStoreError::InvalidKey(value.to_string()))?;
        if segments.next().is_none() {
            return Err(invalid());
        }
        Ok(Self(value.to_string()))
    }

    /// The key as stored in metadata and sent on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The tenant id encoded in the key's second segment.
    #[must_use]
    pub fn tenant_id(&self) -> &str {
        self.0
            .split('/')
            .nth(1)
            .expect("validated keys always carry a tenant segment")
    }
}

/// Narrow object-storage contract: metadata logic depends on this, not on S3.
#[async_trait]
pub trait ObjectStore: Send + Sync {
    /// Store bytes under `key`, requesting server-side encryption.
    ///
    /// # Errors
    /// Returns [`ObjectStoreError`] when the store is unreachable or rejects the write.
    async fn put(
        &self,
        key: &ObjectKey,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<(), ObjectStoreError>;

    /// Fetch the exact bytes stored under `key`.
    ///
    /// # Errors
    /// Returns [`ObjectStoreError::NotFound`] when the object is absent.
    async fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, ObjectStoreError>;

    /// Whether an object exists.
    ///
    /// # Errors
    /// Returns [`ObjectStoreError`] when the store is unreachable.
    async fn exists(&self, key: &ObjectKey) -> Result<bool, ObjectStoreError>;

    /// Remove an object. Missing objects are not an error (idempotent deletion).
    ///
    /// # Errors
    /// Returns [`ObjectStoreError`] when the store is unreachable or rejects the call.
    async fn delete(&self, key: &ObjectKey) -> Result<(), ObjectStoreError>;
}

/// S3/MinIO client configuration.
#[derive(Debug, Clone)]
pub struct S3Config {
    /// Endpoint root, for example `http://127.0.0.1:59010`.
    pub endpoint: String,
    /// SigV4 region.
    pub region: String,
    /// Bucket holding Quansio objects.
    pub bucket: String,
    /// Access key id.
    pub access_key: String,
    /// Secret access key.
    pub secret_key: String,
    /// Optional temporary session token.
    pub session_token: Option<String>,
    /// Payload size above which multipart upload is used.
    pub multipart_threshold: usize,
    /// Multipart part size.
    pub multipart_part_size: usize,
}

impl S3Config {
    /// Read the boundary configuration from `QUANSIO_TEST_MINIO_*` with the dev-stack
    /// defaults documented in the module header.
    ///
    /// # Errors
    /// Returns [`ObjectStoreError::Config`] when a numeric override does not parse.
    pub fn from_env() -> Result<Self, ObjectStoreError> {
        let parse_size = |name: &str, default: usize| -> Result<usize, ObjectStoreError> {
            match std::env::var(name) {
                Ok(value) if !value.is_empty() => value
                    .parse()
                    .map_err(|_| ObjectStoreError::Config(format!("{name} is not a byte count"))),
                _ => Ok(default),
            }
        };
        Ok(Self {
            endpoint: env_or("QUANSIO_TEST_MINIO_ENDPOINT", DEV_ENDPOINT),
            region: env_or("QUANSIO_TEST_MINIO_REGION", DEV_REGION),
            bucket: env_or("QUANSIO_TEST_MINIO_BUCKET", DEV_BUCKET),
            access_key: env_or("QUANSIO_TEST_MINIO_ACCESS_KEY", DEV_ACCESS_KEY),
            secret_key: env_or("QUANSIO_TEST_MINIO_SECRET_KEY", DEV_SECRET_KEY),
            session_token: std::env::var("QUANSIO_TEST_MINIO_SESSION_TOKEN")
                .ok()
                .filter(|value| !value.is_empty()),
            multipart_threshold: parse_size(
                "QUANSIO_TEST_MINIO_MULTIPART_THRESHOLD_BYTES",
                DEFAULT_MULTIPART_THRESHOLD,
            )?,
            multipart_part_size: parse_size(
                "QUANSIO_TEST_MINIO_PART_SIZE_BYTES",
                DEFAULT_MULTIPART_PART_SIZE,
            )?,
        })
    }
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Real S3/MinIO object-store client.
pub struct S3ObjectStore {
    client: reqwest::Client,
    config: S3Config,
    credentials: Credentials,
    base_url: String,
    authority: String,
}

impl S3ObjectStore {
    /// Build a client for `config`.
    ///
    /// # Errors
    /// Returns [`ObjectStoreError::Config`] when the endpoint is not an absolute
    /// HTTP(S) URL or the HTTP client cannot be constructed.
    pub fn new(config: S3Config) -> Result<Self, ObjectStoreError> {
        let (base_url, authority) = split_endpoint(&config.endpoint)?;
        let client = reqwest::Client::builder()
            .build()
            .map_err(|error| ObjectStoreError::Config(error.to_string()))?;
        Ok(Self {
            client,
            credentials: Credentials {
                access_key: config.access_key.clone(),
                secret_key: config.secret_key.clone(),
                session_token: config.session_token.clone(),
            },
            config,
            base_url,
            authority,
        })
    }

    /// Build a client from `QUANSIO_TEST_MINIO_*` (see the module header).
    ///
    /// # Errors
    /// Returns [`ObjectStoreError`] when the boundary configuration is unusable.
    pub fn from_env() -> Result<Self, ObjectStoreError> {
        Self::new(S3Config::from_env()?)
    }

    /// The bucket this client writes to.
    #[must_use]
    pub fn bucket(&self) -> &str {
        &self.config.bucket
    }

    fn canonical_uri(&self, key: Option<&str>) -> String {
        match key {
            Some(key) => format!(
                "/{}/{}",
                uri_encode(&self.config.bucket, true),
                uri_encode(key, false)
            ),
            None => format!("/{}", uri_encode(&self.config.bucket, true)),
        }
    }

    async fn send(
        &self,
        method: reqwest::Method,
        key: Option<&str>,
        query: &[(String, String)],
        body: Vec<u8>,
        content_type: Option<&str>,
        extra_headers: &[(String, String)],
    ) -> Result<reqwest::Response, ObjectStoreError> {
        let payload_hash = sha256_hex(&body);
        let canonical_uri = self.canonical_uri(key);
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        headers.insert("host".to_string(), self.authority.clone());
        headers.insert("x-amz-content-sha256".to_string(), payload_hash.clone());
        if let Some(content_type) = content_type {
            headers.insert("content-type".to_string(), content_type.to_string());
        }
        for (name, value) in extra_headers {
            headers.insert(name.to_ascii_lowercase(), value.clone());
        }
        let signed = sign_request(
            &self.credentials,
            &self.config.region,
            "s3",
            &CanonicalRequest {
                method: method.as_str(),
                uri: &canonical_uri,
                query,
                headers: &headers,
                payload_hash: &payload_hash,
            },
            Utc::now(),
        );

        let mut url = format!("{}{}", self.base_url, canonical_uri);
        if !query.is_empty() {
            url.push('?');
            url.push_str(&canonical_query(query));
        }

        let mut request = self.client.request(method.clone(), url);
        for (name, value) in &signed {
            request = request.header(name.as_str(), value.as_str());
        }
        if matches!(method, reqwest::Method::PUT | reqwest::Method::POST) {
            request = request.body(body);
        }
        request
            .send()
            .await
            .map_err(|error| ObjectStoreError::Transport(error.to_string()))
    }

    async fn encode_error(
        &self,
        response: reqwest::Response,
        method: &str,
        key: &str,
    ) -> ObjectStoreError {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        let code = extract_tag(&body, "Code").unwrap_or_default();
        let message =
            extract_tag(&body, "Message").unwrap_or_else(|| body.chars().take(200).collect());
        ObjectStoreError::Rejected {
            method: method.to_string(),
            key: key.to_string(),
            status,
            code,
            message,
        }
    }

    async fn create_multipart(
        &self,
        key: &ObjectKey,
        content_type: &str,
    ) -> Result<String, ObjectStoreError> {
        let query = vec![("uploads".to_string(), String::new())];
        let extra = vec![(
            "x-amz-server-side-encryption".to_string(),
            SSE_ALGORITHM.to_string(),
        )];
        let response = self
            .send(
                reqwest::Method::POST,
                Some(key.as_str()),
                &query,
                Vec::new(),
                Some(content_type),
                &extra,
            )
            .await?;
        if !response.status().is_success() {
            return Err(self
                .encode_error(response, "CreateMultipartUpload", key.as_str())
                .await);
        }
        let body = response
            .text()
            .await
            .map_err(|error| ObjectStoreError::Transport(error.to_string()))?;
        extract_tag(&body, "UploadId").ok_or_else(|| {
            ObjectStoreError::InvalidResponse("CreateMultipartUpload returned no UploadId".into())
        })
    }

    async fn upload_parts(
        &self,
        key: &ObjectKey,
        upload_id: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<Vec<(usize, String)>, ObjectStoreError> {
        let part_size = self.config.multipart_part_size.max(1);
        let mut parts = Vec::new();
        for (index, chunk) in bytes.chunks(part_size).enumerate() {
            let number = index + 1;
            let query = vec![
                ("partNumber".to_string(), number.to_string()),
                ("uploadId".to_string(), upload_id.to_string()),
            ];
            let response = self
                .send(
                    reqwest::Method::PUT,
                    Some(key.as_str()),
                    &query,
                    chunk.to_vec(),
                    Some(content_type),
                    &[],
                )
                .await?;
            if !response.status().is_success() {
                return Err(self
                    .encode_error(response, "UploadPart", key.as_str())
                    .await);
            }
            let etag = response
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
                .ok_or_else(|| {
                    ObjectStoreError::InvalidResponse("UploadPart returned no ETag".into())
                })?;
            parts.push((number, etag));
        }
        Ok(parts)
    }

    async fn complete_multipart(
        &self,
        key: &ObjectKey,
        upload_id: &str,
        parts: &[(usize, String)],
    ) -> Result<(), ObjectStoreError> {
        let body = complete_multipart_body(parts);
        let query = vec![("uploadId".to_string(), upload_id.to_string())];
        let response = self
            .send(
                reqwest::Method::POST,
                Some(key.as_str()),
                &query,
                body.into_bytes(),
                Some("application/xml"),
                &[],
            )
            .await?;
        if !response.status().is_success() {
            return Err(self
                .encode_error(response, "CompleteMultipartUpload", key.as_str())
                .await);
        }
        let text = response
            .text()
            .await
            .map_err(|error| ObjectStoreError::Transport(error.to_string()))?;
        if text.contains("<Error>") {
            return Err(ObjectStoreError::InvalidResponse(format!(
                "CompleteMultipartUpload returned an error body: {}",
                text.chars().take(200).collect::<String>()
            )));
        }
        Ok(())
    }

    async fn abort_multipart(&self, key: &ObjectKey, upload_id: &str) {
        let query = vec![("uploadId".to_string(), upload_id.to_string())];
        // Best effort: an abort failure must not mask the original error.
        let _ = self
            .send(
                reqwest::Method::DELETE,
                Some(key.as_str()),
                &query,
                Vec::new(),
                None,
                &[],
            )
            .await;
    }

    async fn put_multipart(
        &self,
        key: &ObjectKey,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<(), ObjectStoreError> {
        let upload_id = self.create_multipart(key, content_type).await?;
        match self
            .upload_parts(key, &upload_id, bytes, content_type)
            .await
        {
            Ok(parts) => match self.complete_multipart(key, &upload_id, &parts).await {
                Ok(()) => Ok(()),
                Err(error) => {
                    self.abort_multipart(key, &upload_id).await;
                    Err(error)
                }
            },
            Err(error) => {
                self.abort_multipart(key, &upload_id).await;
                Err(error)
            }
        }
    }
}

#[async_trait]
impl ObjectStore for S3ObjectStore {
    async fn put(
        &self,
        key: &ObjectKey,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<(), ObjectStoreError> {
        if bytes.len() > self.config.multipart_threshold {
            return self.put_multipart(key, bytes, content_type).await;
        }
        let extra = vec![(
            "x-amz-server-side-encryption".to_string(),
            SSE_ALGORITHM.to_string(),
        )];
        let response = self
            .send(
                reqwest::Method::PUT,
                Some(key.as_str()),
                &[],
                bytes.to_vec(),
                Some(content_type),
                &extra,
            )
            .await?;
        if !response.status().is_success() {
            return Err(self.encode_error(response, "PutObject", key.as_str()).await);
        }
        Ok(())
    }

    async fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, ObjectStoreError> {
        let response = self
            .send(
                reqwest::Method::GET,
                Some(key.as_str()),
                &[],
                Vec::new(),
                None,
                &[],
            )
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(ObjectStoreError::NotFound(key.as_str().to_string()));
        }
        if !response.status().is_success() {
            return Err(self.encode_error(response, "GetObject", key.as_str()).await);
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| ObjectStoreError::Transport(error.to_string()))
    }

    async fn exists(&self, key: &ObjectKey) -> Result<bool, ObjectStoreError> {
        let response = self
            .send(
                reqwest::Method::HEAD,
                Some(key.as_str()),
                &[],
                Vec::new(),
                None,
                &[],
            )
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !response.status().is_success() {
            return Err(self
                .encode_error(response, "HeadObject", key.as_str())
                .await);
        }
        Ok(true)
    }

    async fn delete(&self, key: &ObjectKey) -> Result<(), ObjectStoreError> {
        let response = self
            .send(
                reqwest::Method::DELETE,
                Some(key.as_str()),
                &[],
                Vec::new(),
                None,
                &[],
            )
            .await?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        Err(self
            .encode_error(response, "DeleteObject", key.as_str())
            .await)
    }
}

fn complete_multipart_body(parts: &[(usize, String)]) -> String {
    let mut body = String::from("<CompleteMultipartUpload>");
    for (number, etag) in parts {
        body.push_str(&format!(
            "<Part><PartNumber>{number}</PartNumber><ETag>{}</ETag></Part>",
            xml_escape(etag)
        ));
    }
    body.push_str("</CompleteMultipartUpload>");
    body
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn extract_tag(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// Split an endpoint into a normalized base URL and the SigV4 `host` header value.
fn split_endpoint(endpoint: &str) -> Result<(String, String), ObjectStoreError> {
    let (scheme, rest) = endpoint
        .split_once("://")
        .ok_or_else(|| ObjectStoreError::Config(format!("endpoint lacks a scheme: {endpoint}")))?;
    if scheme != "http" && scheme != "https" {
        return Err(ObjectStoreError::Config(format!(
            "endpoint scheme must be http or https: {endpoint}"
        )));
    }
    let base = endpoint.trim_end_matches('/');
    let authority = rest.split('/').next().unwrap_or_default().to_string();
    if authority.is_empty() {
        return Err(ObjectStoreError::Config(format!(
            "endpoint lacks a host: {endpoint}"
        )));
    }
    Ok((base.to_string(), authority))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest() -> Digest {
        Digest::of(b"payload")
    }

    #[test]
    fn object_keys_are_tenant_prefixed_and_content_addressed() {
        let tenant = "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
        let key = ObjectKey::artifact_version(
            tenant,
            "art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
            "artv_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
            &digest(),
        );
        assert_eq!(
            key.as_str(),
            format!(
                "tenants/{tenant}/artifacts/art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA/versions/artv_01J8Z3K6F1N8VQ2X5W9Y0AAAAA/{}",
                digest()
            )
        );
        assert_eq!(key.tenant_id(), tenant);
        assert_eq!(ObjectKey::parse(key.as_str()).expect("round trip"), key);
    }

    #[test]
    fn object_keys_reject_traversal_and_non_tenant_prefixes() {
        for invalid in [
            "tenants/../etc/passwd",
            "/tenants/tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA/x",
            "artifacts/tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA/x",
            "tenants/ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA/x",
            "tenants",
        ] {
            assert!(
                ObjectKey::parse(invalid).is_err(),
                "{invalid} must be rejected"
            );
        }
    }

    #[test]
    fn endpoint_split_preserves_port_and_rejects_bad_schemes() {
        let (base, authority) = split_endpoint("http://127.0.0.1:59010/").expect("valid");
        assert_eq!(base, "http://127.0.0.1:59010");
        assert_eq!(authority, "127.0.0.1:59010");
        assert!(split_endpoint("ftp://example.com").is_err());
        assert!(split_endpoint("127.0.0.1:59010").is_err());
    }
}
