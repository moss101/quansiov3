//! The MCP client against real servers (ADP-009, DOMAIN.md §7.5 "External tools").
//!
//! Every test talks to a **real MCP server process** (`tests/fixtures/mcp_server.py`, a stdlib-only implementation of the
//! protocol) over **real stdio JSON-RPC** or **real streamable HTTP** on loopback, through the real egress broker rules.
//! What the server received is read back from its own log, so "not sent" is a server-side count of zero, never an inference.
//!
//! * discovery: tools register as `mcp.<server>.<tool>`, classified by the configuration alone (a server's `readOnlyHint` and
//!   its injected description change nothing), schemas the vocabulary cannot state refuse the tool, pagination is read in full;
//! * calls: the answer is reduced to text and counts, labelled `UNTRUSTED_EXTERNAL`, bounded as it arrives, validated against
//!   the declaration; a server that asks the client for sampling is refused;
//! * transport hardening: the child's environment is the whitelist and nothing else; HTTP goes only where the egress broker
//!   says; timeouts, concurrency, an oversized message and a crash behave as the contract says; a restart is backed off and
//!   bounded;
//! * outcome: a call that was sent and not answered is in doubt for a mutating tool and a failure for a read, and a
//!   re-delivered dispatch is answered from the journal and never sent again;
//! * a tool list that changes is re-validated: a new tool is registered at the class the configuration gives it and a
//!   projection decides who sees it; a changed tool is a new version and a call planned against the old one is refused
//!   before anything is sent.
//!
//! Environment: `python3` on `PATH` (the fixture server). Without it every test prints `BLOCKED_EXTERNAL` and returns.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use quansio_machine::egress::{EgressGrant, NetworkPolicy};
use quansio_qworkerd::browser::tools::ReplyStatus;
use quansio_qworkerd::mcp::{broker_gate, EgressGate, McpCallContext, McpHost};
use quansio_tools::{McpConfig, RegistryHandle, ToolDeclaration};
use serde_json::{json, Value};

fn python() -> Option<String> {
    let output = Command::new("which").arg("python3").output().ok()?;
    let path = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if path.is_empty() {
        eprintln!("BLOCKED_EXTERNAL: python3 is not on PATH; the MCP fixture server cannot run");
        None
    } else {
        Some(path)
    }
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_server.py")
}

/// The shipped configuration with these servers, and these limits replaced.
fn config(servers: &str, limits: &[(&str, &str)]) -> McpConfig {
    let mut text = include_str!("../../../config/mcp.yaml").replace("servers: []", "");
    for (key, value) in limits {
        let prefix = format!("  {key}: ");
        let start = text
            .find(&prefix)
            .unwrap_or_else(|| panic!("no limit {key}"));
        let end = start + text[start..].find('\n').expect("line end");
        text.replace_range(start..end, &format!("{prefix}{value}"));
    }
    text.push_str("servers:\n");
    text.push_str(servers);
    McpConfig::from_yaml(&text).expect("a valid configuration")
}

fn stdio_server(
    name: &str,
    python: &str,
    log: &Path,
    state: &Path,
    extra_args: &str,
    tools: &str,
) -> String {
    stdio_server_keys(name, python, log, state, extra_args, "", tools)
}

