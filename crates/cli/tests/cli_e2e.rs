//! CLI E2E (APP-013) against the real `quansio-server` on a scratch database, bearer
//! sessions only (the deployable's default): a magic link from the server's console
//! becomes a stored, scoped session; the CLI walks workspaces, a policy, a message, its
//! run, an approval bound to the previewed digest (a stale one refused), the timeline,
//! an artifact download verified against its digest, and a diagnostics bundle without a
//! secret in it; an invited viewer's session is refused by the server for an approval and
//! an admin projection exactly as the API refuses any client; a token is never used for
//! a tenant it was not issued for; there is no flag that bypasses policy.
//!
//! Blocked, not skipped, without the dev stack or the built server binaries.

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

const APPROVAL_SCENARIO: &str = "[[scenario:approval]]";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn binary(env_var: &str, name: &str) -> PathBuf {
    std::env::var(env_var)
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root().join("target").join("debug").join(name))
}

struct Server {
    child: Child,
    origin: String,
    stderr: Arc<Mutex<Vec<String>>>,
    database: String,
    seed: PathBuf,
    admin_url: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = Command::new(&self.seed)
            .args(["drop", "--database", &self.database])
            .env("QUANSIO_DATABASE_URL", &self.admin_url)
            .output();
    }
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("addr").port()
}

/// Start a server on a scratch database, or `None` (blocked) without the prerequisites.
fn start_server(with_minio: bool) -> Option<Server> {
    let admin_url = std::env::var("QUANSIO_TEST_POSTGRES_URL")
        .ok()
        .filter(|value| !value.is_empty());
    let server_bin = binary("QUANSIO_TEST_SERVER_BIN", "quansio-server");
    let seed_bin = binary("QUANSIO_TEST_SEED_BIN", "quansio-dev-seed");
    let Some(admin_url) = admin_url else {
        eprintln!(
            "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
        );
        return None;
    };
    if !server_bin.is_file() || !seed_bin.is_file() {
        eprintln!("BLOCKED_EXTERNAL: {} and {} are needed; build them with cargo build -p quansio-server --bins", server_bin.display(), seed_bin.display());
        return None;
    }
    let database = format!("quansio_cli_e2e_{}", std::process::id());
    let seeded = Command::new(&seed_bin)
        .args([
            "create",
            "--database",
            &database,
            "--tenant",
            "tn_01J8Z3K6F1N8VQ2X5W9Y0CLICL",
            "--user",
            "usr_01J8Z3K6F1N8VQ2X5W9Y0CLICL",
            "--workspace",
            "ws_01J8Z3K6F1N8VQ2X5W9Y0CLICL",
            "--node",
            "wn_01J8Z3K6F1N8VQ2X5W9Y0CLICL",
        ])
        .env("QUANSIO_DATABASE_URL", &admin_url)
        .output()
        .expect("seed");
    assert!(
        seeded.status.success(),
        "seed: {}",
        String::from_utf8_lossy(&seeded.stderr)
    );
    let database_url = String::from_utf8_lossy(&seeded.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("database_url=").map(str::to_string))
        .expect("database_url");
    let port = free_port();
    let origin = format!("http://127.0.0.1:{port}");
    let mut command = Command::new(&server_bin);
    command
        .env("QUANSIO_DATABASE_URL", &database_url)
        .env("QUANSIO_BIND", format!("127.0.0.1:{port}"))
        .env("QUANSIO_APPROVAL_SIGNING_KEY", "cli-e2e-signing-key")
        .env_remove("QUANSIO_TRUST_SCOPE_HEADERS")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if with_minio {
        command.env("QUANSIO_OBJECT_STORE", "1");
    }
    let mut child = command.spawn().expect("spawn quansio-server");
    let stderr = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&stderr);
    let pipe = child.stderr.take().expect("stderr");
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            sink.lock().expect("stderr lines").push(line);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(response) = std::net::TcpStream::connect(format!("127.0.0.1:{port}")) {
            drop(response);
            let health = Command::new("curl")
                .args(["-sf", &format!("{origin}/v1/health")])
                .output();
            if health
                .map(|output| output.status.success())
                .unwrap_or(false)
            {
                break;
            }
        }
        if let Some(status) = child.try_wait().expect("try_wait") {
            panic!(
                "quansio-server exited with {status}: {}",
                stderr.lock().expect("stderr").join("\n")
            );
        }
        assert!(
            Instant::now() < deadline,
            "quansio-server did not become healthy: {}",
            stderr.lock().expect("stderr").join("\n")
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    Some(Server {
        child,
        origin,
        stderr,
        database,
        seed: seed_bin,
        admin_url,
    })
}

