//! A harness for the output tools (ADP-006): a real PostgreSQL database, the development MinIO, the
//! server's real tool dispatcher (schema validation, capability projection, policy, effect ledger)
//! and, when asked, a real headless Chrome behind the render port.
//!
//! A test drives it one governed tool call at a time: [`Harness::call`] starts a run, has the scripted
//! model propose exactly that call, lets the dispatcher validate, authorize, reserve and dispatch it,
//! and returns what the Tool Registry's call row says happened. Nothing here stands in for the
//! database, the object store, the dispatcher or the browser.

#![allow(dead_code)]

use std::collections::{BTreeMap, VecDeque};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use quansio_capability::projection::{CapabilityProjection, ProjectionSubject, SubjectKind};
use quansio_capability::Grant;
use quansio_core::{CanonicalId, CorrelationId, Digest, Prefix, UlidGenerator};
use quansio_server::artifacts::outputs::ports::{
    Blocked, MeasureSpec, PdfSpec, PortError, RenderJob, RenderPort, RenderResult,
};
use quansio_server::artifacts::outputs::OutputToolHost;
use quansio_server::artifacts::tool_host::{ArtifactToolHost, ServerToolHost};
use quansio_server::artifacts::{ArtifactVersionId, ObjectStore, S3ObjectStore};
use quansio_server::control::schema;
use quansio_server::policy::{ActorRoles, TenantRole, WorkspaceRole};
use quansio_server::runtime::agents::AgentDelegationPort;
use quansio_server::runtime::state_machine::DelegationPort;
use quansio_server::runtime::state_machine::{
    ModelCallRequest, ModelProposal, ModelProposalSource, NewRun, ProposedToolCall, RunTriggerKind,
    RuntimeEngine, RuntimeError, RuntimeIdentity, TurnInput,
};
use quansio_server::runtime::turn_loop::{
    ActingActor, RoleProvider, StoredProjectionProvider, ToolDispatchService, UnavailableToolHost,
};
use serde_json::{json, Value};
use sqlx::PgPool;

use super::{admin_url, drop_pool, fresh_database, scratch_name, seed_tenant};

pub const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YADP006";
pub const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADP006";
pub const OTHER_USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADP007";
pub const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YADP006";
pub const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YADP006";
pub const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9YADP006";

// ---------------------------------------------------------------------------------------------
// A real Chrome behind the render port
// ---------------------------------------------------------------------------------------------

/// The Chrome this host has, if any.
pub fn chrome_path() -> Option<&'static str> {
    [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
    ]
    .into_iter()
    .find(|path| std::path::Path::new(path).exists())
}

/// A headless Chrome on a loopback debugging port, killed when dropped.
pub struct Chrome {
    child: Child,
    pub port: u16,
    _profile: tempfile::TempDir,
}

