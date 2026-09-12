//! Minimal AWS Signature Version 4 signer for S3-compatible object storage.
//!
//! The object-storage boundary is deliberately narrow: Quansio needs exactly
//! `PutObject`, `GetObject`, `HeadObject`, `DeleteObject` and the three multipart
//! calls, so signing is implemented here instead of pulling in a full provider SDK.
//! Signing is a pure function of the request, which makes it verifiable against the
//! AWS test vectors without any bucket.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest as _, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Long-lived or temporary credentials used to sign one request.
#[derive(Debug, Clone)]
pub(crate) struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
}

/// Lowercase hex SHA-256 of the exact bytes.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// RFC 3986 encoding with uppercase hex; `/` is preserved for object paths.
pub(crate) fn uri_encode(value: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Canonical query string: keys and values encoded, sorted by encoded key then value.
pub(crate) fn canonical_query(query: &[(String, String)]) -> String {
    let mut encoded: Vec<(String, String)> = query
        .iter()
        .map(|(key, value)| (uri_encode(key, true), uri_encode(value, true)))
        .collect();
    encoded.sort();
    encoded
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// The canonical parts of one request, before signing.
pub(crate) struct CanonicalRequest<'a> {
    /// HTTP method.
    pub method: &'a str,
    /// Canonical URI path.
    pub uri: &'a str,
    /// Raw query parameters (encoded and sorted by the signer).
    pub query: &'a [(String, String)],
    /// Headers to sign; must include `host` and every `x-amz-*` header sent.
    pub headers: &'a BTreeMap<String, String>,
    /// Lowercase hex SHA-256 of the body.
    pub payload_hash: &'a str,
}

/// Sign one request and return the complete header set to send.
///
/// `request.headers` must already contain `host` and every `x-amz-*` header to be
/// signed (`x-amz-content-sha256` is mandatory for S3); keys are lowercased here so
/// the canonical form cannot depend on caller casing. `payload_hash` must equal the
/// value sent in `x-amz-content-sha256` (the empty-string digest for bodyless
/// requests).
pub(crate) fn sign_request(
    credentials: &Credentials,
    region: &str,
    service: &str,
    request: &CanonicalRequest<'_>,
    now: DateTime<Utc>,
) -> BTreeMap<String, String> {
    let CanonicalRequest {
        method,
        uri,
        query,
        headers,
        payload_hash,
    } = request;
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();

    let mut signed: BTreeMap<String, String> = headers
        .iter()
        .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    signed.insert("x-amz-date".to_string(), amz_date.clone());
    if let Some(token) = &credentials.session_token {
        signed.insert("x-amz-security-token".to_string(), token.clone());
    }

    let canonical_headers: String = signed
        .iter()
        .map(|(key, value)| format!("{key}:{value}\n"))
        .collect();
    let signed_headers = signed.keys().cloned().collect::<Vec<_>>().join(";");
    let canonical_query = canonical_query(query);

    let canonical_request = format!(
        "{method}\n{uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );
    let scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let mut key = hmac_sha256(
        format!("AWS4{}", credentials.secret_key).as_bytes(),
        date_stamp.as_bytes(),
    );
    key = hmac_sha256(&key, region.as_bytes());
    key = hmac_sha256(&key, service.as_bytes());
    key = hmac_sha256(&key, b"aws4_request");
    let signature = hex::encode(hmac_sha256(&key, string_to_sign.as_bytes()));

    signed.insert(
        "authorization".to_string(),
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            credentials.access_key
        ),
    );
    signed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> Credentials {
        Credentials {
            access_key: "AKIDEXAMPLE".to_string(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: None,
        }
    }

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .expect("rfc3339")
            .with_timezone(&Utc)
    }

    #[test]
    fn matches_the_aws_documented_signing_example() {
        // AWS "Examples of the complete Signature Version 4 signing process":
        // GET https://iam.amazonaws.com/?Action=ListUsers&Version=2010-05-08
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), "iam.amazonaws.com".to_string());
        headers.insert(
            "content-type".to_string(),
            "application/x-www-form-urlencoded; charset=utf-8".to_string(),
        );
        let query = vec![
            ("Action".to_string(), "ListUsers".to_string()),
            ("Version".to_string(), "2010-05-08".to_string()),
        ];
        let payload_hash = sha256_hex(b"");
        let signed = sign_request(
            &creds(),
            "us-east-1",
            "iam",
            &CanonicalRequest {
                method: "GET",
                uri: "/",
                query: &query,
                headers: &headers,
                payload_hash: &payload_hash,
            },
            at("2015-08-30T12:36:00Z"),
        );
        let authorization = signed.get("authorization").expect("authorization");
        assert!(
            authorization.ends_with(
                "Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
            ),
            "unexpected signature: {authorization}"
        );
        assert!(
            authorization.contains("SignedHeaders=content-type;host;x-amz-date"),
            "unexpected signed headers: {authorization}"
        );
    }

    #[test]
    fn query_encoding_is_rfc3986_and_sorted() {
        let query = vec![
            ("partNumber".to_string(), "2".to_string()),
            ("uploadId".to_string(), "a+b/c=".to_string()),
        ];
        assert_eq!(
            canonical_query(&query),
            "partNumber=2&uploadId=a%2Bb%2Fc%3D"
        );
        assert_eq!(uri_encode("art_01/a b", false), "art_01/a%20b");
        assert_eq!(uri_encode("art_01/a b", true), "art_01%2Fa%20b");
    }
}
