//! EXEC-007: the cloud KMS key provider is selectable by configuration and seals secrets
//! through the KMS (DOSSIER.md §16 "Secrets at rest").
//!
//! * The composition root picks the provider from configuration: the KMS key when
//!   `QUANSIO_SECRETS_KMS_KEY_ID` is set, the local master key file when
//!   `QUANSIO_SECRETS_KEY_FILE` is, and refuses both.
//! * Protocol conformance: `AwsKmsClient` speaks the KMS JSON protocol (`TrentService.*`,
//!   SigV4) to a local HTTP endpoint that answers like KMS. This proves the wiring and the
//!   wire shape only; it is **not** real-boundary evidence.
//! * Real boundary: `QUANSIO_TEST_AWS_KMS_KEY_ID`, `QUANSIO_TEST_AWS_KMS_REGION`,
//!   `QUANSIO_TEST_AWS_KMS_ACCESS_KEY_ID`, `QUANSIO_TEST_AWS_KMS_SECRET_ACCESS_KEY` (and
//!   optionally `QUANSIO_TEST_AWS_KMS_SESSION_TOKEN`, `QUANSIO_TEST_AWS_KMS_ENDPOINT`) — a
//!   secret sealed and opened through a real AWS KMS key. Absent → `BLOCKED_EXTERNAL`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use base64::Engine as _;
use quansio_core::OsEntropy;
use quansio_machine::secrets::{open, seal, KeyProvider, KmsKeyProvider};
use quansio_server::composition::{key_provider, ServerConfig};
use quansio_server::kms::{AwsKmsClient, KmsConfig};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

fn server_config(kms: Option<KmsConfig>, key_file: Option<String>) -> ServerConfig {
    ServerConfig {
        database_url: "postgres://unused".to_string(),
        migration_database_url: None,
        bind: "127.0.0.1:0".to_string(),
        max_connections: 1,
        object_store: None,
        search_index_root: None,
        web_root: None,
        background_interval: None,
        secrets_key_file: key_file,
        secrets_key_ref: "local:master-v1".to_string(),
        secrets_kms: kms,
        nats_url: None,
        intelligence_endpoint: None,
    }
}

fn kms_config(endpoint: &str) -> KmsConfig {
    KmsConfig::new(
        "alias/quansio-test",
        "eu-west-1",
        Some(endpoint.to_string()),
        "AKIDEXAMPLE",
        "secret-example",
        None,
    )
}

/// A local endpoint answering like KMS: `Encrypt` wraps by prefixing, `Decrypt` refuses a
/// blob it did not wrap. It checks every request is a signed `TrentService` call.
///
/// It runs on its own thread and runtime, like a remote KMS: the caller's runtime waits for
/// the call and must not be the one that answers it.
async fn kms_endpoint(calls: Arc<AtomicUsize>) -> String {
    let (ready, address) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("endpoint runtime")
            .block_on(serve_kms(calls, ready));
    });
    address.recv().expect("endpoint address")
}

async fn serve_kms(calls: Arc<AtomicUsize>, ready: std::sync::mpsc::Sender<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("addr");
    ready
        .send(format!("http://{address}"))
        .expect("send the address");
    {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let calls = Arc::clone(&calls);
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                let mut headers = Vec::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    headers.push(line.trim_end().to_ascii_lowercase());
                }
                let header = |name: &str| {
                    headers.iter().find_map(|line| {
                        line.strip_prefix(&format!("{name}: ")).map(str::to_string)
                    })
                };
                let length: usize = header("content-length")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).await.expect("body");
                calls.fetch_add(1, Ordering::SeqCst);
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let signed = header("authorization").is_some_and(|value| {
                    value.starts_with("aws4-hmac-sha256 credential=akidexample/")
                        && value.contains("/eu-west-1/kms/aws4_request")
                });
                let engine = base64::engine::general_purpose::STANDARD;
                let (status, answer) = match (signed, header("x-amz-target").as_deref()) {
                    (false, _) => (403, json!({ "__type": "UnrecognizedClientException" })),
                    (true, Some("trentservice.encrypt")) => {
                        let plain = engine
                            .decode(body["Plaintext"].as_str().unwrap_or_default())
                            .expect("plaintext");
                        let mut wrapped = b"kms-wrapped:".to_vec();
                        wrapped.extend(plain);
                        (
                            200,
                            json!({ "CiphertextBlob": engine.encode(wrapped), "KeyId": body["KeyId"] }),
                        )
                    }
                    (true, Some("trentservice.decrypt")) => {
                        let blob = engine
                            .decode(body["CiphertextBlob"].as_str().unwrap_or_default())
                            .unwrap_or_default();
                        match blob.strip_prefix(b"kms-wrapped:") {
                            Some(plain) => (200, json!({ "Plaintext": engine.encode(plain) })),
                            None => (400, json!({ "__type": "InvalidCiphertextException" })),
                        }
                    }
                    _ => (400, json!({ "__type": "UnknownOperationException" })),
                };
                let text = answer.to_string();
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/x-amz-json-1.1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = write.write_all(response.as_bytes()).await;
            });
        }
    }
}

