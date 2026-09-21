//! `quansio`: the CLI over the public API v1 (APP-013). The transport half: every verb is
//! one or more `/v1/` requests with the stored bearer session; the §15 error body is
//! printed as it came, tokens never are.

#![forbid(unsafe_code)]

use std::io::Write as _;
use std::process::ExitCode;

use quansio_cli::{
    diagnostics_bundle, home_dir, parse, public_path, redact_tokens, verify_digest, Command,
    CredentialError, Credentials, Invocation, StoredSession, UsageError, VERSION,
};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

/// A failure the CLI reports: exit 2 for usage, 1 for anything the server or the store refused.
#[derive(Debug)]
enum Failure {
    Usage(String),
    /// The §15 error body the server returned.
    Api {
        status: u16,
        body: Value,
    },
    Other(String),
}

impl From<UsageError> for Failure {
    fn from(error: UsageError) -> Self {
        Self::Usage(error.0)
    }
}

impl From<CredentialError> for Failure {
    fn from(error: CredentialError) -> Self {
        Self::Other(error.to_string())
    }
}

impl From<reqwest::Error> for Failure {
    fn from(error: reqwest::Error) -> Self {
        Self::Other(format!("request failed: {error}"))
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Self::Other(error.to_string())
    }
}

/// The session the invocation acts with, resolved from the store.
struct Scope {
    home: std::path::PathBuf,
    session: StoredSession,
}

struct Api {
    http: reqwest::Client,
    origin: String,
    bearer: Option<String>,
}

impl Api {
    fn new(origin: &str, bearer: Option<String>) -> Result<Self, Failure> {
        let http = reqwest::Client::builder()
            .user_agent(format!("quansio-cli/{VERSION}"))
            .build()?;
        Ok(Self {
            http,
            origin: quansio_cli::normalize_origin(origin),
            bearer,
        })
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert("x-quansio-client", HeaderValue::from_static("cli"));
        if let Some(bearer) = &self.bearer {
            if let Ok(value) = HeaderValue::from_str(&format!("Bearer {bearer}")) {
                headers.insert(AUTHORIZATION, value);
            }
        }
        headers
    }

    async fn decode(response: reqwest::Response) -> Result<Value, Failure> {
        let status = response.status().as_u16();
        let bytes = response.bytes().await?;
        let body: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({ "code": "INTERNAL", "message": String::from_utf8_lossy(&bytes), "correlation_id": "", "retryable": false }))
        };
        if (200..300).contains(&status) {
            Ok(body)
        } else {
            Err(Failure::Api { status, body })
        }
    }

    /// `GET /v1/<path>`: the path is validated here again, whatever verb built it.
    async fn get(&self, path: &str) -> Result<Value, Failure> {
        let path = public_path(path)?;
        let response = self
            .http
            .get(format!("{}{path}", self.origin))
            .headers(self.headers())
            .send()
            .await?;
        Self::decode(response).await
    }

    async fn get_bytes(&self, path: &str) -> Result<(Vec<u8>, HeaderMap), Failure> {
        let path = public_path(path)?;
        let response = self
            .http
            .get(format!("{}{path}", self.origin))
            .headers(self.headers())
            .send()
            .await?;
        if !response.status().is_success() {
            return Self::decode(response)
                .await
                .map(|_| unreachable!("a non-success status decodes to an error"));
        }
        let headers = response.headers().clone();
        Ok((response.bytes().await?.to_vec(), headers))
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, Failure> {
        let path = public_path(path)?;
        let mut headers = self.headers();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let response = self
            .http
            .post(format!("{}{path}", self.origin))
            .headers(headers)
            .body(body.to_string())
            .send()
            .await?;
        Self::decode(response).await
    }

    /// `POST /v1/commands/<Name>` with a fresh command id; returns the owner's result.
    async fn command(&self, name: &str, params: Value) -> Result<Value, Failure> {
        if !quansio_cli::is_command_name(name) {
            return Err(Failure::Usage(format!("{name:?} is not a command name")));
        }
        let response = self
            .post(
                &format!("/v1/commands/{name}"),
                &json!({ "command_id": command_id(), "params": params }),
            )
            .await?;
        Ok(response.get("result").cloned().unwrap_or(response))
    }
}

