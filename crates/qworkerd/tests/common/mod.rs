//! Real-boundary fixtures for the hardened browser tests (ADP-001).
//!
//! Real Google Chrome, driven over the DevTools protocol on a loopback port, and real HTTP and HTTPS
//! servers on loopback for it to load. Nothing here stands in for the browser or for the network the
//! browser speaks to: a stale page is a page that really navigated, a download is bytes a real server
//! really sent slowly, and a certificate error is a real TLS handshake against a self-signed
//! certificate.
//!
//! When no Chrome is present the suites report `BLOCKED_EXTERNAL` and return, so a machine without a
//! browser is never a pass. The HTTPS server needs `python3` and `openssl` on the PATH, and its tests
//! report `BLOCKED_EXTERNAL` for that reason alone when either is missing.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use quansio_qworkerd::browser::{
    self, BrowserCallContext, BrowserDriver, BrowserSession, DriverConfig,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The Chrome the tests drive, if this host has one.
pub fn chrome_path() -> Option<&'static str> {
    const CANDIDATES: [&str; 3] = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
    ];
    CANDIDATES
        .into_iter()
        .find(|path| std::path::Path::new(path).exists())
}

/// Report that a suite did not run for want of a real boundary.
pub fn blocked(reason: &str) {
    eprintln!("BLOCKED_EXTERNAL: {reason}; the suite did not run");
}

/// How many Chromes one test binary may have running at once. Fifteen headless browsers launching
/// together starve each other of CPU and some are refused their debugging port; four is what a laptop
/// and a CI runner both sustain.
const MAX_BROWSERS: usize = 4;

static BROWSERS_RUNNING: (Mutex<usize>, std::sync::Condvar) =
    (Mutex::new(0), std::sync::Condvar::new());

fn acquire_browser_slot() {
    let (lock, condvar) = &BROWSERS_RUNNING;
    let mut running = lock.lock().unwrap_or_else(PoisonError::into_inner);
    while *running >= MAX_BROWSERS {
        running = condvar
            .wait(running)
            .unwrap_or_else(PoisonError::into_inner);
    }
    *running += 1;
}

fn release_browser_slot() {
    let (lock, condvar) = &BROWSERS_RUNNING;
    *lock.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
    condvar.notify_one();
}

/// Whether the DevTools endpoint answers a real request, not merely a TCP connect: a Chrome that is
/// about to crash accepts the connection and then refuses everything.
fn endpoint_answers(port: u16) -> bool {
    use std::io::{Read, Write};
    let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    if stream
        .write_all(
            format!(
                "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .is_err()
    {
        return false;
    }
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    response.contains("webSocketDebuggerUrl")
}

/// A running headless Chrome, killed when it goes out of scope.
pub struct Browser {
    child: Child,
    pub port: u16,
    _profile: tempfile::TempDir,
}

impl Browser {
    pub fn launch() -> Option<Self> {
        let chrome = chrome_path()?;
        acquire_browser_slot();
        let launched = Self::launch_unlimited(chrome);
        if launched.is_none() {
            release_browser_slot();
        }
        launched
    }

    fn launch_unlimited(chrome: &str) -> Option<Self> {
        let profile = tempfile::tempdir().expect("profile dir");
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").port()
        };
        let mut command = Command::new(chrome);
        command
            .arg("--headless=new")
            .arg("--disable-gpu")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-background-networking")
            .arg("--disable-sync")
            .arg("--remote-allow-origins=*")
            .arg(format!("--remote-debugging-port={port}"))
            .arg(format!("--user-data-dir={}", profile.path().display()));
        if std::env::var_os("CI").is_some() {
            command.arg("--no-sandbox").arg("--disable-dev-shm-usage");
        }
        let child = command
            .arg("about:blank")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + Duration::from_secs(25);
        while Instant::now() < deadline {
            if endpoint_answers(port) {
                return Some(Self {
                    child,
                    port,
                    _profile: profile,
                });
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let mut child = child;
        let _ = child.kill();
        None
    }

    /// The URL of the browser's page list, for tests that count windows.
    pub async fn page_count(&self) -> usize {
        browser::cdp::pages(self.port)
            .await
            .map_or(0, |pages| pages.len())
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        release_browser_slot();
    }
}

/// What a test site answers for a path.
#[derive(Clone)]
pub enum Route {
    /// A page.
    Html(String),
    /// Raw bytes with headers.
    Bytes {
        content_type: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// Headers announcing `total` bytes, `first` now and `rest` after `delay`.
    Slow {
        headers: Vec<(String, String)>,
        total: usize,
        first: Vec<u8>,
        delay: Duration,
        rest: Vec<u8>,
    },
    /// Headers announcing a body that never comes.
    Hang {
        headers: Vec<(String, String)>,
        total: usize,
    },
    /// An empty 204, for beacons and fetches that only need counting.
    Count,
}

/// A real HTTP server on loopback that counts what is requested.
pub struct Site {
    pub port: u16,
    routes: Arc<Mutex<BTreeMap<String, Route>>>,
    hits: Arc<Mutex<BTreeMap<String, u32>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Site {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Site {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind site");
        let port = listener.local_addr().expect("addr").port();
        let routes: Arc<Mutex<BTreeMap<String, Route>>> = Arc::new(Mutex::new(BTreeMap::new()));
        let hits: Arc<Mutex<BTreeMap<String, u32>>> = Arc::new(Mutex::new(BTreeMap::new()));
        let task = tokio::spawn(accept_connections(
            listener,
            Arc::clone(&routes),
            Arc::clone(&hits),
        ));
        Self {
            port,
            routes,
            hits,
            task,
        }
    }

    pub fn route(&self, path: &str, route: Route) {
        self.routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.to_string(), route);
    }

    pub fn html(&self, path: &str, html: &str) {
        self.route(
            path,
            Route::Html(html.replace("{PORT}", &self.port.to_string())),
        );
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn hits(&self, path: &str) -> u32 {
        self.hits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path)
            .copied()
            .unwrap_or(0)
    }
}

type Routes = Arc<Mutex<BTreeMap<String, Route>>>;
type Hits = Arc<Mutex<BTreeMap<String, u32>>>;

/// Accept connections until the listener is dropped, serving each on its own task.
async fn accept_connections(listener: TcpListener, routes: Routes, hits: Hits) {
    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(serve_connection(
            stream,
            Arc::clone(&routes),
            Arc::clone(&hits),
        ));
    }
}

/// Read one request, count it, and answer it from the route table.
async fn serve_connection(mut stream: tokio::net::TcpStream, routes: Routes, hits: Hits) {
    let mut request = Vec::new();
    let mut chunk = [0u8; 2048];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") && request.len() <= 16_384 {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => request.extend_from_slice(&chunk[..read]),
        }
    }
    let text = String::from_utf8_lossy(&request).into_owned();
    let target = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    let path = target.split('?').next().unwrap_or("/").to_string();
    *hits
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(path.clone())
        .or_insert(0) += 1;
    let route = routes
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&path)
        .cloned();
    let _ = respond(&mut stream, route).await;
}