#[test]
fn the_configured_kms_key_is_the_provider_and_a_key_file_is_the_alternative() {
    let kms = key_provider(&server_config(Some(kms_config("http://127.0.0.1:1")), None))
        .expect("kms")
        .expect("a provider");
    assert_eq!(
        kms.key_ref(),
        "alias/quansio-test",
        "secrets record the KMS key"
    );

    let dir = std::env::temp_dir().join(format!("quansio-kms-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let file = dir.join("master.key");
    std::fs::write(
        &file,
        base64::engine::general_purpose::STANDARD.encode([7u8; 32]),
    )
    .expect("key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).expect("mode");
    }
    let local = key_provider(&server_config(None, Some(file.display().to_string())))
        .expect("local")
        .expect("a provider");
    assert_eq!(local.key_ref(), "local:master-v1");

    let both = key_provider(&server_config(
        Some(kms_config("http://127.0.0.1:1")),
        Some(file.display().to_string()),
    ));
    assert!(
        both.is_err(),
        "two providers would seal secrets the other cannot open"
    );
    assert!(key_provider(&server_config(None, None))
        .expect("none")
        .is_none());
    std::fs::remove_dir_all(&dir).ok();
}

/// Called from inside an async runtime, as the secret broker is: the synchronous KMS call
/// must neither block that runtime into a deadlock nor panic on a nested runtime.
#[tokio::test]
async fn a_secret_is_sealed_and_opened_through_the_kms_protocol() {
    let calls = Arc::new(AtomicUsize::new(0));
    let endpoint = kms_endpoint(Arc::clone(&calls)).await;
    let provider = KmsKeyProvider::new(
        AwsKmsClient::new(kms_config(&endpoint)),
        "alias/quansio-test",
    );
    let sealed = seal(&provider, b"ghp_example-token", &OsEntropy).expect("seal");
    assert_eq!(sealed.key_ref, "alias/quansio-test");
    let opened = open(&provider, &sealed.ciphertext).expect("open");
    assert_eq!(opened, b"ghp_example-token");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "one wrap, one unwrap, both at the KMS"
    );

    // A frame whose data key the KMS did not wrap is refused, not opened.
    let mut tampered = sealed.ciphertext.clone();
    let length_at = 1 + 12;
    let wrapped_at = length_at + 2;
    tampered[wrapped_at] ^= 0xff;
    assert!(open(&provider, &tampered).is_err());

    // Unsigned or wrongly signed calls are refused by the KMS and surface as provider errors.
    let wrong = KmsKeyProvider::new(
        AwsKmsClient::new(KmsConfig::new(
            "k",
            "us-east-1",
            Some(endpoint),
            "OTHER",
            "x",
            None,
        )),
        "k",
    );
    let refused = wrong.wrap(&[1; 32]).expect_err("refused");
    assert!(
        refused.to_string().contains("UnrecognizedClientException"),
        "{refused}"
    );
}

#[tokio::test]
async fn a_secret_is_sealed_and_opened_through_a_real_aws_kms_key() {
    let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let (Some(key), Some(region), Some(access), Some(secret)) = (
        var("QUANSIO_TEST_AWS_KMS_KEY_ID"),
        var("QUANSIO_TEST_AWS_KMS_REGION"),
        var("QUANSIO_TEST_AWS_KMS_ACCESS_KEY_ID"),
        var("QUANSIO_TEST_AWS_KMS_SECRET_ACCESS_KEY"),
    ) else {
        eprintln!(
            "BLOCKED_EXTERNAL: QUANSIO_TEST_AWS_KMS_KEY_ID/_REGION/_ACCESS_KEY_ID/_SECRET_ACCESS_KEY are not set; no AWS KMS key to seal under"
        );
        return;
    };
    let config = KmsConfig::new(
        key.clone(),
        region,
        var("QUANSIO_TEST_AWS_KMS_ENDPOINT"),
        access,
        secret,
        var("QUANSIO_TEST_AWS_KMS_SESSION_TOKEN"),
    );
    let provider = KmsKeyProvider::new(AwsKmsClient::new(config), key);
    let sealed = seal(&provider, b"real-boundary-secret", &OsEntropy).expect("seal");
    assert_eq!(
        open(&provider, &sealed.ciphertext).expect("open"),
        b"real-boundary-secret"
    );
}