/// Like [`stdio_server`], with more keys of the server entry itself (before its `tools`).
fn stdio_server_keys(
    name: &str,
    python: &str,
    log: &Path,
    state: &Path,
    extra_args: &str,
    keys: &str,
    tools: &str,
) -> String {
    format!(
        "  - name: {name}
    transport: stdio
    command: {python}
    args: [\"{}\", \"--transport\", \"stdio\", \"--log\", \"{}\", \"--state\", \"{}\"{extra_args}]
    env: {{ DEMO_MODE: fixture }}
{keys}    tools:
{tools}",
        fixture().display(),
        log.display(),
        state.display()
    )
}

fn scratch(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(label)
        .tempdir()
        .expect("tempdir")
}

fn log_entries(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn calls_of(log: &Path, tool: &str) -> usize {
    log_entries(log)
        .iter()
        .filter(|entry| entry["method"] == "tools/call" && entry["name"] == tool)
        .count()
}

fn host(config: McpConfig) -> McpHost {
    McpHost::new(config, RegistryHandle::builtin(), Arc::new(NoGrantsGate))
}

/// An egress gate that allows nothing: stdio servers never ask it.
struct NoGrantsGate;

#[async_trait::async_trait]
impl EgressGate for NoGrantsGate {
    async fn permit(
        &self,
        _url: &str,
        _resolved: &[std::net::IpAddr],
        _at: &str,
    ) -> Result<(), String> {
        Err("no_grant".to_string())
    }
}

fn version_of(host: &McpHost, tool: &str) -> u32 {
    host.registry()
        .current()
        .get(tool)
        .unwrap_or_else(|| panic!("{tool} is registered"))
        .version
}

async fn call(
    host: &McpHost,
    tool: &str,
    args: Value,
    token: &str,
) -> quansio_qworkerd::mcp::McpReply {
    let version = version_of(host, tool);
    host.dispatch(
        tool,
        version,
        &args,
        &McpCallContext {
            dispatch_token: token,
            effect_id: "eff_test",
        },
    )
    .await
}

const TOOLS: &str = "      echo: { effect_class: read.external }
      env_dump: { effect_class: read.external }
      mixed: { effect_class: read.external }
      big: { effect_class: read.external }
      fail: { effect_class: read.external }
      structured: { effect_class: read.external }
      sampling: { effect_class: read.external }
      note_create: { effect_class: record.create }
";

// ---------------------------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn discovery_registers_the_tools_the_configuration_classifies_and_refuses_the_ones_it_cannot_state(
) {
    let Some(python) = python() else { return };
    let dir = scratch("mcp-discovery");
    let log = dir.path().join("calls.jsonl");
    let servers = stdio_server(
        "demo",
        &python,
        &log,
        &dir.path().join("state"),
        ", \"--page-size\", \"4\"",
        TOOLS,
    );
    let host = host(config(&servers, &[]));
    let report = host.sync().await;
    let sync = &report.servers[0];
    assert!(sync.connected, "{sync:#?}");
    assert!(
        log_entries(&log)
            .iter()
            .filter(|e| e["method"] == "tools/list")
            .count()
            >= 4,
        "a paginated list is read to its end"
    );

    let registry = host.registry().current();
    let class_of = |name: &str| {
        let declaration: &ToolDeclaration = registry
            .get(name)
            .unwrap_or_else(|| panic!("{name} is registered"));
        match &declaration.effect_class {
            quansio_tools::EffectClassRule::Static(class) => class.as_str().to_string(),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(class_of("mcp.demo.echo"), "read.external");
    assert_eq!(class_of("mcp.demo.note_create"), "record.create");
    // Nobody classified these, so they settle as the highest-risk default: never a read.
    assert_eq!(class_of("mcp.demo.crash"), "data.upload.protected");
    assert_eq!(class_of("mcp.demo.slow"), "data.upload.protected");
    // The server's own claim that this tool is read-only and harmless is not a classification.
    assert_eq!(
        class_of("mcp.demo.hint"),
        "data.upload.protected",
        "a readOnlyHint changes nothing"
    );

    // The injected description and the schema's own prose never reach a model's tool definition.
    let poison = registry.get("mcp.demo.poison").expect("poison");
    assert!(
        !poison.description.to_lowercase().contains("ignore"),
        "{}",
        poison.description
    );
    assert!(
        !poison.description.contains("attacker"),
        "{}",
        poison.description
    );
    let schema = poison.input_schema.to_string();
    assert!(!schema.contains("obey"), "{schema}");
    assert!(quansio_tools::check_vocabulary("t", &poison.input_schema).is_ok());

    // What the vocabulary cannot state is refused, by rule, and nothing else is.
    let rules: Vec<(&str, &str)> = sync
        .rejected
        .iter()
        .map(|r| (r.remote_name.as_str(), r.rule))
        .collect();
    assert!(
        rules.contains(&("oneof_tool", "mcp.schema.keyword")),
        "{rules:?}"
    );
    assert!(
        rules.contains(&("bad_root", "mcp.schema.root")),
        "{rules:?}"
    );
    assert!(
        registry.get("mcp.demo.oneof_tool").is_none()
            && registry.get("mcp.demo.bad_root").is_none()
    );
    host.close_all().await;
}

#[tokio::test]
async fn an_unreachable_or_unconfigured_or_disabled_server_registers_nothing() {
    let Some(python) = python() else { return };
    let dir = scratch("mcp-none");
    let log = dir.path().join("calls.jsonl");
    let down = "  - name: gone
    transport: stdio
    command: /nonexistent/mcp-server
";
    let host = host(config(down, &[]));
    let sync = host.sync().await;
    assert!(!sync.servers[0].connected);
    assert_eq!(
        sync.servers[0].failure.as_deref(),
        Some("mcp.transport.unreachable")
    );
    assert_eq!(host.registry().current().external().count(), 0);

    // Switched off: nothing starts at all.
    let servers = stdio_server("demo", &python, &log, &dir.path().join("state"), "", TOOLS);
    let mut text = config(&servers, &[]);
    text.enabled = false;
    let off = McpHost::new(text, RegistryHandle::builtin(), Arc::new(NoGrantsGate));
    let report = off.sync().await;
    assert_eq!(report.servers[0].failure.as_deref(), Some("mcp.disabled"));
    assert!(
        log_entries(&log).is_empty(),
        "a disabled client starts no server"
    );
    let reply = off
        .dispatch(
            "mcp.demo.echo",
            1,
            &json!({"text": "x"}),
            &McpCallContext {
                dispatch_token: "t",
                effect_id: "e",
            },
        )
        .await;
    assert_eq!(
        reply.error.as_ref().expect("error")["code"],
        json!("MCP_SERVER_UNAVAILABLE")
    );
    // A server nobody configured.
    let none = host.refresh_server("nobody").await;
    assert_eq!(none.failure.as_deref(), Some("mcp.server.unknown"));
}

// ---------------------------------------------------------------------------------------------
// Calls
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_call_comes_back_reduced_labelled_and_bounded() {
    let Some(python) = python() else { return };
    let dir = scratch("mcp-call");
    let log = dir.path().join("calls.jsonl");
    let servers = stdio_server("demo", &python, &log, &dir.path().join("state"), "", TOOLS);
    let host = host(config(&servers, &[("max_result_bytes", "2048")]));
    assert!(host.sync().await.servers[0].connected);

    let echoed = call(&host, "mcp.demo.echo", json!({"text": "hello"}), "tok-1").await;
    assert_eq!(echoed.status, ReplyStatus::Ok, "{echoed:#?}");
    assert_eq!(echoed.output["text"], json!("hello"));
    assert_eq!(echoed.output["trust"], json!("UNTRUSTED_EXTERNAL"));
    assert_eq!(echoed.ledger_outcome(), "SETTLED_SUCCESS");
    let declaration = host
        .registry()
        .current()
        .get("mcp.demo.echo")
        .cloned()
        .expect("declaration");
    assert_eq!(
        quansio_tools::first_violation(&declaration.output_schema, &echoed.output),
        None,
        "the result is exactly what the declaration says"
    );
    assert!(echoed.to_json()["trust"] == json!("UNTRUSTED_EXTERNAL"));
    assert!(echoed.evidence.is_some(), "the call's record is evidence");

    // Other content is counted and not returned: no image bytes, no resource text.
    let mixed = call(&host, "mcp.demo.mixed", json!({}), "tok-2").await;
    assert_eq!(mixed.output["text"], json!("first\nsecond"));
    assert_eq!(mixed.output["omitted_blocks"], json!(2));
    let wire = mixed.to_json().to_string();
    assert!(
        !wire.contains("AAAA") && !wire.contains("hidden") && !wire.contains("file:///secret"),
        "{wire}"
    );

    // The text is cut at the bound and says so.
    let big = call(&host, "mcp.demo.big", json!({"bytes": 100_000}), "tok-3").await;
    assert_eq!(big.status, ReplyStatus::Ok);
    assert_eq!(big.output["truncated"], json!(true));
    assert!(big.output["text"].as_str().expect("text").len() <= 2048);

    // A tool-level error is a known failure.
    let failed = call(&host, "mcp.demo.fail", json!({}), "tok-4").await;
    assert_eq!(failed.status, ReplyStatus::Error);
    assert_eq!(
        failed.error.as_ref().expect("error")["code"],
        json!("MCP_TOOL_ERROR")
    );
    assert_eq!(failed.ledger_outcome(), "SETTLED_FAILED");

    // Structured content is returned as text, also as data.
    let structured = call(&host, "mcp.demo.structured", json!({}), "tok-5").await;
    assert_eq!(structured.output["text"], json!("{\"answer\":42}"));

    // An argument the schema does not declare is refused before the server hears of it.
    let before = log_entries(&log).len();
    let refused = call(
        &host,
        "mcp.demo.echo",
        json!({"text": "x", "extra": 1}),
        "tok-6",
    )
    .await;
    assert_eq!(
        refused.error.as_ref().expect("error")["code"],
        json!("MCP_INVALID_REQUEST")
    );
    assert_eq!(log_entries(&log).len(), before, "nothing was sent");
    // A name nobody registered, and a built-in tool's name.
    let unknown = host
        .dispatch(
            "mcp.demo.nothing",
            1,
            &json!({}),
            &McpCallContext {
                dispatch_token: "t7",
                effect_id: "e",
            },
        )
        .await;
    assert_eq!(
        unknown.error.as_ref().expect("error")["code"],
        json!("MCP_TOOL_NOT_REGISTERED")
    );
    let builtin = host
        .dispatch(
            "fs.read",
            1,
            &json!({"path": "x"}),
            &McpCallContext {
                dispatch_token: "t8",
                effect_id: "e",
            },
        )
        .await;
    assert_eq!(
        builtin.error.as_ref().expect("error")["code"],
        json!("MCP_TOOL_NOT_REGISTERED")
    );
    host.close_all().await;
}

#[tokio::test]
async fn a_redelivered_dispatch_is_answered_from_the_journal_and_never_sent_again() {
    let Some(python) = python() else { return };
    let dir = scratch("mcp-replay");
    let log = dir.path().join("calls.jsonl");
    let servers = stdio_server("demo", &python, &log, &dir.path().join("state"), "", TOOLS);
    let host = host(config(&servers, &[]));
    assert!(host.sync().await.servers[0].connected);
    let first = call(
        &host,
        "mcp.demo.note_create",
        json!({"title": "once"}),
        "tok-once",
    )
    .await;
    assert_eq!(first.status, ReplyStatus::Ok);
    let again = call(
        &host,
        "mcp.demo.note_create",
        json!({"title": "once"}),
        "tok-once",
    )
    .await;
    assert!(again.replayed);
    assert_eq!(again.output, first.output);
    assert_eq!(calls_of(&log, "note_create"), 1, "the server saw one call");
    assert_eq!(
        std::fs::read_dir(dir.path().join("state"))
            .expect("state")
            .count(),
        1,
        "and made one note"
    );
    host.close_all().await;
}

#[tokio::test]
async fn a_server_that_asks_the_client_for_something_is_refused() {
    let Some(python) = python() else { return };
    let dir = scratch("mcp-sampling");
    let log = dir.path().join("calls.jsonl");
    let servers = stdio_server("demo", &python, &log, &dir.path().join("state"), "", TOOLS);
    let host = host(config(&servers, &[]));
    assert!(host.sync().await.servers[0].connected);
    let reply = call(&host, "mcp.demo.sampling", json!({}), "tok").await;
    assert_eq!(reply.status, ReplyStatus::Ok, "{reply:#?}");
    let answer = log_entries(&log)
        .into_iter()
        .find_map(|entry| entry.get("client_answer_to_sampling").cloned())
        .expect("the server recorded what the client answered");
    assert_eq!(answer["error"]["code"], json!(-32601), "{answer}");
    let initialize = log_entries(&log)
        .into_iter()
        .find(|e| e["method"] == "initialize")
        .expect("initialize");
    assert!(initialize["arguments"].is_null());
    host.close_all().await;
}

// ---------------------------------------------------------------------------------------------
// Transport hardening
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_server_process_gets_the_whitelisted_environment_and_nothing_else() {
    let Some(python) = python() else { return };
    // Variables a provider key or a connector token would live in, and the worker's own configuration.
    for (name, value) in [
        ("ANTHROPIC_API_KEY", "sk-ant-canary-0001"),
        ("GITHUB_TOKEN", "ghp_canary0002"),
        ("QUANSIO_SERVER_URL", "http://canary.invalid"),
        ("QUANSIO_TEST_CANARY", "canary-0004"),
        ("AWS_SECRET_ACCESS_KEY", "canary-0005"),
    ] {
        std::env::set_var(name, value);
    }
    let dir = scratch("mcp-env");
    let log = dir.path().join("calls.jsonl");
    let servers = stdio_server("demo", &python, &log, &dir.path().join("state"), "", TOOLS);
    let host = host(config(&servers, &[]));
    assert!(host.sync().await.servers[0].connected);
    let reply = call(&host, "mcp.demo.env_dump", json!({}), "tok").await;
    let names: Vec<String> = serde_json::from_str(reply.output["text"].as_str().expect("text"))
        .expect("a list of names");
    for forbidden in [
        "ANTHROPIC_API_KEY",
        "GITHUB_TOKEN",
        "QUANSIO_SERVER_URL",
        "QUANSIO_TEST_CANARY",
        "AWS_SECRET_ACCESS_KEY",
        "HOME",
        "USER",
        "SHELL",
    ] {
        assert!(
            !names.iter().any(|n| n == forbidden),
            "{forbidden} reached the server: {names:?}"
        );
    }
    assert!(
        names.iter().any(|n| n == "DEMO_MODE"),
        "the entry's own value is set: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "PATH"),
        "a whitelisted name is inherited: {names:?}"
    );
    // What the interpreter (or the platform's shim for it) adds on its own is not the worker's environment:
    // measure it by starting the same program with nothing at all.
    let baseline = Command::new(&python)
        .env_clear()
        .args([
            "-c",
            "import os, json; print(json.dumps(sorted(os.environ)))",
        ])
        .output()
        .expect("the interpreter runs with an empty environment");
    let by_itself: Vec<String> = serde_json::from_slice(&baseline.stdout).expect("names");
    let allowed = ["DEMO_MODE", "PATH", "LANG", "LC_ALL", "TMPDIR"];
    let extra: Vec<&String> = names
        .iter()
        .filter(|n| !allowed.contains(&n.as_str()) && !by_itself.contains(n))
        .collect();
    assert!(extra.is_empty(), "unexpected environment names: {extra:?}");
    let wire = reply.to_json().to_string();
    assert!(
        !wire.contains("canary"),
        "no canary value anywhere in the reply"
    );
    host.close_all().await;
}

#[tokio::test]
async fn a_call_that_is_sent_and_not_answered_is_in_doubt_for_a_mutation_and_a_failure_for_a_read()
{
    let Some(python) = python() else { return };
    let dir = scratch("mcp-doubt");
    let log_rw = dir.path().join("rw.jsonl");
    let log_ro = dir.path().join("ro.jsonl");
    let servers = format!(
        "{}{}",
        stdio_server("rw", &python, &log_rw, &dir.path().join("s1"), "", "      slow: { effect_class: record.update }\n      echo: { effect_class: read.external }\n"),
        stdio_server("ro", &python, &log_ro, &dir.path().join("s2"), "", "      slow: { effect_class: read.external }\n"),
    );
    let host = host(config(&servers, &[("call_timeout_ms", "300")]));
    let report = host.sync().await;
    assert!(report.servers.iter().all(|s| s.connected), "{report:#?}");

    let write = call(&host, "mcp.rw.slow", json!({"seconds": 3}), "tok-w").await;
    assert_eq!(write.status, ReplyStatus::InDoubt, "{write:#?}");
    assert_eq!(write.ledger_outcome(), "OUTCOME_UNKNOWN");
    assert_eq!(
        write.error.as_ref().expect("error")["retryable"],
        json!(false)
    );
    assert_eq!(calls_of(&log_rw, "slow"), 1, "the server received it");
    // The same dispatch again: answered, not sent.
    let again = call(&host, "mcp.rw.slow", json!({"seconds": 3}), "tok-w").await;
    assert!(again.replayed && again.status == ReplyStatus::InDoubt);
    assert_eq!(
        calls_of(&log_rw, "slow"),
        1,
        "it was never sent a second time"
    );

    let read = call(&host, "mcp.ro.slow", json!({"seconds": 3}), "tok-r").await;
    assert_eq!(read.status, ReplyStatus::Error, "{read:#?}");
    assert_eq!(
        read.error.as_ref().expect("error")["code"],
        json!("TOOL_TIMEOUT")
    );
    assert_eq!(read.ledger_outcome(), "SETTLED_FAILED");
    host.close_all().await;
}

#[tokio::test]
async fn a_dispatch_whose_future_was_abandoned_after_it_was_sent_is_in_doubt_and_not_resent() {
    let Some(python) = python() else { return };
    let dir = scratch("mcp-abandon");
    let log = dir.path().join("calls.jsonl");
    let servers = stdio_server(
        "demo",
        &python,
        &log,
        &dir.path().join("state"),
        "",
        "      slow: { effect_class: record.update }\n",
    );
    let host = host(config(&servers, &[]));
    assert!(host.sync().await.servers[0].connected);
    let version = version_of(&host, "mcp.demo.slow");
    // The worker stops (its future is dropped) after the request is on the wire and before any answer.
    let outcome = tokio::time::timeout(
        Duration::from_millis(600),
        host.dispatch(
            "mcp.demo.slow",
            version,
            &json!({"seconds": 2}),
            &McpCallContext {
                dispatch_token: "tok-a",
                effect_id: "e",
            },
        ),
    )
    .await;
    assert!(outcome.is_err(), "the dispatch was cut off mid-flight");
    assert_eq!(calls_of(&log, "slow"), 1);
    let resumed = call(&host, "mcp.demo.slow", json!({"seconds": 2}), "tok-a").await;
    assert_eq!(resumed.status, ReplyStatus::InDoubt, "{resumed:#?}");
    assert_eq!(
        calls_of(&log, "slow"),
        1,
        "a delivery with no recorded outcome is never sent again"
    );
    host.close_all().await;
}

#[tokio::test]
async fn a_server_that_exits_mid_call_is_restarted_with_backoff_and_the_call_is_in_doubt() {
    let Some(python) = python() else { return };
    let dir = scratch("mcp-crash");
    let log = dir.path().join("calls.jsonl");
    let tools = "      crash: { effect_class: record.update }\n      echo: { effect_class: read.external }\n";
    let servers = stdio_server("demo", &python, &log, &dir.path().join("state"), "", tools);
    let host = host(config(&servers, &[]));
    assert!(host.sync().await.servers[0].connected);
    let crashed = call(&host, "mcp.demo.crash", json!({}), "tok-c").await;
    assert_eq!(crashed.status, ReplyStatus::InDoubt, "{crashed:#?}");
    assert_eq!(calls_of(&log, "crash"), 1);
    // The next call finds the server gone, restarts it (after the configured delay) and works.
    let started = std::time::Instant::now();
    let next = call(&host, "mcp.demo.echo", json!({"text": "again"}), "tok-e").await;
    assert_eq!(next.status, ReplyStatus::Ok, "{next:#?}");
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "the first restart waits the configured 250 ms"
    );
    assert_eq!(
        log_entries(&log)
            .iter()
            .filter(|e| e["method"] == "initialize")
            .count(),
        2,
        "the server was started twice"
    );
    host.close_all().await;
}

#[tokio::test]
async fn a_server_that_never_stays_up_is_given_up_on_after_its_restart_budget() {
    let Some(python) = python() else { return };
    let dir = scratch("mcp-giveup");
    let count = dir.path().join("starts.txt");
    // A server that records each start and exits at once.
    let servers = format!(
        "  - name: flaky
    transport: stdio
    command: {python}
    args: [\"-c\", \"open('{}', 'a').write('x'); import sys; sys.exit(1)\"]
",
        count.display()
    );
    let host = host(config(&servers, &[]));
    for _ in 0..8 {
        let sync = host.refresh_server("flaky").await;
        assert!(!sync.connected);
    }
    let starts = std::fs::read_to_string(&count).expect("starts").len();
    assert_eq!(
        starts,
        1 + 3,
        "one start and max_restarts (3) restarts, then no more"
    );
    let last = host.refresh_server("flaky").await;
    assert_eq!(last.failure.as_deref(), Some("mcp.transport.unreachable"));
    host.close_all().await;
}

#[tokio::test]
async fn a_server_serves_at_most_its_concurrency_and_the_rest_are_refused_not_queued_forever() {
    let Some(python) = python() else { return };
    let dir = scratch("mcp-busy");
    let log = dir.path().join("calls.jsonl");
    let servers = stdio_server_keys(
        "demo",
        &python,
        &log,
        &dir.path().join("state"),
        "",
        "    max_concurrent_calls: 1\n",
        "      slow: { effect_class: read.external }\n",
    );
    let host = host(config(&servers, &[("queue_timeout_ms", "200")]));
    assert!(host.sync().await.servers[0].connected);
    let (first, second) = tokio::join!(
        call(&host, "mcp.demo.slow", json!({"seconds": 1}), "tok-1"),
        async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            call(&host, "mcp.demo.slow", json!({"seconds": 1}), "tok-2").await
        }
    );
    assert_eq!(first.status, ReplyStatus::Ok, "{first:#?}");
    assert_eq!(
        second.error.as_ref().expect("error")["code"],
        json!("MCP_BUSY"),
        "{second:#?}"
    );
    assert_eq!(
        calls_of(&log, "slow"),
        1,
        "the refused call never reached the server"
    );
    host.close_all().await;
}

#[tokio::test]
async fn an_answer_longer_than_the_bound_ends_the_conversation_and_never_fills_the_workers_memory()
{
    let Some(python) = python() else { return };
    let dir = scratch("mcp-flood");
    let log = dir.path().join("calls.jsonl");
    let tools = "      flood: { effect_class: record.update }\n      echo: { effect_class: read.external }\n";
    let servers = stdio_server("demo", &python, &log, &dir.path().join("state"), "", tools);
    let host = host(config(&servers, &[("max_message_bytes", "65536")]));
    assert!(host.sync().await.servers[0].connected);
    let flooded = call(&host, "mcp.demo.flood", json!({}), "tok-f").await;
    // The server's "answer" is a 3 MiB line. It is cut by the bound as it is read, the conversation is over, and a
    // mutating call that was sent and never answered is in doubt.
    assert_eq!(flooded.status, ReplyStatus::InDoubt, "{flooded:#?}");
    // The conversation ended, so the next call starts a fresh server (a second `initialize` in its log).
    let next = call(&host, "mcp.demo.echo", json!({"text": "after"}), "tok-n").await;
    assert_eq!(next.status, ReplyStatus::Ok, "{next:#?}");
    assert_eq!(next.output["text"], json!("after"));
    assert_eq!(
        log_entries(&log)
            .iter()
            .filter(|e| e["method"] == "initialize")
            .count(),
        2,
        "the over-long line ended the first conversation"
    );
    host.close_all().await;
}

// ---------------------------------------------------------------------------------------------
// A tool list that changes
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_tool_that_appears_mid_run_is_registered_at_the_highest_risk_class_and_never_exposed_by_a_read_grant(
) {
    let Some(python) = python() else { return };
    let dir = scratch("mcp-added");
    let log = dir.path().join("calls.jsonl");
    let tools = "      add_tool: { effect_class: record.update }\n      echo: { effect_class: read.external }\n";
    let servers = stdio_server("demo", &python, &log, &dir.path().join("state"), "", tools);
    let host = host(config(&servers, &[]));
    assert!(host.sync().await.servers[0].connected);
    assert!(host
        .registry()
        .current()
        .get("mcp.demo.shiny_new")
        .is_none());

    // The server adds a tool and says so; the next call re-reads the list before it goes out.
    let added = call(&host, "mcp.demo.add_tool", json!({}), "tok-a").await;
    assert_eq!(added.status, ReplyStatus::Ok);
    let echoed = call(&host, "mcp.demo.echo", json!({"text": "x"}), "tok-e").await;
    assert_eq!(echoed.status, ReplyStatus::Ok, "{echoed:#?}");
    let registry = host.registry().current();
    let shiny = registry
        .get("mcp.demo.shiny_new")
        .expect("the new tool is registered after revalidation");
    assert!(
        matches!(&shiny.effect_class, quansio_tools::EffectClassRule::Static(c) if c.as_str() == "data.upload.protected")
    );

    // A projection that grants reads on the whole server does not expose it: appearing grants nothing.
    let grants = vec![quansio_capability::Grant::from_json(&json!({
        "effect_class": "read.external",
        "resource": { "kind": "connector", "selector": "mcp.demo/*" },
        "constraints": {},
    }))
    .expect("grant")];
    let projection = quansio_capability::projection::CapabilityProjection::from_parts(
        quansio_core::CanonicalId::generate(
            quansio_core::Prefix::CapabilityProjection,
            &mut quansio_core::UlidGenerator::new(),
        ),
        quansio_capability::ProjectionSubject::new(
            quansio_capability::SubjectKind::Run,
            "run_test",
        ),
        Vec::new(),
        grants,
        chrono_now(),
        None,
    )
    .expect("projection");
    let exposed = registry.exposed_names(&projection, chrono_now());
    assert!(exposed.contains(&"mcp.demo.echo"), "{exposed:?}");
    assert!(
        !exposed.contains(&"mcp.demo.shiny_new"),
        "a tool that appeared is not exposed by a read grant: {exposed:?}"
    );
    host.close_all().await;
}

fn chrono_now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

#[tokio::test]
async fn a_call_planned_against_a_tool_that_then_changed_is_refused_before_anything_is_sent() {
    let Some(python) = python() else { return };
    let dir = scratch("mcp-changed");
    let log = dir.path().join("calls.jsonl");
    let tools = "      change_schema: { effect_class: record.update }\n      echo: { effect_class: read.external }\n";
    let servers = stdio_server("demo", &python, &log, &dir.path().join("state"), "", tools);
    let host = host(config(&servers, &[]));
    assert!(host.sync().await.servers[0].connected);
    let planned_version = version_of(&host, "mcp.demo.echo");
    assert_eq!(planned_version, 1);

    let changed = call(&host, "mcp.demo.change_schema", json!({}), "tok-c").await;
    assert_eq!(changed.status, ReplyStatus::Ok);
    // A call that was planned (at version 1) before the change arrives after it.
    let echoes_before = calls_of(&log, "echo");
    let stale = host
        .dispatch(
            "mcp.demo.echo",
            planned_version,
            &json!({"text": "old"}),
            &McpCallContext {
                dispatch_token: "tok-s",
                effect_id: "e",
            },
        )
        .await;
    assert_eq!(
        stale.error.as_ref().expect("error")["code"],
        json!("MCP_TOOL_CHANGED"),
        "{stale:#?}"
    );
    assert_eq!(
        stale.error.as_ref().expect("error")["retryable"],
        json!(true)
    );
    assert_eq!(calls_of(&log, "echo"), echoes_before, "nothing was sent");

    // Planned again, against version 2, the call goes through and may use the new field.
    assert_eq!(version_of(&host, "mcp.demo.echo"), 2);
    let fresh = call(
        &host,
        "mcp.demo.echo",
        json!({"text": "new", "loud": true}),
        "tok-n",
    )
    .await;
    assert_eq!(fresh.status, ReplyStatus::Ok, "{fresh:#?}");
    assert_eq!(calls_of(&log, "echo"), echoes_before + 1);
    host.close_all().await;
}

// ---------------------------------------------------------------------------------------------
// Streamable HTTP through the egress broker
// ---------------------------------------------------------------------------------------------

struct HttpServer {
    child: Child,
    port: u16,
}

impl HttpServer {
    fn start(python: &str, log: &Path, sse: bool) -> Self {
        let dir = log.parent().expect("dir");
        let port_file = dir.join("port.txt");
        let mut command = Command::new(python);
        command
            .arg(fixture())
            .args(["--transport", "http", "--port", "0", "--port-file"])
            .arg(&port_file)
            .arg("--log")
            .arg(log)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if sse {
            command.arg("--sse");
        }
        let child = command.spawn().expect("the fixture server starts");
        let port = (0..100)
            .find_map(|_| {
                std::thread::sleep(Duration::from_millis(100));
                std::fs::read_to_string(&port_file)
                    .ok()
                    .and_then(|text| text.trim().parse().ok())
            })
            .expect("the server reports its port");
        Self { child, port }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn http_config(port: u16, tools: &str) -> McpConfig {
    config(
        &format!("  - name: web\n    transport: http\n    url: http://127.0.0.1:{port}/mcp\n    tools:\n{tools}"),
        &[],
    )
}

fn loopback_gate(port: u16, allow_loopback: bool, with_grant: bool) -> Arc<dyn EgressGate> {
    let rules = if allow_loopback {
        json!([{ "effect_class": "network.egress.new_destination", "decision": "allow",
                 "conditions": { "allowed_address_classes": ["loopback"] } }])
    } else {
        json!([{ "effect_class": "network.egress.new_destination", "decision": "allow" }])
    };
    let policy =
        NetworkPolicy::from_rules("pol_01J8Z3K6F1N8VQ2X5W9Y0ABCDE", 1, &rules).expect("policy");
    let grants = if with_grant {
        vec![EgressGrant {
            effect_id: "eff_grant".to_string(),
            tenant_id: "tn_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
            target_id: "tgt_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
            target_generation: 1,
            capability_id: None,
            policy_id: "pol_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
            policy_version: 1,
            host: "127.0.0.1".to_string(),
            port,
            issued_at: "2020-01-01T00:00:00Z".to_string(),
            expires_at: "2999-01-01T00:00:00Z".to_string(),
            revoked_at: None,
        }]
    } else {
        Vec::new()
    };
    broker_gate(
        "tn_01J8Z3K6F1N8VQ2X5W9Y0ABCDE",
        "tgt_01J8Z3K6F1N8VQ2X5W9Y0ABCDE",
        1,
        policy,
        grants,
    )
}

#[tokio::test]
async fn an_http_server_is_reached_through_the_egress_broker_and_speaks_the_session_protocol() {
    let Some(python) = python() else { return };
    let dir = scratch("mcp-http");
    let log = dir.path().join("calls.jsonl");
    let server = HttpServer::start(&python, &log, false);
    let host = McpHost::new(
        http_config(server.port, "      echo: { effect_class: read.external }\n"),
        RegistryHandle::builtin(),
        loopback_gate(server.port, true, true),
    );
    let report = host.sync().await;
    assert!(report.servers[0].connected, "{report:#?}");
    let reply = call(&host, "mcp.web.echo", json!({"text": "over http"}), "tok").await;
    assert_eq!(reply.status, ReplyStatus::Ok, "{reply:#?}");
    assert_eq!(reply.output["text"], json!("over http"));
    // The server only answers requests that carry its session id, so success proves the header; the protocol
    // revision the server negotiated is stated on every request after initialize.
    let calls: Vec<Value> = log_entries(&log)
        .into_iter()
        .filter(|e| e["http"] == json!(true) && e["session"].is_string())
        .collect();
    assert!(!calls.is_empty());
    assert!(
        calls.iter().all(|e| e["protocol"] == json!("2025-06-18")),
        "{calls:#?}"
    );
    assert!(calls.iter().all(|e| e["accept"]
        .as_str()
        .is_some_and(|a| a.contains("text/event-stream"))));
}

#[tokio::test]
async fn http_egress_is_deny_by_default_and_a_refused_destination_is_never_contacted() {
    let Some(python) = python() else { return };
    let dir = scratch("mcp-egress");
    let log = dir.path().join("calls.jsonl");
    let server = HttpServer::start(&python, &log, false);
    let tools = "      echo: { effect_class: read.external }\n";

    // No grant for the destination: refused, and the server saw nothing.
    let host = McpHost::new(
        http_config(server.port, tools),
        RegistryHandle::builtin(),
        loopback_gate(server.port, true, false),
    );
    let report = host.sync().await;
    assert!(!report.servers[0].connected);
    assert_eq!(
        report.servers[0].failure.as_deref(),
        Some("mcp.transport.egress_denied")
    );
    assert!(
        log_entries(&log).is_empty(),
        "no request reached the server"
    );

    // A grant, but the policy only allows public addresses: loopback is refused by address class.
    let host = McpHost::new(
        http_config(server.port, tools),
        RegistryHandle::builtin(),
        loopback_gate(server.port, false, true),
    );
    let report = host.sync().await;
    assert_eq!(
        report.servers[0].failure.as_deref(),
        Some("mcp.transport.egress_denied")
    );
    assert!(
        log_entries(&log).is_empty(),
        "no request reached the server"
    );

    // The gate that stdio servers are composed with allows nothing, so an HTTP server behind it is unreachable.
    let host = McpHost::new(
        http_config(server.port, tools),
        RegistryHandle::builtin(),
        Arc::new(NoGrantsGate),
    );
    assert!(!host.sync().await.servers[0].connected);
    assert!(log_entries(&log).is_empty());

    // The call path refuses too: a registered tool whose destination was revoked is `EGRESS_DENIED`, nothing sent.
    let registry = RegistryHandle::builtin();
    let live = McpHost::new(
        http_config(server.port, tools),
        registry.clone(),
        loopback_gate(server.port, true, true),
    );
    assert!(live.sync().await.servers[0].connected);
    let revoked = McpHost::new(
        http_config(server.port, tools),
        registry,
        Arc::new(NoGrantsGate),
    );
    let before = log_entries(&log).len();
    let reply = call(&revoked, "mcp.web.echo", json!({"text": "x"}), "tok").await;
    assert_eq!(
        reply.error.as_ref().expect("error")["code"],
        json!("EGRESS_DENIED"),
        "{reply:#?}"
    );
    assert_eq!(
        log_entries(&log).len(),
        before,
        "nothing was sent after the revocation"
    );
}

#[tokio::test]
async fn an_http_server_that_asks_for_sampling_in_its_event_stream_is_refused() {
    let Some(python) = python() else { return };
    let dir = scratch("mcp-http-sse");
    let log = dir.path().join("calls.jsonl");
    let server = HttpServer::start(&python, &log, true);
    let host = McpHost::new(
        http_config(
            server.port,
            "      sampling: { effect_class: read.external }\n",
        ),
        RegistryHandle::builtin(),
        loopback_gate(server.port, true, true),
    );
    assert!(host.sync().await.servers[0].connected);
    let reply = call(&host, "mcp.web.sampling", json!({}), "tok").await;
    assert_eq!(reply.status, ReplyStatus::Ok, "{reply:#?}");
    let refusal = log_entries(&log)
        .into_iter()
        .find_map(|entry| entry.get("client_response").cloned())
        .expect("the server received the client's answer to its request");
    assert_eq!(refusal["error"]["code"], json!(-32601), "{refusal}");
    assert_eq!(refusal["id"], json!("srv-sampling-1"));
}

// ---------------------------------------------------------------------------------------------
// The source says what the tests prove
// ---------------------------------------------------------------------------------------------

#[test]
fn the_stdio_transport_clears_the_environment_and_no_other_module_starts_a_process() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mcp");
    let read = |name: &str| std::fs::read_to_string(root.join(name)).expect(name);
    let stdio = read("stdio.rs");
    assert!(
        stdio.contains(".env_clear()"),
        "the child's environment is cleared before anything is set"
    );
    assert!(stdio.contains(".envs(environment)"));
    for module in ["mod.rs", "http.rs", "transport.rs"] {
        let source = read(module);
        assert!(
            !source.contains("Command::new"),
            "{module} starts no process"
        );
        assert!(
            !source.contains("tokio::process"),
            "{module} starts no process"
        );
    }
    // The worker's environment is read once, and only the names a server may inherit are kept.
    let host = read("mod.rs");
    assert_eq!(host.matches("std::env::vars()").count(), 1);
    assert!(host.contains(".filter(|(name, _)| inheritable.contains(name))"));
    // Redirects are off and the request goes to the vetted addresses.
    let http = read("http.rs");
    assert!(
        http.contains("redirect::Policy::none()")
            && http.contains(".resolve_to_addrs(")
            && http.contains(".no_proxy()")
    );
}
