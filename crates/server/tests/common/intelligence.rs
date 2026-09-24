//! The real Python intelligence process for tests that cross the Rust ↔ Python boundary.
//!
//! [`start`] runs `python -m intelligence.server` from the `python/` project on loopback TCP with
//! a model catalog whose only chat route is the OpenAI-compatible provider pointed at the dev
//! stack's deterministic stub provider (`QUANSIO_TEST_STUB_PROVIDER_URL`, default
//! `http://127.0.0.1:59020`). Tool schemas come from the repository's `config/tools.yaml`, as in
//! production. Without the stub provider or `uv` it prints a `BLOCKED_EXTERNAL` marker and
//! returns `None`.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// The catalog id of the stub route.
pub const STUB_ROUTE: &str = "openai-compatible-local";
/// The model id the stub route sends on the wire.
pub const STUB_MODEL: &str = "int001-stub-model";

/// The stub provider's base URL.
#[must_use]
pub fn stub_provider() -> String {
    std::env::var("QUANSIO_TEST_STUB_PROVIDER_URL")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:59020".to_string())
}

/// The running Python service, stopped when dropped.
pub struct Service {
    child: Child,
    /// `http://127.0.0.1:<port>`.
    pub endpoint: String,
    _dir: tempfile::TempDir,
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Start the service against the dev stub provider, or report why it cannot run here.
pub async fn start() -> Option<Service> {
    start_with_provider(None).await
}

/// Start the service with its provider at `provider` (the dev stub when `None`).
pub async fn start_with_provider(provider: Option<&str>) -> Option<Service> {
    start_with(Options {
        provider: provider.map(str::to_string),
        ..Options::default()
    })
    .await
}

/// How to start the service.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// The provider's base URL (the dev stub when `None`).
    pub provider: Option<String>,
    /// Extra environment for the process.
    pub env: Vec<(String, String)>,
    /// Where the process's stderr (its JSON log lines) goes; discarded when `None`.
    pub stderr: Option<PathBuf>,
    /// The provider credential the catalog's handle materializes.
    pub credential: Option<String>,
}

/// Start the service with `options`, or report why it cannot run here.
pub async fn start_with(options: Options) -> Option<Service> {
    let provider = options.provider.as_deref();
    let host_port = stub_provider()
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    if provider.is_none()
        && std::net::TcpStream::connect_timeout(&host_port.parse().ok()?, Duration::from_secs(2))
            .is_err()
    {
        println!(
            "BLOCKED_EXTERNAL: the dev stub provider at {host_port} is not reachable (scripts/dev/up)"
        );
        return None;
    }
    if Command::new("uv").arg("--version").output().is_err() {
        println!(
            "BLOCKED_EXTERNAL: uv is not on PATH; the intelligence service runs from the python/ project"
        );
        return None;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let catalog = dir.path().join("models.yaml");
    std::fs::write(
        &catalog,
        format!(
            "version: 1\n\
             providers:\n  openai-compatible:\n    kind: openai_compatible\n    base_url: {}/v1\n    credential_handle: provider/int001-stub\n\
             routing:\n  strategy: deterministic\n  primary: {STUB_ROUTE}\n  fallback_order: []\n\
             models:\n  - id: {STUB_ROUTE}\n    provider: openai-compatible\n    model_id: {STUB_MODEL}\n    capabilities: [tools, streaming]\n    context_window: 32768\n    cost_class: low\n    dlp_eligible: false\n",
            provider.map_or_else(stub_provider, str::to_string).trim_end_matches('/')
        ),
    )
    .expect("catalog");
    let port = TcpListener::bind("127.0.0.1:0")
        .expect("port")
        .local_addr()
        .expect("addr")
        .port();
    let child = Command::new("uv")
        .args(["run", "--project"])
        .arg(root().join("python"))
        .args([
            "python",
            "-m",
            "intelligence.server",
            "--transport",
            "tcp",
            "--host",
            "127.0.0.1",
            "--port",
        ])
        .arg(port.to_string())
        .env("QUANSIO_INTELLIGENCE_MODEL_CATALOG", &catalog)
        .env(
            "QUANSIO_SECRET_PROVIDER_INT001_STUB",
            options
                .credential
                .as_deref()
                .unwrap_or("stub-credential-for-tests"),
        )
        .envs(
            options
                .env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        )
        .stdout(Stdio::null())
        .stderr(match &options.stderr {
            Some(path) => Stdio::from(std::fs::File::create(path).expect("stderr file")),
            None => Stdio::null(),
        })
        .spawn()
        .expect("spawn the intelligence service");
    let service = Service {
        child,
        endpoint: format!("http://127.0.0.1:{port}"),
        _dir: dir,
    };
    // `uv run` resolves the project first; allow a cold start.
    for _ in 0..240 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Some(service);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("the intelligence service did not start listening on {port}");
}
