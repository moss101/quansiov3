//! OPS-003: a run traced end to end across service boundaries, and no secret canary in any signal.
//!
//! One chat turn through the real composition — HTTP → runtime turn → gateway model source →
//! the real Python intelligence process → the dev stub provider → a tool call — started with a
//! desktop-style `traceparent`. The runtime's structured log lines, the intelligence process's log
//! lines and the spans exported over OTLP/HTTP to a real (in-test) collector must all carry that
//! trace, with every span's parent the span that caused it, across the process boundary. A secret
//! canary injected as the provider credential and in the message content must appear in none of
//! the runtime logs, the plane's logs, the exported spans, the metrics or the diagnostic bundle.
//! The bundle is bounded, tenant-scoped and for tenant administrators only; metrics are served only
//! to the operator's token.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`, `uv`, and the dev stub provider; without them the test
//! prints `BLOCKED_EXTERNAL` and returns.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::routing::post;
use quansio_server::control::schema;
use quansio_server::intelligence::model_source::{GatewayModelSource, ModelCallSettings};
use quansio_server::intelligence::IntelligenceClient;
use quansio_server::observability::{self, LogConfig, LogSink, OtlpExporter, DEFAULT_BUNDLE_LIMIT};
use quansio_server::runtime::context_bridge::GatewayContextBridge;
use serde_json::{json, Value};

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0PS003";
const OWNER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0PS003";
const MEMBER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0PSMBR";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0PS003";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0PS003";
const CANARY: &str = "qncy_ops003_canary_7d1f9e";
const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const DESKTOP_SPAN: &str = "00f067aa0ba902b7";
const METRICS_TOKEN: &str = "ops003-metrics-token";

async fn seed(pool: &sqlx::PgPool) {
    common::seed_tenant(pool, TENANT, OWNER, WORKSPACE, NODE).await;
    let mut tx = pool.begin().await.expect("begin");
    sqlx::query("INSERT INTO users (id, primary_email, display_name) VALUES ($1, $2, 'member')")
        .bind(MEMBER)
        .bind(format!("{MEMBER}@example.com"))
        .execute(&mut *tx)
        .await
        .expect("user");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) \
         VALUES ('tm_01J8Z3K6F1N8VQ2X5W9Y0PSMBR', $1, $2, 'member', 'active')",
    )
    .bind(TENANT)
    .bind(MEMBER)
    .execute(&mut *tx)
    .await
    .expect("membership");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9Y0PS003', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(json!([
        { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" }
    ]))
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
}

/// Span events (`event == "span"`) of `trace`, from JSON log lines.
fn spans(lines: &[String], trace: &str) -> Vec<Value> {
    lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| line["event"] == "span" && line["trace_id"] == trace)
        .collect()
}

fn named<'a>(spans: &'a [Value], name: &str) -> Vec<&'a Value> {
    spans.iter().filter(|span| span["name"] == name).collect()
}