/// A client-minted, ULID-shaped command id (the server's idempotency key).
fn command_id() -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut time = u128::from(chrono::Utc::now().timestamp_millis().unsigned_abs());
    let mut stamp = [b'0'; 10];
    for slot in stamp.iter_mut().rev() {
        *slot = ALPHABET[usize::try_from(time % 32).unwrap_or(0)];
        time /= 32;
    }
    let mut entropy = [b'0'; 16];
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = u128::from(std::process::id());
    let mut seed = nanos
        ^ (pid << 64)
        ^ u128::from(
            quansio_cli::sha256_hex(&nanos.to_le_bytes())
                .bytes()
                .fold(0u64, |acc, b| {
                    acc.wrapping_mul(31).wrapping_add(u64::from(b))
                }),
        );
    for slot in &mut entropy {
        *slot = ALPHABET[usize::try_from(seed % 32).unwrap_or(0)];
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
    }
    format!(
        "cmd_{}{}",
        String::from_utf8_lossy(&stamp),
        String::from_utf8_lossy(&entropy)
    )
}

fn home() -> std::path::PathBuf {
    home_dir(
        std::env::var("QUANSIO_CLI_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

fn scope(invocation: &Invocation) -> Result<Scope, Failure> {
    let home = home();
    let store = Credentials::load(&home)?;
    let session = store
        .select(invocation.server.as_deref(), invocation.tenant.as_deref())?
        .clone();
    Ok(Scope { home, session })
}

fn workspace_of(scope: &Scope) -> Result<String, Failure> {
    scope.session.workspace_id.clone().ok_or_else(|| {
        Failure::Other("no default workspace: run `quansio workspaces use <ws_…>`".to_string())
    })
}

/// Refresh the access token once when the server says it expired.
async fn with_refresh<F, Fut>(scope: &mut Scope, call: F) -> Result<Value, Failure>
where
    F: Fn(Api) -> Fut,
    Fut: std::future::Future<Output = Result<Value, Failure>>,
{
    let api = Api::new(
        &scope.session.origin,
        Some(scope.session.access_token.clone()),
    )?;
    match call(api).await {
        Err(Failure::Api { body, .. }) if body["code"] == "AUTH_EXPIRED" => {
            let anonymous = Api::new(&scope.session.origin, None)?;
            let renewed = anonymous
                .post(
                    "/v1/auth/refresh",
                    &json!({ "refresh_token": scope.session.refresh_token }),
                )
                .await?;
            let mut store = Credentials::load(&scope.home)?;
            let session = session_from(
                &scope.session.origin,
                &renewed,
                scope.session.workspace_id.clone(),
            )?;
            store.put(session.clone());
            store.save(&scope.home)?;
            scope.session = session;
            let api = Api::new(
                &scope.session.origin,
                Some(scope.session.access_token.clone()),
            )?;
            call(api).await
        }
        other => other,
    }
}

fn session_from(
    origin: &str,
    issued: &Value,
    workspace: Option<String>,
) -> Result<StoredSession, Failure> {
    let text = |key: &str| {
        issued
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Failure::Other(format!("the server's session has no {key}")))
    };
    Ok(StoredSession {
        origin: quansio_cli::normalize_origin(origin),
        tenant_id: text("tenant_id")?,
        user_id: text("user_id")?,
        workspace_id: issued
            .get("workspace_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(workspace),
        access_token: text("access_token")?,
        access_expires_at: text("access_expires_at")?,
        refresh_token: text("refresh_token")?,
        refresh_expires_at: text("refresh_expires_at")?,
    })
}

fn print_json(value: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(
        out,
        "{}",
        serde_json::to_string_pretty(&redact_tokens(value)).unwrap_or_default()
    );
}

fn print_rows(rows: &[Value], columns: &[&str]) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", columns.join("\t"));
    for row in rows {
        let cells: Vec<String> = columns
            .iter()
            .map(|column| match &row[*column] {
                Value::Null => String::new(),
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .collect();
        let _ = writeln!(out, "{}", cells.join("\t"));
    }
}

fn print_summary(invocation: &Invocation, value: &Value, rows_key: Option<&str>, columns: &[&str]) {
    if invocation.json {
        print_json(value);
        return;
    }
    match rows_key.and_then(|key| value[key].as_array()) {
        Some(rows) => print_rows(rows, columns),
        None => print_json(value),
    }
}

async fn run(invocation: Invocation) -> Result<(), Failure> {
    match &invocation.command {
        Command::Login { email, name, token } => {
            login(
                &invocation,
                email.as_deref(),
                name.as_deref(),
                token.as_deref(),
            )
            .await
        }
        Command::Logout => {
            let scope = scope(&invocation)?;
            let api = Api::new(
                &scope.session.origin,
                Some(scope.session.access_token.clone()),
            )?;
            let _ = api.post("/v1/auth/sign-out", &json!({})).await;
            let mut store = Credentials::load(&scope.home)?;
            store.remove(&scope.session.origin, &scope.session.tenant_id);
            store.save(&scope.home)?;
            println!(
                "signed out of {} ({})",
                scope.session.origin, scope.session.tenant_id
            );
            Ok(())
        }
        Command::Whoami => {
            let mut scope = scope(&invocation)?;
            let me = with_refresh(&mut scope, |api| async move { api.get("/v1/me").await }).await?;
            if invocation.json {
                print_json(&json!({ "session": scope.session.redacted(), "me": me }));
            } else {
                println!(
                    "user\t{}\t{}",
                    me["user"]["id"].as_str().unwrap_or(""),
                    me["user"]["primary_email"].as_str().unwrap_or("")
                );
                println!("tenant\t{}", scope.session.tenant_id);
                println!("server\t{}", scope.session.origin);
                println!(
                    "workspace\t{}",
                    scope.session.workspace_id.as_deref().unwrap_or("(none)")
                );
            }
            Ok(())
        }
        Command::WorkspacesList => {
            let mut scope = scope(&invocation)?;
            let me = with_refresh(&mut scope, |api| async move { api.get("/v1/me").await }).await?;
            print_summary(
                &invocation,
                &me,
                Some("workspaces"),
                &["id", "name", "role"],
            );
            Ok(())
        }
        Command::WorkspacesUse { workspace_id } => {
            let scope = scope(&invocation)?;
            let mut store = Credentials::load(&scope.home)?;
            let mut session = scope.session.clone();
            session.workspace_id = Some(workspace_id.clone());
            store.put(session);
            store.save(&scope.home)?;
            println!("default workspace\t{workspace_id}");
            Ok(())
        }
        Command::WorkspacesCreate { name } => {
            let mut scope = scope(&invocation)?;
            let name = name.clone();
            let created = with_refresh(&mut scope, |api| {
                let name = name.clone();
                async move {
                    api.command("CreateWorkspace", json!({ "name": name }))
                        .await
                }
            })
            .await?;
            print_summary(&invocation, &created, None, &[]);
            Ok(())
        }
        Command::ThreadsList => {
            let mut scope = scope(&invocation)?;
            let workspace = workspace_of(&scope)?;
            let threads = with_refresh(&mut scope, |api| {
                let workspace = workspace.clone();
                async move {
                    api.get(&format!("/v1/threads?workspace_id={workspace}"))
                        .await
                }
            })
            .await?;
            print_summary(
                &invocation,
                &threads,
                Some("threads"),
                &["id", "kind", "title", "updated_at"],
            );
            Ok(())
        }
        Command::MessagesList { thread_id } => {
            let mut scope = scope(&invocation)?;
            let thread_id = thread_id.clone();
            let messages = with_refresh(&mut scope, |api| {
                let thread_id = thread_id.clone();
                async move {
                    api.get(&format!("/v1/messages?thread_id={thread_id}"))
                        .await
                }
            })
            .await?;
            print_summary(
                &invocation,
                &messages,
                Some("messages"),
                &["id", "author_id", "created_at"],
            );
            Ok(())
        }
        Command::MessagesPost {
            content,
            thread_id,
            teammate_id,
        } => {
            let mut scope = scope(&invocation)?;
            let workspace = workspace_of(&scope)?;
            let mut params = json!({ "workspace_id": workspace, "content": content });
            if let Some(thread) = thread_id {
                params["thread_id"] = json!(thread);
            }
            if let Some(teammate) = teammate_id {
                params["teammate_id"] = json!(teammate);
            }
            let posted = with_refresh(&mut scope, |api| {
                let params = params.clone();
                async move { api.command("PostMessage", params).await }
            })
            .await?;
            print_summary(&invocation, &posted, None, &[]);
            Ok(())
        }
        Command::ObjectivesCreate { title, description } => {
            let mut scope = scope(&invocation)?;
            let workspace = workspace_of(&scope)?;
            let mut params = json!({ "workspace_id": workspace, "title": title });
            if let Some(description) = description {
                params["description"] = json!(description);
            }
            let created = with_refresh(&mut scope, |api| {
                let params = params.clone();
                async move { api.command("CreateObjective", params).await }
            })
            .await?;
            print_summary(&invocation, &created, None, &[]);
            Ok(())
        }
        Command::RunsGet { run_id } => {
            let mut scope = scope(&invocation)?;
            let run_id = run_id.clone();
            let run = with_refresh(&mut scope, |api| {
                let run_id = run_id.clone();
                async move { api.get(&format!("/v1/runs/{run_id}")).await }
            })
            .await?;
            print_summary(&invocation, &run, None, &[]);
            Ok(())
        }
        Command::RunsTimeline { run_id } => {
            let mut scope = scope(&invocation)?;
            let run_id = run_id.clone();
            let timeline = with_refresh(&mut scope, |api| {
                let run_id = run_id.clone();
                async move { api.get(&format!("/v1/runs/{run_id}/timeline")).await }
            })
            .await?;
            print_summary(
                &invocation,
                &timeline,
                Some("turns"),
                &["id", "status", "started_at"],
            );
            Ok(())
        }
        Command::RunsCancel { run_id } => {
            let mut scope = scope(&invocation)?;
            let run_id = run_id.clone();
            let cancelled = with_refresh(&mut scope, |api| {
                let run_id = run_id.clone();
                async move { api.command("CancelRun", json!({ "run_id": run_id })).await }
            })
            .await?;
            print_summary(&invocation, &cancelled, None, &[]);
            Ok(())
        }
        Command::ApprovalsList { run_id } => {
            let mut scope = scope(&invocation)?;
            let query = match run_id {
                Some(run_id) => format!("run_id={run_id}"),
                None => format!("workspace_id={}", workspace_of(&scope)?),
            };
            let approvals = with_refresh(&mut scope, |api| {
                let query = query.clone();
                async move { api.get(&format!("/v1/approvals?{query}")).await }
            })
            .await?;
            print_summary(
                &invocation,
                &approvals,
                Some("approvals"),
                &["id", "status", "summary", "params_digest", "expires_at"],
            );
            Ok(())
        }
        Command::ApprovalsShow { approval_id } => {
            let mut scope = scope(&invocation)?;
            let workspace = workspace_of(&scope)?;
            let approvals = with_refresh(&mut scope, |api| {
                let workspace = workspace.clone();
                async move {
                    api.get(&format!("/v1/approvals?workspace_id={workspace}"))
                        .await
                }
            })
            .await?;
            let found = approvals["approvals"]
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["id"] == *approval_id))
                .cloned();
            let Some(request) = found else {
                return Err(Failure::Api {
                    status: 404,
                    body: json!({ "code": "NOT_FOUND", "message": format!("approval {approval_id} is not in workspace {workspace}"), "correlation_id": "", "retryable": false }),
                });
            };
            if invocation.json {
                print_json(&request);
            } else {
                println!(
                    "{}\t{}",
                    request["id"].as_str().unwrap_or(""),
                    request["status"].as_str().unwrap_or("")
                );
                println!("summary\t{}", request["summary"].as_str().unwrap_or(""));
                println!(
                    "effect\t{}\ttier {}",
                    request["effect"]["class"].as_str().unwrap_or("?"),
                    request["effect"]["tier"]
                );
                println!("consequences\t{}", request["consequence_preview"]);
                println!("expires\t{}", request["expires_at"].as_str().unwrap_or(""));
                println!(
                    "digest\t{}",
                    request["params_digest"].as_str().unwrap_or("")
                );
                println!(
                    "approve with: quansio approvals approve {} --digest {}",
                    request["id"].as_str().unwrap_or(""),
                    request["params_digest"].as_str().unwrap_or("")
                );
            }
            Ok(())
        }
        Command::ApprovalsApprove {
            approval_id,
            digest,
        } => {
            let mut scope = scope(&invocation)?;
            let params = json!({ "approval_request_id": approval_id, "params_digest": digest });
            let granted = with_refresh(&mut scope, |api| {
                let params = params.clone();
                async move { api.command("ApproveEffect", params).await }
            })
            .await?;
            print_summary(&invocation, &granted, None, &[]);
            Ok(())
        }
        Command::ApprovalsDeny { approval_id } => {
            let mut scope = scope(&invocation)?;
            let params = json!({ "approval_request_id": approval_id });
            let denied = with_refresh(&mut scope, |api| {
                let params = params.clone();
                async move { api.command("DenyEffect", params).await }
            })
            .await?;
            print_summary(&invocation, &denied, None, &[]);
            Ok(())
        }
        Command::ArtifactsList => {
            let mut scope = scope(&invocation)?;
            let workspace = workspace_of(&scope)?;
            let artifacts = with_refresh(&mut scope, |api| {
                let workspace = workspace.clone();
                async move {
                    api.get(&format!("/v1/artifacts?workspace_id={workspace}"))
                        .await
                }
            })
            .await?;
            print_summary(
                &invocation,
                &artifacts,
                Some("artifacts"),
                &["id", "kind", "title", "current_version_id", "version_count"],
            );
            Ok(())
        }
        Command::ArtifactsVersions { artifact_id } => {
            let mut scope = scope(&invocation)?;
            let artifact_id = artifact_id.clone();
            let versions = with_refresh(&mut scope, |api| {
                let artifact_id = artifact_id.clone();
                async move {
                    api.get(&format!("/v1/artifacts/{artifact_id}/versions"))
                        .await
                }
            })
            .await?;
            print_summary(
                &invocation,
                &versions,
                Some("versions"),
                &[
                    "id",
                    "version",
                    "media_type",
                    "size_bytes",
                    "content_digest",
                ],
            );
            Ok(())
        }
        Command::ArtifactsDownload {
            artifact_id,
            version_id,
            out,
        } => {
            let scope = scope(&invocation)?;
            let api = Api::new(
                &scope.session.origin,
                Some(scope.session.access_token.clone()),
            )?;
            let versions = api
                .get(&format!("/v1/artifacts/{artifact_id}/versions"))
                .await?;
            let expected = versions["versions"]
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["id"] == *version_id))
                .and_then(|row| row["content_digest"].as_str())
                .map(str::to_string);
            let (bytes, headers) = api
                .get_bytes(&format!(
                    "/v1/artifacts/{artifact_id}/versions/{version_id}/content"
                ))
                .await?;
            let labelled = headers
                .get("x-quansio-digest")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let digest = labelled.or(expected).ok_or_else(|| Failure::Other("the server labelled the bytes with no digest; refusing to write unverified content".to_string()))?;
            if !verify_digest(&bytes, &digest) {
                return Err(Failure::Other(format!("digest mismatch: the bytes hash to {} but the record says {digest}; nothing written", quansio_cli::sha256_hex(&bytes))));
            }
            std::fs::write(out, &bytes)?;
            if invocation.json {
                print_json(
                    &json!({ "artifact_id": artifact_id, "version_id": version_id, "bytes": bytes.len(), "content_digest": digest, "path": out }),
                );
            } else {
                println!(
                    "wrote {} bytes to {} (sha256 {digest} verified)",
                    bytes.len(),
                    out.display()
                );
            }
            Ok(())
        }
        Command::RoutinesList => {
            let mut scope = scope(&invocation)?;
            let workspace = workspace_of(&scope)?;
            let routines = with_refresh(&mut scope, |api| {
                let workspace = workspace.clone();
                async move {
                    api.get(&format!("/v1/routines?workspace_id={workspace}"))
                        .await
                }
            })
            .await?;
            print_summary(
                &invocation,
                &routines,
                Some("routines"),
                &["id", "name", "status"],
            );
            Ok(())
        }
        Command::TargetsList => {
            let mut scope = scope(&invocation)?;
            let workspace = workspace_of(&scope)?;
            let targets = with_refresh(&mut scope, |api| {
                let workspace = workspace.clone();
                async move {
                    api.get(&format!("/v1/targets?workspace_id={workspace}"))
                        .await
                }
            })
            .await?;
            print_summary(
                &invocation,
                &targets,
                Some("targets"),
                &["id", "class", "status"],
            );
            Ok(())
        }
        Command::DiagnosticsBundle { out } => {
            let store = Credentials::load(&home())?;
            let session = store
                .select(invocation.server.as_deref(), invocation.tenant.as_deref())
                .ok()
                .cloned();
            let origin = invocation
                .server
                .clone()
                .or_else(|| session.as_ref().map(|session| session.origin.clone()))
                .ok_or_else(|| {
                    Failure::Other(
                        "diagnostics bundle needs --server or a stored session".to_string(),
                    )
                })?;
            let api = Api::new(
                &origin,
                session.as_ref().map(|session| session.access_token.clone()),
            )?;
            let mut errors = Vec::new();
            let health = probe(&api, "/v1/health", &mut errors).await;
            let commands = probe(&api, "/v1/commands", &mut errors).await;
            let projections = probe(&api, "/v1/read-projections", &mut errors).await;
            let me = if session.is_some() {
                probe(&api, "/v1/me", &mut errors).await
            } else {
                None
            };
            let bundle = diagnostics_bundle(
                session.as_ref(),
                health.as_ref(),
                commands.as_ref(),
                projections.as_ref(),
                me.as_ref(),
                &errors,
            );
            match out {
                Some(path) => {
                    std::fs::write(path, serde_json::to_vec_pretty(&bundle).unwrap_or_default())?;
                    println!("wrote diagnostics bundle to {}", path.display());
                }
                None => print_json(&bundle),
            }
            Ok(())
        }
        Command::ApiGet { path } => {
            let mut scope = scope(&invocation)?;
            let path = path.clone();
            let value = with_refresh(&mut scope, |api| {
                let path = path.clone();
                async move { api.get(&path).await }
            })
            .await?;
            print_json(&value);
            Ok(())
        }
        Command::ApiCommand { name, params } => {
            let mut scope = scope(&invocation)?;
            let name = name.clone();
            let params = params.clone();
            let result = with_refresh(&mut scope, |api| {
                let name = name.clone();
                let params = params.clone();
                async move { api.command(&name, params).await }
            })
            .await?;
            print_json(&result);
            Ok(())
        }
    }
}