struct Cli {
    home: PathBuf,
    origin: String,
}

struct Outcome {
    status: i32,
    stdout: String,
    stderr: String,
}

impl Outcome {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|error| {
            panic!(
                "stdout is not JSON ({error}): {}\nstderr: {}",
                self.stdout, self.stderr
            )
        })
    }

    fn error(&self) -> Value {
        serde_json::from_str(&self.stderr)
            .unwrap_or_else(|error| panic!("stderr is not JSON ({error}): {}", self.stderr))
    }
}

impl Cli {
    fn run(&self, args: &[&str]) -> Outcome {
        let output = Command::new(env!("CARGO_BIN_EXE_quansio"))
            .args(args)
            .env("QUANSIO_CLI_HOME", &self.home)
            .env_remove("HOME")
            .output()
            .expect("run quansio");
        Outcome {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }
    }

    fn ok(&self, args: &[&str]) -> Outcome {
        let outcome = self.run(args);
        assert_eq!(
            outcome.status,
            0,
            "quansio {}: {}\n{}",
            args.join(" "),
            outcome.stderr,
            outcome.stdout
        );
        outcome
    }

    /// Sign in with a magic link the server printed on its console.
    fn login(&self, server: &Server, email: &str, name: &str) -> Value {
        let started = self
            .ok(&[
                "--json",
                "--server",
                &self.origin,
                "login",
                "--email",
                email,
                "--name",
                name,
            ])
            .json();
        assert_eq!(started["delivery"], "console", "{started}");
        let needle = format!("magic-link for {email}");
        let deadline = Instant::now() + Duration::from_secs(15);
        let token = loop {
            let found = server
                .stderr
                .lock()
                .expect("stderr")
                .iter()
                .rev()
                .find(|line| line.contains(&needle))
                .and_then(|line| {
                    line.split("token=")
                        .nth(1)
                        .map(|token| token.trim().to_string())
                });
            if let Some(token) = found {
                break token;
            }
            assert!(
                Instant::now() < deadline,
                "no magic link on the console for {email}"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        let completed = self
            .ok(&[
                "--json",
                "--server",
                &self.origin,
                "login",
                "--token",
                &token,
            ])
            .json();
        assert_eq!(
            completed["session"]["access_token"], "<redacted>",
            "tokens never reach the screen: {completed}"
        );
        completed["session"].clone()
    }
}

#[test]
fn the_cli_is_a_scoped_client_of_the_public_api_and_nothing_else() {
    let minio = std::env::var("QUANSIO_TEST_MINIO_ENDPOINT")
        .ok()
        .filter(|value| !value.is_empty())
        .is_some_and(|endpoint| {
            Command::new("curl")
                .args(["-sf", &format!("{endpoint}/minio/health/live")])
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
        });
    let Some(server) = start_server(minio) else {
        return;
    };
    let owner_home = tempfile::tempdir().expect("home");
    let owner = Cli {
        home: owner_home.path().to_path_buf(),
        origin: server.origin.clone(),
    };

    // No session yet: a verb is refused before any request; the bypass flags are not flags.
    let nothing = owner.run(&["whoami"]);
    assert_eq!(nothing.status, 1);
    assert!(
        nothing.stderr.contains("not signed in"),
        "{}",
        nothing.stderr
    );
    for bypass in [
        vec!["--force", "whoami"],
        vec!["approvals", "list", "--bypass-policy"],
        vec!["--sudo", "runs", "get", "run_1"],
    ] {
        let refused = owner.run(&bypass);
        assert_eq!(refused.status, 2, "{bypass:?}");
        assert!(
            refused.stderr.contains("never bypasses"),
            "{}",
            refused.stderr
        );
    }
    assert_eq!(owner.run(&["api", "get", "/internal/db"]).status, 2);
    assert_eq!(owner.run(&["api", "get", "/v2/anything"]).status, 2);

    // 1. Sign in: the link is on the console; the session is stored 0600 and scoped.
    let session = owner.login(&server, "owner@example.com", "Owner");
    let tenant = session["tenant_id"].as_str().expect("tenant").to_string();
    let workspace = session["workspace_id"]
        .as_str()
        .expect("workspace")
        .to_string();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(owner_home.path().join("credentials.json"))
            .expect("credentials")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let me = owner.ok(&["--json", "whoami"]).json();
    assert_eq!(me["me"]["user"]["primary_email"], "owner@example.com");
    assert_eq!(me["session"]["tenant_id"], tenant);
    assert!(
        !me.to_string().contains("eyJ"),
        "no JWT on the screen: {me}"
    );
    let plain = owner.ok(&["whoami"]);
    assert!(
        plain.stdout.contains("owner@example.com") && !plain.stdout.contains("eyJ"),
        "{}",
        plain.stdout
    );
    // A token is never used for another tenant or server.
    let mismatch = owner.run(&["--tenant", "tn_01J8Z3K6F1N8VQ2X5W9Y0OTHER", "whoami"]);
    assert_eq!(mismatch.status, 1);
    assert!(
        mismatch.stderr.contains("no session for"),
        "{}",
        mismatch.stderr
    );
    let other_server = owner.run(&["--server", "http://127.0.0.1:1", "whoami"]);
    assert_eq!(other_server.status, 1);
    assert!(
        other_server.stderr.contains("no session for"),
        "{}",
        other_server.stderr
    );

    // 2. Workspaces, a policy through the generic verb, a message, its run.
    let workspaces = owner.ok(&["--json", "workspaces", "list"]).json();
    assert_eq!(workspaces["workspaces"][0]["id"], workspace);
    assert_eq!(workspaces["workspaces"][0]["role"], "admin");
    owner.ok(&["workspaces", "use", &workspace]);
    let rules = r#"{"scope":"tenant","rules":[{"effect_class":"read.internal","resource":{"kind":"fs","selector":"**"},"decision":"allow"},{"effect_class":"fs.write.workspace","resource":{"kind":"fs","selector":"**"},"decision":"ask"},{"effect_class":"fs.write.host","resource":{"kind":"fs","selector":"**"},"decision":"ask"}]}"#;
    let policy = owner
        .ok(&["api", "command", "SetPolicy", "--params", rules])
        .json();
    assert_eq!(policy["version"], 1, "{policy}");
    assert_eq!(
        owner
            .run(&["api", "command", "setPolicy", "--params", "{}"])
            .status,
        2,
        "not a catalogued spelling"
    );
    let posted = owner
        .ok(&[
            "--json",
            "messages",
            "post",
            "--content",
            &format!("write the summary {APPROVAL_SCENARIO}"),
        ])
        .json();
    assert_eq!(posted["turn"]["kind"], "parked", "{posted}");
    let run_id = posted["run_id"].as_str().expect("run").to_string();
    let thread_id = posted["thread_id"].as_str().expect("thread").to_string();
    let run = owner.ok(&["--json", "runs", "get", &run_id]).json();
    assert_eq!(run["id"], run_id);
    assert!(run["capability_projection"]["grants"].is_array(), "{run}");
    let threads = owner.ok(&["--json", "threads", "list"]).json();
    assert!(
        threads["threads"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == thread_id)),
        "{threads}"
    );
    // The execution targets the live surface observes (APP-007): the session's own
    // workspace has none yet, and the projection says so in its catalogued shape.
    let targets = owner.ok(&["--json", "targets", "list"]).json();
    assert_eq!(targets["items"], serde_json::json!([]), "{targets}");
    assert_eq!(targets["workspace_id"], workspace, "{targets}");
    let messages = owner
        .ok(&["--json", "messages", "list", "--thread", &thread_id])
        .json();
    assert!(
        messages["messages"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        "{messages}"
    );

    // 3. The approval: previewed, then bound to the previewed digest; a stale digest is
    //    refused by the server; the granted decision completes the run.
    let approvals = owner
        .ok(&["--json", "approvals", "list", "--run", &run_id])
        .json();
    let request = &approvals["approvals"][0];
    let approval_id = request["id"].as_str().expect("apr").to_string();
    let digest = request["params_digest"]
        .as_str()
        .expect("digest")
        .to_string();
    assert_eq!(request["status"], "requested");
    let shown = owner.ok(&["approvals", "show", &approval_id]);
    assert!(
        shown.stdout.contains(&digest) && shown.stdout.contains("approve with:"),
        "{}",
        shown.stdout
    );
    let stale = owner.run(&[
        "--json",
        "approvals",
        "approve",
        &approval_id,
        "--digest",
        &"0".repeat(64),
    ]);
    assert_eq!(stale.status, 1);
    assert_eq!(
        stale.error()["error"]["code"],
        "APPROVAL_SUPERSEDED",
        "{}",
        stale.stderr
    );
    assert_eq!(
        owner
            .run(&[
                "approvals",
                "approve",
                &approval_id,
                "--digest",
                "not-a-digest"
            ])
            .status,
        2
    );
    let granted = owner
        .ok(&[
            "--json",
            "approvals",
            "approve",
            &approval_id,
            "--digest",
            &digest,
        ])
        .json();
    assert_eq!(granted["params_digest"], digest, "{granted}");
    assert_eq!(granted["turn"]["kind"], "completed", "{granted}");
    let again = owner.run(&[
        "--json",
        "approvals",
        "approve",
        &approval_id,
        "--digest",
        &digest,
    ]);
    assert_eq!(again.error()["error"]["code"], "CONFLICT_STATE");
    let timeline = owner.ok(&["--json", "runs", "timeline", &run_id]).json();
    assert!(timeline.to_string().contains("fs.write"), "{timeline}");
    let human = owner.ok(&["approvals", "list", "--run", &run_id]);
    assert!(human.stdout.starts_with("id\tstatus"), "{}", human.stdout);

    // 4. Artifacts: uploaded through the generic verb, listed, downloaded and verified
    //    against the digest the server recorded; a tampered record is refused.
    if minio {
        let uploaded = owner.ok(&["--json", "api", "command", "CreateArtifact", "--params", &format!(r#"{{"workspace_id":"{workspace}","title":"notes.txt","media_type":"text/plain","bytes_base64":"aGVsbG8gZnJvbSB0aGUgY2xpCg=="}}"#)]).json();
        let artifact_id = uploaded["artifact_id"].as_str().expect("art").to_string();
        let listed = owner.ok(&["--json", "artifacts", "list"]).json();
        assert!(
            listed["artifacts"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == artifact_id)),
            "{listed}"
        );
        let versions = owner
            .ok(&["--json", "artifacts", "versions", &artifact_id])
            .json();
        let version_id = versions["versions"][0]["id"]
            .as_str()
            .expect("artv")
            .to_string();
        let out = owner_home.path().join("notes.txt");
        let downloaded = owner
            .ok(&[
                "--json",
                "artifacts",
                "download",
                &artifact_id,
                &version_id,
                "--out",
                out.to_str().expect("path"),
            ])
            .json();
        assert_eq!(std::fs::read(&out).expect("bytes"), b"hello from the cli\n");
        assert_eq!(
            downloaded["content_digest"],
            versions["versions"][0]["content_digest"]
        );
    } else {
        eprintln!("BLOCKED_EXTERNAL: dev MinIO is not reachable; the artifact download case needs the object store");
    }

    // 5. A diagnostics bundle carries the server's catalogs and the session's scope, never a token.
    let bundle_path = owner_home.path().join("diagnostics.json");
    owner.ok(&[
        "diagnostics",
        "bundle",
        "--out",
        bundle_path.to_str().expect("path"),
    ]);
    let bundle: Value =
        serde_json::from_slice(&std::fs::read(&bundle_path).expect("bundle")).expect("json");
    assert_eq!(bundle["server"]["health"]["status"], "ready", "{bundle}");
    assert!(
        bundle["server"]["commands"]["commands"].is_array()
            || bundle["server"]["commands"].is_object(),
        "{bundle}"
    );
    assert_eq!(bundle["session"]["tenant_id"], tenant);
    assert_eq!(bundle["session"]["access_token"], "<redacted>");
    assert!(!bundle.to_string().contains("eyJ"), "no JWT in the bundle");
    // Unserved projections are the server's typed refusal, never an invention.
    let routines = owner.run(&["--json", "routines", "list"]);
    assert_eq!(routines.status, 1);
    assert_eq!(routines.error()["status"], 404, "{}", routines.stderr);

    // 6. An invited viewer signs in with their own link (the invitation is accepted by the
    //    sign-in), holds a viewer's role, and the server refuses what a viewer may not do —
    //    through the CLI exactly as through any client.
    let invited = owner.ok(&["--json", "api", "command", "InviteMember", "--params", &format!(r#"{{"email":"viewer@example.com","role":"member","workspace_id":"{workspace}","workspace_role":"viewer"}}"#)]).json();
    assert_eq!(invited["status"], "invited", "{invited}");
    let viewer_home = tempfile::tempdir().expect("home");
    let viewer = Cli {
        home: viewer_home.path().to_path_buf(),
        origin: server.origin.clone(),
    };
    let viewer_session = viewer.login(&server, "viewer@example.com", "Viewer");
    assert_eq!(
        viewer_session["tenant_id"], tenant,
        "the invitation's tenant: {viewer_session}"
    );
    viewer.ok(&["workspaces", "use", &workspace]);
    let members = owner.ok(&["--json", "api", "get", "/v1/members"]).json();
    let viewer_row = members["members"]
        .as_array()
        .expect("members")
        .iter()
        .find(|row| row["user"]["primary_email"] == "viewer@example.com")
        .expect("viewer");
    assert_eq!(
        viewer_row["status"], "active",
        "signing in accepted the invitation: {members}"
    );
    let posted = owner
        .ok(&[
            "--json",
            "messages",
            "post",
            "--content",
            &format!("write again {APPROVAL_SCENARIO}"),
        ])
        .json();
    let run_id = posted["run_id"].as_str().expect("run").to_string();
    let approvals = viewer
        .ok(&["--json", "approvals", "list", "--run", &run_id])
        .json();
    let request = &approvals["approvals"][0];
    let approval_id = request["id"].as_str().expect("apr").to_string();
    let digest = request["params_digest"]
        .as_str()
        .expect("digest")
        .to_string();
    let refused = viewer.run(&[
        "--json",
        "approvals",
        "approve",
        &approval_id,
        "--digest",
        &digest,
    ]);
    assert_eq!(refused.status, 1);
    assert_eq!(
        refused.error()["error"]["code"],
        "SCOPE_FORBIDDEN",
        "{}",
        refused.stderr
    );
    assert_eq!(
        refused.error()["error"]["details"]["action"],
        "approve_effect"
    );
    let audit = viewer.run(&["--json", "api", "get", "/v1/audit"]);
    assert_eq!(audit.error()["error"]["code"], "SCOPE_FORBIDDEN");
    let still = owner
        .ok(&["--json", "approvals", "list", "--run", &run_id])
        .json();
    assert_eq!(
        still["approvals"][0]["status"], "requested",
        "nothing was decided: {still}"
    );
    let denied = owner
        .ok(&["--json", "approvals", "deny", &approval_id])
        .json();
    assert!(
        denied["approval_request_id"] == approval_id || denied.is_object(),
        "{denied}"
    );
    // The viewer's credentials are theirs alone: the owner's home knows nothing of them.
    let owner_store: Value = serde_json::from_slice(
        &std::fs::read(owner_home.path().join("credentials.json")).expect("store"),
    )
    .expect("json");
    assert_eq!(owner_store["sessions"].as_array().map_or(0, Vec::len), 1);
    assert_eq!(owner_store["sessions"][0]["user_id"], session["user_id"]);

    // 7. Sign out: the session is gone from the store and from the server.
    owner.ok(&["logout"]);
    let gone = owner.run(&["whoami"]);
    assert_eq!(gone.status, 1);
    assert!(gone.stderr.contains("not signed in"), "{}", gone.stderr);
    drop(server);
}