fn id(span: &Value, key: &str) -> String {
    span[key].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn a_run_is_traced_end_to_end_and_no_canary_leaves_any_signal() {
    std::env::set_var("QUANSIO_TEST_SECRET_CANARY", CANARY);

    // A real OTLP/HTTP collector, in-process: it keeps every request body it receives.
    let exported = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = exported.clone();
    let collector = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let collector_addr = collector.local_addr().expect("addr");
    tokio::spawn(async move {
        let app = axum::Router::new().route(
            "/v1/traces",
            post(move |body: String| {
                let sink = sink.clone();
                async move {
                    sink.lock().expect("sink").push(body);
                    "{}"
                }
            }),
        );
        axum::serve(collector, app).await.expect("collector");
    });
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    assert!(observability::install(LogConfig {
        service: "runtime",
        sink: LogSink::Memory(lines.clone()),
        ring_capacity: 10_000,
        otlp: Some(OtlpExporter::start(
            &format!("http://{collector_addr}"),
            "quansio-runtime"
        )),
    }));

    let dir = tempfile::tempdir().expect("dir");
    let plane_log = dir.path().join("intelligence.log");
    let Some(service) = common::intelligence::start_with(common::intelligence::Options {
        stderr: Some(plane_log.clone()),
        env: vec![("QUANSIO_TEST_SECRET_CANARY".to_string(), CANARY.to_string())],
        credential: Some(CANARY.to_string()),
        ..common::intelligence::Options::default()
    })
    .await
    else {
        return;
    };
    let name = common::scratch_name("ops003_trace");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    schema::migrate(&pool).await.expect("migrate");
    seed(&pool).await;

    let settings = ModelCallSettings::load().expect("config/intelligence.yaml");
    let client = IntelligenceClient::connect_lazy(&service.endpoint).expect("endpoint");
    let context = Arc::new(GatewayContextBridge::new(client.clone(), settings.deadline));
    let state = quansio_server::api::ApiState::new(pool.clone())
        .expect("api")
        .with_metrics_token(METRICS_TOKEN)
        .with_model_source(Arc::new(GatewayModelSource::new(
            pool.clone(),
            client,
            context,
            settings,
        )));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let router = quansio_server::api::router(state);
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    let http = reqwest::Client::new();

    // The desktop starts the trace.
    let response = http
        .post(format!("http://{addr}/v1/commands/PostMessage"))
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", OWNER)
        .header("traceparent", format!("00-{TRACE}-{DESKTOP_SPAN}-01"))
        .json(&json!({
            "command_id": "cmd_01J8Z3K6F1N8VQ2X5W9Y0PS003",
            "params": {
                "workspace_id": WORKSPACE,
                "content": format!("Read the notes; my key is {CANARY}. [[stub:call fs__read {{\"path\": \"/work/root/notes.md\"}}]]"),
            }
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(response.status(), 200);
    let echoed = response
        .headers()
        .get("traceparent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        echoed.starts_with(&format!("00-{TRACE}-")),
        "the server answers inside the trace: {echoed}"
    );
    let body: Value = response.json().await.expect("json");
    let run_id = body["result"]["run_id"].as_str().expect("run").to_string();

    // -- the runtime's spans: one trace, each span the child of what caused it ------------
    let runtime = lines.lock().expect("lines").clone();
    let traced = spans(&runtime, TRACE);
    let http_span = named(&traced, "http.request");
    let http_span = http_span
        .iter()
        .find(|span| span["route"] == "/v1/commands/:command")
        .expect("the request span");
    assert_eq!(
        id(http_span, "parent_span_id"),
        DESKTOP_SPAN,
        "continues the desktop's span"
    );
    let turns = named(&traced, "runtime.turn");
    assert_eq!(turns.len(), 1, "{traced:?}");
    assert_eq!(id(turns[0], "parent_span_id"), id(http_span, "span_id"));
    assert_eq!(turns[0]["run_id"], run_id);
    let model_calls = named(&traced, "runtime.model_call");
    assert_eq!(model_calls.len(), 2, "a tool round and its answer");
    let model_call_ids: BTreeSet<String> =
        model_calls.iter().map(|span| id(span, "span_id")).collect();
    for span in &model_calls {
        assert_eq!(id(span, "parent_span_id"), id(turns[0], "span_id"));
    }
    let tools = named(&traced, "runtime.tool_call");
    assert_eq!(tools.len(), 1);
    assert_eq!(id(tools[0], "parent_span_id"), id(turns[0], "span_id"));
    assert_eq!(tools[0]["tool"], "fs.read");
    let rpcs = named(&traced, "intelligence.rpc");
    let rpc_methods: BTreeMap<String, String> = rpcs
        .iter()
        .map(|span| {
            (
                id(span, "span_id"),
                span["method"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(
        rpc_methods
            .values()
            .filter(|method| *method == "BuildContext")
            .count(),
        2
    );
    assert_eq!(
        rpc_methods
            .values()
            .filter(|method| *method == "FulfillModel")
            .count(),
        2
    );
    for span in &rpcs {
        assert!(
            model_call_ids.contains(&id(span, "parent_span_id")),
            "{span}"
        );
        assert_eq!(span["outcome"], "ok", "{span}");
    }

    // -- across the process boundary: the plane's spans continue the runtime's RPC spans ---
    let plane_lines: Vec<String> = std::fs::read_to_string(&plane_log)
        .expect("plane log")
        .lines()
        .map(str::to_string)
        .collect();
    let handled = spans(&plane_lines, TRACE);
    let handled: Vec<&Value> = handled
        .iter()
        .filter(|span| span["name"] == "intelligence.handle")
        .collect();
    assert_eq!(
        handled.len(),
        4,
        "every RPC is a span of the same trace in the plane: {plane_lines:?}"
    );
    for span in &handled {
        assert_eq!(span["service"], "intelligence");
        let parent = id(span, "parent_span_id");
        assert_eq!(
            rpc_methods.get(&parent).map(String::as_str),
            span["method"].as_str(),
            "the plane's span is the child of the runtime's call span: {span}"
        );
        assert_eq!(span["tenant_id"], TENANT);
        assert_eq!(span["outcome"], "ok");
    }

    // -- the spans reached the operator's collector over OTLP ------------------------------
    for _ in 0..40 {
        let sent = exported.lock().expect("exported").join("\n");
        if sent.contains("runtime.turn") && sent.contains("intelligence.rpc") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let bodies: Vec<Value> = exported
        .lock()
        .expect("exported")
        .iter()
        .map(|body| serde_json::from_str(body).expect("OTLP JSON"))
        .collect();
    let otlp_spans: Vec<&Value> = bodies
        .iter()
        .flat_map(|body| body["resourceSpans"].as_array().into_iter().flatten())
        .flat_map(|resource| resource["scopeSpans"].as_array().into_iter().flatten())
        .flat_map(|scope| scope["spans"].as_array().into_iter().flatten())
        .filter(|span| span["traceId"] == TRACE)
        .collect();
    let exported_names: BTreeSet<&str> = otlp_spans
        .iter()
        .filter_map(|span| span["name"].as_str())
        .collect();
    for expected in [
        "http.request",
        "runtime.turn",
        "runtime.model_call",
        "runtime.tool_call",
        "intelligence.rpc",
    ] {
        assert!(
            exported_names.contains(expected),
            "{expected} exported: {exported_names:?}"
        );
    }
    assert_eq!(
        bodies[0]["resourceSpans"][0]["resource"]["attributes"][0]["value"]["stringValue"],
        "quansio-runtime"
    );

    // -- metrics: the operator's token only ------------------------------------------------
    let unauthorized = http
        .get(format!("http://{addr}/metrics"))
        .send()
        .await
        .expect("metrics");
    assert_eq!(unauthorized.status(), 401);
    let metrics = http
        .get(format!("http://{addr}/metrics"))
        .bearer_auth(METRICS_TOKEN)
        .send()
        .await
        .expect("metrics")
        .text()
        .await
        .expect("text");
    assert!(metrics.contains("quansio_http_requests_total{route=\"/v1/commands/:command\",method=\"POST\",status=\"200\"} 1"), "{metrics}");
    assert!(
        metrics.contains(
            "quansio_model_calls_total{provider=\"openai-compatible\",stop_reason=\"tool_use\"} 1"
        ),
        "{metrics}"
    );
    assert!(
        metrics.contains(
            "quansio_model_calls_total{provider=\"openai-compatible\",stop_reason=\"end_turn\"} 1"
        ),
        "{metrics}"
    );
    assert!(
        metrics.contains("quansio_tool_calls_total{tool=\"fs.read\",outcome=\"completed\"} 1"),
        "{metrics}"
    );
    assert!(
        metrics
            .contains("quansio_intelligence_rpc_total{method=\"FulfillModel\",outcome=\"ok\"} 2"),
        "{metrics}"
    );

    // -- the diagnostic bundle: administrators, this tenant, bounded ------------------------
    let member = http
        .get(format!("http://{addr}/v1/diagnostics/bundle"))
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", MEMBER)
        .send()
        .await
        .expect("bundle");
    assert_eq!(member.status(), 403);
    let bundle_text = http
        .get(format!("http://{addr}/v1/diagnostics/bundle"))
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", OWNER)
        .send()
        .await
        .expect("bundle")
        .text()
        .await
        .expect("text");
    assert!(
        bundle_text.len() <= DEFAULT_BUNDLE_LIMIT,
        "{}",
        bundle_text.len()
    );
    let bundle: Value = serde_json::from_str(&bundle_text).expect("bundle json");
    let logged = bundle["logs"].as_array().expect("logs");
    assert!(!logged.is_empty());
    assert!(
        logged.iter().all(|line| line["tenant_id"] == TENANT),
        "only this tenant's lines"
    );
    assert!(logged
        .iter()
        .any(|line| line["trace_id"] == TRACE && line["name"] == "runtime.turn"));

    // -- something does try to log the canary ---------------------------------------------
    // A call whose correlation id carries it and whose scope the plane refuses: the runtime's RPC
    // span (in this tenant's bundle) and the plane's scope-rejection warning both log that id.
    let mut probe = IntelligenceClient::connect(&service.endpoint)
        .await
        .expect("connect");
    let refused = probe
        .classify_trust(
            &quansio_server::intelligence::CallScope {
                tenant_id: TENANT.to_string(),
                workspace_id: String::new(),
                correlation_id: format!("corr-{CANARY}"),
                deadline: Duration::from_secs(10),
                capability_projection_id: "cap_probe".to_string(),
            },
            "web_page",
            "probe",
        )
        .await;
    assert!(refused.is_err(), "an unscoped call is refused");
    let bundle_text = http
        .get(format!("http://{addr}/v1/diagnostics/bundle"))
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", OWNER)
        .send()
        .await
        .expect("bundle")
        .text()
        .await
        .expect("text");
    let runtime_probe = lines
        .lock()
        .expect("lines")
        .iter()
        .any(|line| line.contains("corr-[REDACTED:canary]"));
    assert!(runtime_probe, "the runtime logged the probe, redacted");
    assert!(
        bundle_text.contains("corr-[REDACTED:canary]"),
        "the bundle carries the probe, redacted"
    );
    for _ in 0..20 {
        if std::fs::read_to_string(&plane_log)
            .unwrap_or_default()
            .contains("corr-[REDACTED:canary]")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        std::fs::read_to_string(&plane_log)
            .expect("plane log")
            .contains("corr-[REDACTED:canary]"),
        "the plane logged the probe, redacted"
    );

    // -- no canary anywhere --------------------------------------------------------------
    let runtime_text = lines.lock().expect("lines").join("\n");
    let plane_text = std::fs::read_to_string(&plane_log).expect("plane log");
    let exported_text = exported.lock().expect("exported").join("\n");
    for (signal, text) in [
        ("runtime logs", &runtime_text),
        ("intelligence logs", &plane_text),
        ("exported spans", &exported_text),
        ("metrics", &metrics),
        ("diagnostic bundle", &bundle_text),
    ] {
        assert!(!text.is_empty(), "{signal} is empty, so it proves nothing");
        assert!(!text.contains(CANARY), "the canary leaked into {signal}");
    }

    // A malformed traceparent is not continued: the server starts its own trace.
    let fresh = http
        .get(format!("http://{addr}/v1/health"))
        .header(
            "traceparent",
            format!("00-{}-{DESKTOP_SPAN}-01", "0".repeat(32)),
        )
        .send()
        .await
        .expect("health");
    let started = fresh
        .headers()
        .get("traceparent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        started.starts_with("00-") && !started.contains(&"0".repeat(32)),
        "{started}"
    );

    common::drop_pool(&pool, &name).await;
    drop(service);
}