impl Chrome {
    pub fn launch() -> Option<Self> {
        let chrome = chrome_path()?;
        let profile = tempfile::tempdir().expect("profile");
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
            if answers(port) {
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
}

impl Drop for Chrome {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn answers(port: u16) -> bool {
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

/// The render port, implemented by the worker's own browser renderer (`quansio_qworkerd`) against a
/// real Chrome. This is the composition the deployable will make over the machine gateway.
pub struct ChromeRender {
    pub port: u16,
    /// Every job it has been given, for tests that assert what the host asked for.
    pub seen: Mutex<Vec<(usize, bool)>>,
}

impl ChromeRender {
    pub fn new(port: u16) -> Arc<Self> {
        Arc::new(Self {
            port,
            seen: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl RenderPort for ChromeRender {
    async fn render(&self, job: RenderJob) -> Result<RenderResult, PortError> {
        use quansio_qworkerd::browser::render as worker;
        self.seen
            .lock()
            .expect("seen")
            .push((job.html.len(), job.pdf.is_some()));
        let request = worker::RenderRequest {
            html: job.html,
            assets: job
                .assets
                .into_iter()
                .map(|(name, (media_type, bytes))| {
                    (name, worker::PinnedAsset { media_type, bytes })
                })
                .collect::<BTreeMap<_, _>>(),
            viewport: job.viewport,
            measure: match job.measure {
                MeasureSpec::Text => worker::Measure::Text,
                MeasureSpec::Slides {
                    overflow_tolerance_px,
                    min_contrast_text,
                    min_contrast_large,
                } => worker::Measure::Slides(worker::SlideLimits {
                    overflow_tolerance_px,
                    min_contrast_text,
                    min_contrast_large,
                }),
            },
            pdf: job.pdf.map(
                |PdfSpec {
                     paper_width_in,
                     paper_height_in,
                     margin_in,
                     prefer_css_page_size,
                     max_bytes,
                 }| worker::PdfJob {
                    paper_width_in,
                    paper_height_in,
                    margin_in,
                    prefer_css_page_size,
                    max_bytes,
                },
            ),
            screenshot: job.screenshot,
            max_screenshot_bytes: job.max_screenshot_bytes,
            deadline: Duration::from_millis(job.deadline_ms),
        };
        match worker::render(self.port, &request).await {
            Ok(output) => Ok(RenderResult {
                pdf: output.pdf,
                screenshot: output.screenshot,
                measurements: output.measurements,
                text: output.text,
                blocked: output
                    .blocked
                    .into_iter()
                    .map(|blocked| Blocked {
                        scheme: blocked.scheme,
                        host: blocked.host,
                        resource_type: blocked.resource_type,
                    })
                    .collect(),
                browser: output.browser,
            }),
            Err(worker::RenderError::TooLarge { what, bytes, limit }) => Err(PortError::TooLarge(
                format!("the {what} is {bytes} bytes, over the {limit}-byte limit"),
            )),
            Err(error) => Err(PortError::Failed(error.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
struct Scripted {
    moves: Arc<Mutex<VecDeque<(String, Value)>>>,
}

#[async_trait]
impl ModelProposalSource for Scripted {
    async fn propose(&self, _request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        let next = self.moves.lock().expect("moves").pop_front();
        let n = UlidGenerator::new().generate().to_string();
        Ok(match next {
            Some((tool, arguments)) => ModelProposal {
                tool_calls: vec![ProposedToolCall::new(format!("call_{n}"), tool, arguments)],
                ..ModelProposal::default()
            },
            None => ModelProposal {
                assistant_text: Some("done".to_string()),
                ..ModelProposal::default()
            },
        })
    }
}

struct Roles {
    user: String,
}

#[async_trait]
impl RoleProvider for Roles {
    async fn actor_for_run(&self, _run_id: &str) -> Result<Option<ActingActor>, RuntimeError> {
        Ok(Some(ActingActor {
            roles: ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Editor)),
            user_id: Some(self.user.clone()),
        }))
    }
}

/// What one governed call did.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The tool.
    pub tool: String,
    /// The call row's status (`completed` or a refusal).
    pub status: String,
    /// The call row's result: the tool's output, or the error it was refused with.
    pub result: Value,
}

impl Outcome {
    pub fn ok(&self) -> bool {
        self.status == "completed"
    }

    /// The refusal's code, when there is one.
    pub fn code(&self) -> Option<&str> {
        self.result
            .pointer("/error/code")
            .or_else(|| self.result.get("code"))
            .and_then(Value::as_str)
    }

    /// Whether the refusal names a rule.
    pub fn names(&self, text: &str) -> bool {
        self.result.to_string().contains(text)
    }
}

/// A version row.
#[derive(Debug, Clone)]
pub struct VersionRow {
    pub seq: i32,
    pub id: String,
    pub media_type: String,
    pub parent: Option<String>,
    pub format_check: Value,
    pub quality_record: Value,
}

pub struct Harness {
    pub name: String,
    pub pool: PgPool,
    pub identity: RuntimeIdentity,
    pub objects: Arc<dyn ObjectStore>,
    pub engine: RuntimeEngine,
    moves: Arc<Mutex<VecDeque<(String, Value)>>>,
}

impl Harness {
    /// A harness over a scratch database and the development MinIO; `None` (with the standard
    /// BLOCKED_EXTERNAL marker printed by the caller) when the database is not available.
    pub async fn prepare(
        prefix: &str,
        outputs: impl FnOnce(OutputToolHost) -> OutputToolHost,
    ) -> Option<Self> {
        let name = admin_url().map(|_| scratch_name(prefix))?;
        let pool = fresh_database(&name).await?;
        schema::migrate(&pool).await.expect("migrate");
        seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
        let mut tx = pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        sqlx::query(
            "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) \
             VALUES ($1, $2, $3, 'teammate', 1, 'ACTIVE')",
        )
        .bind(AGENT)
        .bind(TENANT)
        .bind(WORKSPACE)
        .execute(&mut *tx)
        .await
        .expect("agent thread");
        sqlx::query(
            "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
             VALUES ('pol_01J8Z3K6F1N8VQ2X5W9YADP006', $1, NULL, 'tenant', $2)",
        )
        .bind(TENANT)
        .bind(json!([
            { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" },
            { "effect_class": "read.internal", "resource": { "kind": "artifact", "selector": "*" }, "decision": "allow" },
            { "effect_class": "content.upload", "resource": { "kind": "artifact", "selector": "*" }, "decision": "allow" }
        ]))
        .execute(&mut *tx)
        .await
        .expect("policy");
        tx.commit().await.expect("commit");
        let identity = RuntimeIdentity::system(
            TENANT,
            "outputs-test",
            CorrelationId::generate(&mut UlidGenerator::new()),
        );
        let objects: Arc<dyn ObjectStore> =
            Arc::new(S3ObjectStore::from_env().expect("QUANSIO_TEST_MINIO_* configuration"));
        let moves = Arc::new(Mutex::new(VecDeque::new()));
        let delegation: Arc<dyn DelegationPort> = Arc::new(
            AgentDelegationPort::with_structural_check(pool.clone(), identity.clone())
                .expect("delegation"),
        );
        let host = ServerToolHost::new(
            ArtifactToolHost::new(pool.clone(), Arc::clone(&objects)),
            Arc::new(UnavailableToolHost),
        )
        .with_outputs(outputs(OutputToolHost::new(
            pool.clone(),
            Arc::clone(&objects),
        )));
        let dispatcher = ToolDispatchService::new(
            pool.clone(),
            identity.clone(),
            Arc::new(
                StoredProjectionProvider::new(pool.clone(), identity.clone()).expect("projections"),
            ),
            Arc::new(Roles {
                user: USER.to_string(),
            }),
            Arc::new(host),
            Arc::clone(&delegation),
        )
        .expect("dispatch");
        let engine = RuntimeEngine::new(pool.clone(), identity.clone())
            .expect("engine")
            .with_model_source(Arc::new(Scripted {
                moves: Arc::clone(&moves),
            }))
            .with_tool_dispatch(Arc::new(dispatcher))
            .with_delegation(delegation);
        Some(Self {
            name,
            pool,
            identity,
            objects,
            engine,
            moves,
        })
    }

    /// Propose one governed tool call in a fresh run and report what the call row says.
    pub async fn call(&self, tool: &str, arguments: Value) -> Outcome {
        let run = self
            .engine
            .create_run(NewRun::new(
                WORKSPACE,
                CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("node"),
                CanonicalId::parse_typed(AGENT, Prefix::AgentThread).expect("agent"),
                RunTriggerKind::Manual,
            ))
            .await
            .expect("run");
        let run = self
            .engine
            .enqueue(&run.id, run.generation)
            .await
            .expect("enqueue");
        let run = self
            .engine
            .start(&run.id, run.generation)
            .await
            .expect("start");
        self.seed_projection(&run.id.to_string()).await;
        self.moves
            .lock()
            .expect("moves")
            .push_back((tool.to_string(), arguments));
        let current = self.engine.store().load_run(&run.id).await.expect("run");
        let _ = self
            .engine
            .run_turn(
                &run.id,
                current.generation,
                TurnInput::new(RunTriggerKind::Manual),
            )
            .await
            .expect("turn");
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let (status, result): (String, Option<Value>) = sqlx::query_as(
            "SELECT status, result FROM tool_calls WHERE tenant_id = $1 AND tool_name = $2 \
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(TENANT)
        .bind(tool)
        .fetch_one(&mut *tx)
        .await
        .expect("the call was recorded");
        tx.commit().await.expect("commit");
        Outcome {
            tool: tool.to_string(),
            status,
            result: result.unwrap_or(Value::Null),
        }
    }

    async fn seed_projection(&self, run_id: &str) {
        let grants: Vec<Grant> = [
            ("read.internal", "fs", "**"),
            ("read.internal", "artifact", "*"),
            ("content.upload", "artifact", "*"),
        ]
        .into_iter()
        .map(|(class, kind, selector)| {
            Grant::from_json(&json!({
                "effect_class": class,
                "resource": { "kind": kind, "selector": selector },
                "constraints": {},
            }))
            .expect("grant")
        })
        .collect();
        let projection = CapabilityProjection::from_parts(
            CanonicalId::generate(Prefix::CapabilityProjection, &mut UlidGenerator::new()),
            ProjectionSubject::new(SubjectKind::Run, run_id),
            Vec::new(),
            grants,
            chrono::Utc::now(),
            Some(chrono::Utc::now() + chrono::Duration::hours(1)),
        )
        .expect("projection");
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        sqlx::query(
            "INSERT INTO capability_projections (id, tenant_id, workspace_id, subject_kind, \
             subject_id, inputs, grants, inputs_digest, computed_at, expires_at) \
             VALUES ($1, $2, $3, 'run', $4, $5, $6, $7, $8, $9)",
        )
        .bind(projection.id.to_string())
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(run_id)
        .bind(serde_json::to_value(&projection.inputs).expect("inputs"))
        .bind(serde_json::to_value(&projection.grants).expect("grants"))
        .bind(projection.inputs_digest.to_string())
        .bind(projection.computed_at)
        .bind(projection.expires_at)
        .execute(&mut *tx)
        .await
        .expect("projection row");
        tx.commit().await.expect("commit");
    }

    /// Make an artifact through the governed `artifact.create` tool and return its id.
    pub async fn create(
        &self,
        kind: &str,
        role: &str,
        title: &str,
        media_type: &str,
        source: &[u8],
    ) -> String {
        let outcome = self
            .call(
                "artifact.create",
                content_args(kind, role, title, media_type, source),
            )
            .await;
        assert!(outcome.ok(), "artifact.create refused: {outcome:#?}");
        outcome.result["artifact_id"]
            .as_str()
            .expect("artifact id")
            .to_string()
    }

    /// Add a version through the governed `artifact.update` tool.
    pub async fn update(&self, artifact_id: &str, media_type: &str, source: &[u8]) -> Outcome {
        let mut args = content_args_for_update(media_type, source);
        args["artifact_id"] = json!(artifact_id);
        self.call("artifact.update", args).await
    }

    pub async fn versions(&self, artifact_id: &str) -> Vec<VersionRow> {
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let rows: Vec<(i32, String, String, Option<String>, Option<Value>, Option<Value>)> = sqlx::query_as(
            "SELECT seq, id, media_type, parent_version_id, format_check, quality_record FROM artifact_versions \
             WHERE artifact_id = $1 AND tenant_id = $2 ORDER BY seq",
        )
        .bind(artifact_id)
        .bind(TENANT)
        .fetch_all(&mut *tx)
        .await
        .expect("versions");
        tx.commit().await.expect("commit");
        rows.into_iter()
            .map(
                |(seq, id, media_type, parent, format_check, quality_record)| VersionRow {
                    seq,
                    id,
                    media_type,
                    parent,
                    format_check: format_check.unwrap_or(Value::Null),
                    quality_record: quality_record.unwrap_or(Value::Null),
                },
            )
            .collect()
    }

    /// The newest artifact with a role, if any.
    pub async fn artifact_with_role(&self, role: &str) -> Option<String> {
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM artifacts WHERE tenant_id = $1 AND role = $2 ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(TENANT)
        .bind(role)
        .fetch_optional(&mut *tx)
        .await
        .expect("artifact");
        tx.commit().await.expect("commit");
        id
    }

    /// The bytes of a version, digest-verified by the store.
    pub async fn bytes(&self, version_id: &str) -> Vec<u8> {
        use quansio_server::artifacts::ArtifactStore;
        let store = ArtifactStore::new(self.objects.as_ref());
        let mut tx = self.pool.begin().await.expect("begin");
        let id = ArtifactVersionId::parse(version_id).expect("version id");
        let bytes = store
            .fetch_bytes(&mut tx, TENANT, USER, &id)
            .await
            .expect("bytes");
        tx.commit().await.expect("commit");
        bytes
    }

    /// The bytes of an evidence row, digest-verified by the store.
    pub async fn evidence_bytes(&self, evidence_id: &str) -> Vec<u8> {
        use quansio_server::artifacts::{EvidenceId, EvidenceStore};
        let store = EvidenceStore::new(self.objects.as_ref());
        let mut tx = self.pool.begin().await.expect("begin");
        let id = EvidenceId::parse(evidence_id).expect("evidence id");
        let bytes = store
            .fetch_bytes(&mut tx, TENANT, &id)
            .await
            .expect("evidence bytes");
        tx.commit().await.expect("commit");
        bytes
    }

    pub async fn finish(self) {
        drop_pool(&self.pool, &self.name).await;
    }
}

fn digest(bytes: &[u8]) -> String {
    Digest::of(bytes).to_string()
}

/// `artifact.create` arguments for a source.
pub fn content_args(kind: &str, role: &str, title: &str, media_type: &str, source: &[u8]) -> Value {
    let mut args = content_args_for_update(media_type, source);
    args["role"] = json!(role);
    args["kind"] = json!(kind);
    args["title"] = json!(title);
    args
}

fn content_args_for_update(media_type: &str, source: &[u8]) -> Value {
    use base64::Engine as _;
    match std::str::from_utf8(source) {
        Ok(text)
            if !media_type.starts_with("image/")
                && !media_type.starts_with("video/")
                && !media_type.starts_with("audio/") =>
        {
            json!({ "content": text, "content_digest": digest(source), "media_type": media_type })
        }
        _ => json!({
            "content_base64": base64::engine::general_purpose::STANDARD.encode(source),
            "content_digest": digest(source),
            "media_type": media_type,
        }),
    }
}