async fn respond(stream: &mut tokio::net::TcpStream, route: Option<Route>) -> std::io::Result<()> {
    let head = |status: &str, content_type: &str, length: usize, extra: &[(String, String)]| {
        let mut head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {length}\r\nConnection: close\r\n"
        );
        for (name, value) in extra {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        head
    };
    match route {
        None => {
            stream
                .write_all(head("404 Not Found", "text/plain", 0, &[]).as_bytes())
                .await?;
        }
        Some(Route::Count) => {
            stream
                .write_all(head("204 No Content", "text/plain", 0, &[]).as_bytes())
                .await?;
        }
        Some(Route::Html(body)) => {
            stream
                .write_all(head("200 OK", "text/html; charset=utf-8", body.len(), &[]).as_bytes())
                .await?;
            stream.write_all(body.as_bytes()).await?;
        }
        Some(Route::Bytes {
            content_type,
            headers,
            body,
        }) => {
            stream
                .write_all(head("200 OK", &content_type, body.len(), &headers).as_bytes())
                .await?;
            stream.write_all(&body).await?;
        }
        Some(Route::Slow {
            headers,
            total,
            first,
            delay,
            rest,
        }) => {
            stream
                .write_all(head("200 OK", "application/octet-stream", total, &headers).as_bytes())
                .await?;
            stream.write_all(&first).await?;
            stream.flush().await?;
            tokio::time::sleep(delay).await;
            stream.write_all(&rest).await?;
        }
        Some(Route::Hang { headers, total }) => {
            stream
                .write_all(head("200 OK", "application/octet-stream", total, &headers).as_bytes())
                .await?;
            stream.flush().await?;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }
    stream.flush().await
}

/// A real HTTPS server with a self-signed certificate, run by `python3` and made by `openssl`.
pub struct TlsSite {
    child: Child,
    pub port: u16,
    pub certificate_der_sha256: String,
    _dir: tempfile::TempDir,
}

impl Drop for TlsSite {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const TLS_SERVER: &str = r#"
import http.server, ssl, sys
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"<html><head><title>secure page</title></head><body>secure body</body></html>"
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass
server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(sys.argv[1], sys.argv[2])
server.socket = context.wrap_socket(server.socket, server_side=True)
print(server.server_address[1], flush=True)
server.serve_forever()
"#;

impl TlsSite {
    /// Start the server, or `None` when `python3` or `openssl` is missing.
    pub fn start() -> Option<Self> {
        let dir = tempfile::tempdir().ok()?;
        let key = dir.path().join("key.pem");
        let cert = dir.path().join("cert.pem");
        let made = Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "2",
                "-subj",
                "/CN=localhost",
                "-keyout",
            ])
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        if !made.success() {
            return None;
        }
        let der = Command::new("openssl")
            .args(["x509", "-outform", "DER", "-in"])
            .arg(&cert)
            .output()
            .ok()?;
        if !der.status.success() {
            return None;
        }
        let certificate_der_sha256 = quansio_core::Digest::of(&der.stdout).as_str().to_string();
        let mut child = Command::new("python3")
            .arg("-c")
            .arg(TLS_SERVER)
            .arg(&cert)
            .arg(&key)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(stdout), &mut line).ok()?;
        let port: u16 = line.trim().parse().ok()?;
        Some(Self {
            child,
            port,
            certificate_der_sha256,
            _dir: dir,
        })
    }

    pub fn url(&self) -> String {
        format!("https://127.0.0.1:{}/", self.port)
    }

    pub fn origin(&self) -> String {
        format!("https://127.0.0.1:{}", self.port)
    }
}