/// One diagnostics read: the payload, or the failure recorded in the bundle's errors.
async fn probe(api: &Api, path: &'static str, errors: &mut Vec<String>) -> Option<Value> {
    match api.get(path).await {
        Ok(value) => Some(value),
        Err(Failure::Api { status, body }) => {
            errors.push(format!(
                "{path}: {status} {}",
                body["code"].as_str().unwrap_or("")
            ));
            None
        }
        Err(other) => {
            errors.push(format!("{path}: {other:?}"));
            None
        }
    }
}

async fn login(
    invocation: &Invocation,
    email: Option<&str>,
    name: Option<&str>,
    token: Option<&str>,
) -> Result<(), Failure> {
    let origin = invocation
        .server
        .clone()
        .ok_or_else(|| Failure::Usage("login needs --server URL".to_string()))?;
    let api = Api::new(&origin, None)?;
    if let Some(token) = token {
        let issued = api
            .post("/v1/auth/magic-link/complete", &json!({ "token": token }))
            .await?;
        let session = session_from(&origin, &issued, None)?;
        if let Some(wanted) = &invocation.tenant {
            if *wanted != session.tenant_id {
                return Err(Failure::Other(format!(
                    "the link signs into tenant {}, not {wanted}; nothing stored",
                    session.tenant_id
                )));
            }
        }
        let home = home();
        let mut store = Credentials::load(&home)?;
        store.put(session.clone());
        let path = store.save(&home)?;
        if invocation.json {
            print_json(&json!({ "session": session.redacted(), "credentials": path }));
        } else {
            println!(
                "signed in to {} as {} (tenant {})",
                session.origin, session.user_id, session.tenant_id
            );
            println!("credentials stored at {}", path.display());
        }
        return Ok(());
    }
    let email =
        email.ok_or_else(|| Failure::Usage("login needs --email or --token".to_string()))?;
    let started = api
        .post(
            "/v1/auth/magic-link/start",
            &json!({ "email": email, "display_name": name.unwrap_or("") }),
        )
        .await?;
    if invocation.json {
        print_json(&started);
    } else {
        println!("a sign-in link was sent to {email} (delivery: {}); complete with: quansio login --server {origin} --token <token from the link>", started["delivery"].as_str().unwrap_or("?"));
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let invocation = match parse(&args) {
        Ok(invocation) => invocation,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let json = invocation.json;
    match run(invocation).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(message)) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
        Err(Failure::Api { status, body }) => {
            if json {
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(
                        &json!({ "status": status, "error": redact_tokens(&body) })
                    )
                    .unwrap_or_default()
                );
            } else {
                eprintln!(
                    "{} {}: {}{}",
                    status,
                    body["code"].as_str().unwrap_or("ERROR"),
                    body["message"].as_str().unwrap_or(""),
                    body["correlation_id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .map(|id| format!(" (correlation {id})"))
                        .unwrap_or_default()
                );
            }
            ExitCode::from(1)
        }
        Err(Failure::Other(message)) => {
            eprintln!("{message}");
            ExitCode::from(1)
        }
    }
}