/// The session id every test uses.
pub const SESSION: &str = "bsn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";

/// A driver for a fresh session over `browser`, journalling into `dir`.
pub fn driver(
    browser: &Browser,
    dir: &std::path::Path,
    tweak: impl FnOnce(&mut DriverConfig),
) -> BrowserDriver {
    let mut config = DriverConfig::new(browser.port, dir);
    config.action_timeout = Duration::from_millis(2500);
    config.settle_timeout = Duration::from_millis(2000);
    tweak(&mut config);
    let session = BrowserSession::open(
        SESSION,
        "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
    );
    BrowserDriver::open(config, session, None).expect("open the driver")
}

/// A driver with a secret port installed.
pub fn driver_with_secrets(
    browser: &Browser,
    dir: &std::path::Path,
    port: browser::secrets::SecretPort,
    tweak: impl FnOnce(&mut DriverConfig),
) -> BrowserDriver {
    let mut config = DriverConfig::new(browser.port, dir);
    config.action_timeout = Duration::from_millis(2500);
    config.settle_timeout = Duration::from_millis(2000);
    tweak(&mut config);
    let session = BrowserSession::open(
        SESSION,
        "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
    );
    BrowserDriver::open(config, session, Some(port)).expect("open the driver")
}

/// The identity of the Nth tool call of a test.
pub fn call(n: u32) -> BrowserCallContext {
    BrowserCallContext {
        run_id: "run_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string(),
        tool_call_id: format!("tc_call{n:04}"),
        action_id: format!("act_{n:04}"),
        approved_tier: None,
    }
}

/// Dispatch a tool and return its wire reply.
pub async fn run(
    driver: &BrowserDriver,
    ctx: &BrowserCallContext,
    tool: &str,
    args: Value,
) -> Value {
    let mut full = args;
    full["session_id"] = json!(SESSION);
    driver.dispatch(tool, &full, ctx).await.to_json()
}

/// Open a tab and navigate it to `url` through the tool surface; returns `(tab_id, revision)`.
pub async fn open_page(driver: &BrowserDriver, n: u32, url: &str) -> (String, String) {
    let reply = run(driver, &call(n), "browser.navigate", json!({ "url": url })).await;
    assert_eq!(reply["status"], "ok", "navigate {url}: {reply}");
    (
        reply["output"]["tab_id"]
            .as_str()
            .expect("tab id")
            .to_string(),
        reply["output"]["document_revision"]
            .as_str()
            .expect("revision")
            .to_string(),
    )
}

/// Observe a tab through the tool surface; returns the reply's output.
pub async fn observe(driver: &BrowserDriver, n: u32, tab_id: &str) -> Value {
    let reply = run(
        driver,
        &call(n),
        "browser.observe",
        json!({ "tab_id": tab_id }),
    )
    .await;
    assert_eq!(reply["status"], "ok", "observe: {reply}");
    reply["output"].clone()
}

/// The ref of the first element named `name`.
pub fn ref_named(observation: &Value, name: &str) -> String {
    observation["elements"]
        .as_array()
        .expect("elements")
        .iter()
        .find(|element| element["name"] == name)
        .unwrap_or_else(|| panic!("no element named {name:?} in {observation}"))["ref"]
        .as_str()
        .expect("ref")
        .to_string()
}

/// Run JavaScript in a tab over a second, independent CDP connection — the page's own world, as a
/// different party would see it.
pub async fn evaluate(browser: &Browser, tab_id: &str, expression: &str) -> Value {
    let pages = browser::cdp::pages(browser.port).await.expect("pages");
    let page = pages
        .into_iter()
        .find(|page| page.id == tab_id)
        .expect("the tab is open");
    let connection = browser::cdp::connect(page).await.expect("connect");
    let result = connection
        .call(
            "Runtime.evaluate",
            json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }),
        )
        .await
        .expect("evaluate");
    result["result"]["value"].clone()
}

/// Wait until `condition` holds or `within` passes.
pub async fn eventually<F, Fut>(within: Duration, mut condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if condition().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}
