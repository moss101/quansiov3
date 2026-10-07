//! The composition sandbox (ADP-009, DOMAIN.md §7.5 "Composition sandbox").
//!
//! Two kinds of test, said plainly:
//!
//! * **sandbox semantics** (most of this file) run real scripts in the real embedded interpreter against a
//!   [`Dispatcher`] that records every host call and answers as scripted. What is under test is the sandbox — what a script
//!   can reach, what a limit does, what a terminal condition ends — not the tools behind `call`, so the dispatcher is the
//!   thing a tool would be. The host-call count it records is the proof that something did or did not reach the runtime.
//! * **real boundary**: one suite drives a script against a **real headless Chrome** through the real `BrowserDriver`
//!   (and the server's `compose_dispatch.rs` drives it through the real dispatcher, PostgreSQL and a real MCP server).
//!   `BLOCKED_EXTERNAL` without a Chrome.
//!
//! Proven here: no ambient I/O and no way to make more code (every probe returns `undefined` or throws, and nothing reaches
//! the dispatcher); a deadline, a memory limit, a stack limit, an output cap and a host-call budget each end the cell and say
//! which; a terminal condition ends the cell and **no `try/catch` in the script changes it** — the dispatcher saw exactly the
//! calls before it; `state` persists across the cells of one session and nowhere else, and is cleared on reset, a limit, a
//! terminal condition and idle time; results are frozen, so a handle is never edited into another element and never
//! re-resolved by the sandbox; a re-delivered dispatch never runs a script twice.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use quansio_qworkerd::compose::{
    ComposeConfig, ComposeContext, ComposeHost, ComposeReply, HostCaller, HostResult,
};
use serde_json::{json, Value};

type Responder = Box<dyn Fn(usize, &str, &Value) -> HostResult + Send + Sync>;

/// A dispatcher that records every call it is handed and answers as the test says.
struct Dispatcher {
    calls: Mutex<Vec<(String, Value)>>,
    respond: Responder,
}

impl Dispatcher {
    fn new(
        respond: impl Fn(usize, &str, &Value) -> HostResult + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            respond: Box::new(respond),
        })
    }

    /// Answers every call with `{ "echo": args }`.
    fn echo() -> Arc<Self> {
        Self::new(|_, tool, args| HostResult::Ok(json!({ "tool": tool, "echo": args })))
    }

    fn calls(&self) -> Vec<(String, Value)> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn count(&self) -> usize {
        self.calls().len()
    }
}

#[async_trait]
impl HostCaller for Dispatcher {
    async fn call(&self, tool: &str, args: Value) -> HostResult {
        let index = {
            let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
            calls.push((tool.to_string(), args.clone()));
            calls.len()
        };
        (self.respond)(index, tool, &args)
    }
}

fn config_with(edits: &[(&str, &str)]) -> ComposeConfig {
    let mut text = include_str!("../../../config/composition.yaml").to_string();
    for (key, value) in edits {
        let prefix = format!("  {key}: ");
        let start = text
            .find(&prefix)
            .unwrap_or_else(|| panic!("no limit {key}"));
        let end = start + text[start..].find('\n').expect("line end");
        text.replace_range(start..end, &format!("{prefix}{value}"));
    }
    ComposeConfig::from_yaml(&text).expect("a valid configuration")
}

fn host() -> ComposeHost {
    ComposeHost::new(ComposeConfig::builtin())
}

static TOKENS: Mutex<u32> = Mutex::new(0);

fn token() -> String {
    let mut next = TOKENS.lock().unwrap_or_else(PoisonError::into_inner);
    *next += 1;
    format!("dsp_{:04}", *next)
}

async fn run_in(
    host: &ComposeHost,
    run_id: &str,
    args: Value,
    caller: &Arc<Dispatcher>,
) -> ComposeReply {
    let token = token();
    host.run(
        &args,
        &ComposeContext {
            run_id,
            dispatch_token: &token,
        },
        Arc::clone(caller) as Arc<dyn HostCaller>,
    )
    .await
}

async fn run(host: &ComposeHost, script: &str, caller: &Arc<Dispatcher>) -> Value {
    let reply = run_in(host, "run_a", json!({ "script": script }), caller).await;
    assert_eq!(
        reply.status,
        quansio_qworkerd::browser::tools::ReplyStatus::Ok,
        "{reply:#?}"
    );
    reply.output
}

fn code_of(output: &Value) -> &str {
    output["error"]["code"].as_str().unwrap_or("")
}

// ---------------------------------------------------------------------------------------------
// The happy path
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_script_composes_calls_and_returns_a_value_the_declaration_describes() {
    let dispatcher = Dispatcher::new(|_, _, args| {
        let sum = args["a"].as_i64().unwrap_or(0) + args["b"].as_i64().unwrap_or(0);
        HostResult::Ok(json!({ "sum": sum }))
    });
    let output = run(
        &host(),
        r#"
        const first = await call("demo.add", { a: 1, b: 2 });
        const second = call("demo.add", { a: first.sum, b: 3 });
        log("sum", second.sum, { ok: true });
        image("evd_01J8Z3K6F1N8VQ2X5W9Y0ABCDE");
        return { total: second.sum };
        "#,
        &dispatcher,
    )
    .await;
    assert_eq!(output["ok"], json!(true), "{output:#}");
    assert_eq!(output["result_json"], json!("{\"total\":6}"));
    assert_eq!(output["logs"], json!("sum 6 {\"ok\":true}\n"));
    assert_eq!(output["images"], json!(["evd_01J8Z3K6F1N8VQ2X5W9Y0ABCDE"]));
    assert_eq!(output["host_calls"], json!(2));
    assert_eq!(output["trust"], json!("UNTRUSTED_EXTERNAL"));
    assert_eq!(output["error"], Value::Null);
    assert_eq!(
        dispatcher.calls(),
        vec![
            ("demo.add".to_string(), json!({"a": 1, "b": 2})),
            ("demo.add".to_string(), json!({"a": 3, "b": 3})),
        ]
    );
    // The reply is exactly what compose.run declares.
    let declaration = quansio_tools::ToolRegistry::builtin()
        .get("compose.run")
        .expect("declared");
    assert_eq!(
        quansio_tools::first_violation(&declaration.output_schema, &output),
        None
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_script_error_is_reported_with_its_message_and_what_it_logged_before() {
    let dispatcher = Dispatcher::echo();
    let output = run(
        &host(),
        "log('before'); throw new Error('boom');",
        &dispatcher,
    )
    .await;
    assert_eq!(output["ok"], json!(false));
    assert_eq!(code_of(&output), "COMPOSE_SCRIPT_ERROR");
    assert!(output["error"]["message"]
        .as_str()
        .unwrap()
        .contains("boom"));
    assert_eq!(output["logs"], json!("before\n"));
    let syntax = run(&host(), "this is not javascript (", &dispatcher).await;
    assert_eq!(code_of(&syntax), "COMPOSE_SCRIPT_ERROR");
    assert!(
        syntax["error"]["message"]
            .as_str()
            .unwrap()
            .contains("SyntaxError"),
        "{syntax:#}"
    );
    // A script that waits for something nothing can resolve ends rather than hanging.
    let stuck = run(
        &host(),
        "await new Promise(() => {}); return 1;",
        &dispatcher,
    )
    .await;
    assert_eq!(code_of(&stuck), "COMPOSE_SCRIPT_ERROR");
    assert!(
        stuck["error"]["message"]
            .as_str()
            .unwrap()
            .contains("did not finish"),
        "{stuck:#}"
    );
    assert_eq!(dispatcher.count(), 0);
}

// ---------------------------------------------------------------------------------------------
// No ambient I/O, no way to make more code
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn nothing_that_reaches_outside_exists_in_the_interpreter() {
    let dispatcher = Dispatcher::echo();
    let output = run(
        &host(),
        r#"
        const names = [
          "fetch", "XMLHttpRequest", "WebSocket", "EventSource", "require", "module", "exports", "process", "Deno", "Bun",
          "std", "os", "setTimeout", "setInterval", "setImmediate", "queueMicrotask", "structuredClone", "navigator",
          "window", "document", "self", "global", "performance", "WebAssembly", "Proxy", "WeakRef", "FinalizationRegistry",
          "eval", "Function", "AsyncFunction", "readFile", "print", "scriptArgs", "importScripts", "hostCall", "hostLog",
          "hostImage", "__host_call", "native",
        ];
        const present = names.filter((name) => typeof globalThis[name] !== "undefined");
        let dynamicImport = "unreachable";
        try {
          await import("data:text/javascript,export default 1");
          dynamicImport = "loaded";
        } catch (error) {
          dynamicImport = "refused";
        }
        return { present, dynamicImport, dateExists: typeof Date, jsonExists: typeof JSON };
        "#,
        &dispatcher,
    )
    .await;
    assert_eq!(output["ok"], json!(true), "{output:#}");
    let result: Value = serde_json::from_str(output["result_json"].as_str().unwrap()).unwrap();
    assert_eq!(result["present"], json!([]), "these names exist: {result}");
    assert_eq!(result["dynamicImport"], json!("refused"));
    assert_eq!(
        result["dateExists"],
        json!("function"),
        "the language's own built-ins are there"
    );
    assert_eq!(dispatcher.count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ways_to_build_more_code_or_reach_the_global_object_are_closed() {
    let attempts = [
        r#"return [].constructor.constructor("return globalThis")();"#,
        r#"return (function () {}).constructor("return globalThis")();"#,
        r#"return (async function () {}).constructor("return globalThis")();"#,
        r#"return Object.getPrototypeOf(async function* () {}).constructor("return globalThis")();"#,
        r#"return Object.getPrototypeOf(function* () {}).constructor("return globalThis")();"#,
        r#"return globalThis.constructor.constructor("return this")();"#,
        r#"return new Function("return globalThis")();"#,
        r#"return Function("return globalThis")();"#,
        r#"return eval("globalThis");"#,
        r#"return (0, globalThis.eval)("globalThis");"#,
        r#"return Reflect.construct(Object.getPrototypeOf(function () {}).constructor, ["return this"])();"#,
        r#"return ({}).__proto__.constructor.constructor("return this")();"#,
        r#"return Object.getOwnPropertyDescriptor(Object.getPrototypeOf(function () {}), "constructor").value("return this")();"#,
    ];
    let dispatcher = Dispatcher::echo();
    for script in attempts {
        let output = run(&host(), script, &dispatcher).await;
        assert_eq!(output["ok"], json!(false), "{script} produced {output:#}");
        assert_eq!(code_of(&output), "COMPOSE_SCRIPT_ERROR", "{script}");
    }
    assert_eq!(dispatcher.count(), 0, "none of it reached the dispatcher");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_scripts_api_and_state_can_not_be_replaced_or_polluted() {
    let dispatcher = Dispatcher::echo();
    let output = run(
        &host(),
        r#"
        const outcomes = {};
        for (const [name, attempt] of Object.entries({
          replaceCall: () => { call = () => ({ forged: true }); },
          replaceState: () => { state = { forged: true }; },
          deleteCall: () => { delete globalThis.call; },
          redefineLog: () => { Object.defineProperty(globalThis, "log", { value: 1 }); },
        })) {
          try { attempt(); outcomes[name] = "allowed"; } catch (error) { outcomes[name] = error.name; }
        }
        return { outcomes, callIsFunction: typeof call, stateIsObject: typeof state };
        "#,
        &dispatcher,
    )
    .await;
    let result: Value = serde_json::from_str(output["result_json"].as_str().unwrap()).unwrap();
    for name in ["replaceCall", "replaceState", "deleteCall", "redefineLog"] {
        assert_eq!(
            result["outcomes"][name],
            json!("TypeError"),
            "{name}: {result}"
        );
    }
    assert_eq!(result["callIsFunction"], json!("function"));
}

// ---------------------------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_loop_is_cut_at_the_deadline_and_says_so() {
    let dispatcher = Dispatcher::echo();
    let host = host();
    for script in [
        "while (true) {}",
        "for (;;) { try { } catch (e) { } }",
        "let n = 0; while (true) { n += 1; }",
    ] {
        let started = Instant::now();
        let reply = run_in(
            &host,
            "run_a",
            json!({"script": script, "deadline_ms": 300}),
            &dispatcher,
        )
        .await;
        let elapsed = started.elapsed();
        assert_eq!(
            code_of(&reply.output),
            "COMPOSE_LIMIT_EXCEEDED",
            "{script}: {:#}",
            reply.output
        );
        assert_eq!(reply.output["error"]["limit"], json!("deadline"));
        assert!(elapsed < Duration::from_secs(5), "{script} ran {elapsed:?}");
        assert!(
            elapsed >= Duration::from_millis(250),
            "{script} ended early: {elapsed:?}"
        );
    }
    // The ceiling holds: asking for a day gets the configured maximum, not a day.
    let reply = run_in(
        &host,
        "run_a",
        json!({"script": "return 1;", "deadline_ms": 120000}),
        &dispatcher,
    )
    .await;
    assert_eq!(reply.output["ok"], json!(true));
    let refused = run_in(
        &host,
        "run_a",
        json!({"script": "return 1;", "deadline_ms": 86_400_000}),
        &dispatcher,
    )
    .await;
    assert_eq!(
        refused.error.as_ref().expect("error")["code"],
        json!("VALIDATION_SCHEMA")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_memory_bomb_ends_the_cell_and_not_the_worker() {
    let dispatcher = Dispatcher::echo();
    let host = ComposeHost::new(config_with(&[("memory_bytes", "16777216")]));
    for script in [
        "const keep = []; while (true) { keep.push(new Array(100000).fill(1)); }",
        "let s = 'x'; while (true) { s += s; }",
        "const huge = new Array(2 ** 32 - 1).fill(0); return huge.length;",
        "return 'x'.repeat(2 ** 30).length;",
        "const m = new Map(); let i = 0; while (true) { m.set(i, new Array(1000)); i += 1; }",
        "const buffers = []; while (true) { buffers.push(new Uint8Array(1_000_000)); }",
    ] {
        let started = Instant::now();
        let reply = run_in(
            &host,
            "run_a",
            json!({"script": script, "deadline_ms": 20000}),
            &dispatcher,
        )
        .await;
        assert_eq!(
            code_of(&reply.output),
            "COMPOSE_LIMIT_EXCEEDED",
            "{script}: {:#}",
            reply.output
        );
        assert!(
            matches!(
                reply.output["error"]["limit"].as_str(),
                Some("memory" | "deadline")
            ),
            "{script}: {:#}",
            reply.output
        );
        assert!(started.elapsed() < Duration::from_secs(20), "{script}");
    }
    // The next cell runs normally: the worker is fine.
    let after = run_in(
        &host,
        "run_a",
        json!({"script": "return 'still here';"}),
        &dispatcher,
    )
    .await;
    assert_eq!(after.output["ok"], json!(true), "{:#}", after.output);
    assert_eq!(dispatcher.count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn unbounded_recursion_ends_the_cell_at_the_stack_limit() {
    let dispatcher = Dispatcher::echo();
    let host = host();
    for script in [
        "function f(n) { return f(n + 1) + 1; } return f(0);",
        "const o = {}; o.o = o; return JSON.stringify(o);",
        "let a = []; for (let i = 0; i < 100000; i += 1) { a = [a]; } return JSON.stringify(a);",
    ] {
        let reply = run_in(&host, "run_a", json!({"script": script}), &dispatcher).await;
        assert_eq!(
            reply.output["ok"],
            json!(false),
            "{script}: {:#}",
            reply.output
        );
        assert!(
            matches!(
                code_of(&reply.output),
                "COMPOSE_LIMIT_EXCEEDED" | "COMPOSE_SCRIPT_ERROR"
            ),
            "{script}: {:#}",
            reply.output
        );
    }
    let recursion = run_in(
        &host,
        "run_a",
        json!({"script": "function f(n) { return f(n + 1) + 1; } return f(0);"}),
        &dispatcher,
    )
    .await;
    assert_eq!(
        recursion.output["error"]["limit"],
        json!("stack"),
        "{:#}",
        recursion.output
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_huge_output_is_cut_at_the_cap_and_images_at_three() {
    let dispatcher = Dispatcher::echo();
    let host = host();
    let flood = run(
        &host,
        r#"
        for (let i = 0; i < 2000; i += 1) { log("line " + i + " " + "x".repeat(100)); }
        for (let i = 0; i < 10; i += 1) { image("evd_" + i); }
        image("z".repeat(100000));
        return "y".repeat(1000000);
        "#,
        &dispatcher,
    )
    .await;
    assert_eq!(flood["ok"], json!(true), "{flood:#}");
    assert!(
        flood["logs"].as_str().unwrap().len() <= 38 * 1024,
        "logs are {} bytes",
        flood["logs"].as_str().unwrap().len()
    );
    assert_eq!(flood["truncated"], json!(true));
    assert_eq!(
        flood["images"],
        json!(["evd_0", "evd_1", "evd_2"]),
        "three images and no pixels"
    );
    assert!(flood["result_json"].as_str().unwrap().len() <= 38 * 1024);
    // Arguments and results are bounded too: a call with an enormous argument is refused before the dispatcher sees it.
    let big = run(&host, "try { call('demo.x', { blob: 'a'.repeat(400000) }); } catch (e) { return e.code; } return 'sent';", &dispatcher).await;
    assert_eq!(
        big["result_json"],
        json!("\"COMPOSE_LIMIT_EXCEEDED\""),
        "{big:#}"
    );
    assert_eq!(dispatcher.count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_script_that_floods_host_calls_is_stopped_at_its_budget_even_inside_try_catch() {
    let dispatcher = Dispatcher::echo();
    let host = host();
    let reply = run_in(
        &host,
        "run_a",
        json!({"script": "for (let i = 0; i < 1000; i += 1) { try { call('demo.noop', { i }); } catch (e) { } } return 'finished';"}),
        &dispatcher,
    )
    .await;
    assert_eq!(
        code_of(&reply.output),
        "COMPOSE_TERMINATED",
        "{:#}",
        reply.output
    );
    assert_eq!(
        reply.output["error"]["condition"],
        json!("host_call_budget")
    );
    assert_eq!(
        reply.output["result_json"],
        Value::Null,
        "the script's own return is discarded"
    );
    assert_eq!(
        dispatcher.count(),
        64,
        "exactly the budget reached the dispatcher"
    );

    // A caller can lower the budget and can not raise it past the ceiling.
    let low = Dispatcher::echo();
    let reply = run_in(&host, "run_a", json!({"script": "for (let i = 0; i < 10; i += 1) call('demo.noop', {});", "max_host_calls": 3}), &low).await;
    assert_eq!(
        reply.output["error"]["condition"],
        json!("host_call_budget")
    );
    assert_eq!(low.count(), 3);
    let high = Dispatcher::echo();
    let reply = run_in(&host, "run_a", json!({"script": "for (let i = 0; i < 600; i += 1) call('demo.noop', {});", "max_host_calls": 512}), &high).await;
    assert_eq!(
        reply.output["error"]["condition"],
        json!("host_call_budget")
    );
    assert_eq!(high.count(), 512, "512 is the ceiling");
    let over = run_in(
        &host,
        "run_a",
        json!({"script": "return 1;", "max_host_calls": 100000}),
        &high,
    )
    .await;
    assert_eq!(
        over.error.as_ref().expect("error")["code"],
        json!("VALIDATION_SCHEMA")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_script_size_and_the_arguments_are_checked_before_anything_runs() {
    let dispatcher = Dispatcher::echo();
    let host = ComposeHost::new(config_with(&[("max_script_bytes", "1024")]));
    let reply = run_in(
        &host,
        "run_a",
        json!({"script": format!("return '{}';", "a".repeat(2000))}),
        &dispatcher,
    )
    .await;
    assert_eq!(reply.output["error"]["limit"], json!("script_size"));
    for bad in [
        json!({}),
        json!({"script": ""}),
        json!({"script": "return 1;", "unknown": true}),
        json!({"script": 7}),
    ] {
        let reply = run_in(&host, "run_a", bad.clone(), &dispatcher).await;
        assert_eq!(
            reply.error.as_ref().expect("error")["code"],
            json!("VALIDATION_SCHEMA"),
            "{bad}"
        );
    }
    let off = ComposeHost::new({
        let mut config = ComposeConfig::builtin();
        config.enabled = false;
        config
    });
    let reply = run_in(&off, "run_a", json!({"script": "return 1;"}), &dispatcher).await;
    assert_eq!(
        reply.error.as_ref().expect("error")["code"],
        json!("COMPOSE_LIMIT_EXCEEDED")
    );
    assert_eq!(dispatcher.count(), 0);
}

// ---------------------------------------------------------------------------------------------
// Terminal conditions
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_condition_ends_the_cell_and_no_catch_changes_it() {
    for code in [
        "COMPUTER_TAKEOVER_ACTIVE",
        "BROWSER_INPUT_FENCED",
        "COMPUTER_PERMISSION_REVOKED",
        "COMPUTER_STOPPED_BY_USER",
        "BUDGET_EXHAUSTED",
    ] {
        let dispatcher = Dispatcher::new(move |index, _, _| {
            if index == 2 {
                HostResult::Failed {
                    code: code.to_string(),
                    message: "ended".to_string(),
                    retryable: true,
                    details: None,
                }
            } else {
                HostResult::Ok(json!({"n": index}))
            }
        });
        let output = run(
            &host(),
            r#"
            log("first", call("demo.a", {}).n);
            try { call("demo.b", {}); log("not reached"); } catch (error) { log("caught", error.message); }
            try { call("demo.c", {}); log("also not reached"); } catch (error) { log("caught again"); }
            try { call("demo.d", {}); } finally { log("finally runs but achieves nothing"); }
            return "the script thinks it finished";
            "#,
            &dispatcher,
        )
        .await;
        assert_eq!(output["ok"], json!(false), "{code}: {output:#}");
        assert_eq!(code_of(&output), "COMPOSE_TERMINATED", "{code}");
        assert_eq!(output["error"]["condition"], json!("host_error"));
        assert_eq!(output["error"]["host_code"], json!(code));
        assert_eq!(
            output["result_json"],
            Value::Null,
            "the script's return is discarded: {code}"
        );
        assert_eq!(
            dispatcher.count(),
            2,
            "{code}: the calls after the terminal one never reached the runtime"
        );
        assert!(
            output["logs"].as_str().unwrap().starts_with("first 1\n"),
            "what was logged before stays: {output:#}"
        );
        assert!(!output["logs"].as_str().unwrap().contains("not reached"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pending_approval_and_an_unknown_outcome_end_the_cell_too() {
    let parked = Dispatcher::new(|index, _, _| {
        if index == 2 {
            HostResult::Parked {
                detail: "apr_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
            }
        } else {
            HostResult::Ok(json!({}))
        }
    });
    let output = run(&host(), "call('x.a', {}); try { call('x.b', {}); } catch (e) {} try { call('x.c', {}); } catch (e) {} return 'done';", &parked).await;
    assert_eq!(
        output["error"]["condition"],
        json!("approval_required"),
        "{output:#}"
    );
    assert_eq!(parked.count(), 2);

    let unknown = Dispatcher::new(|index, _, _| {
        if index == 1 {
            HostResult::Unknown {
                detail: "no answer".to_string(),
            }
        } else {
            HostResult::Ok(json!({}))
        }
    });
    let output = run(&host(), "try { call('x.a', {}); } catch (e) {} try { call('x.again', {}); } catch (e) {} return 'retried';", &unknown).await;
    assert_eq!(
        output["error"]["condition"],
        json!("outcome_unknown"),
        "{output:#}"
    );
    assert_eq!(
        unknown.count(),
        1,
        "a script does not go on to retry past an unknown outcome"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn any_other_error_is_the_scripts_to_handle() {
    let dispatcher = Dispatcher::new(|index, _, _| {
        if index == 1 {
            HostResult::Failed {
                code: "BROWSER_STALE_PAGE".to_string(),
                message: "the page changed".to_string(),
                retryable: true,
                details: Some(json!({"expected": "r1", "actual": "r2"})),
            }
        } else {
            HostResult::Ok(json!({"ok": index}))
        }
    });
    let output = run(
        &host(),
        r#"
        let seen;
        try { call("browser.click", {}); } catch (error) {
          seen = { isHostError: error instanceof HostError, code: error.code, retryable: error.retryable, actual: error.details.actual, frozen: Object.isFrozen(error) };
        }
        const next = call("browser.observe", {});
        return { seen, next };
        "#,
        &dispatcher,
    )
    .await;
    assert_eq!(output["ok"], json!(true), "{output:#}");
    let result: Value = serde_json::from_str(output["result_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        result["seen"],
        json!({"isHostError": true, "code": "BROWSER_STALE_PAGE", "retryable": true, "actual": "r2", "frozen": true})
    );
    assert_eq!(
        dispatcher.count(),
        2,
        "a stale page is an ordinary error, so the script went on"
    );
}

// ---------------------------------------------------------------------------------------------
// Handles are frozen and never re-resolved
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_element_handle_is_frozen_and_goes_back_exactly_as_it_came() {
    let dispatcher = Dispatcher::new(|_, tool, _| {
        if tool == "browser.observe" {
            HostResult::Ok(
                json!({"snapshot_id": "snp_1", "document_revision": "rev_1", "elements": [{"ref": "e1", "name": "Go"}, {"ref": "e2", "name": "Other"}]}),
            )
        } else {
            HostResult::Ok(json!({"clicked": true}))
        }
    });
    let output = run(
        &host(),
        r#"
        const view = call("browser.observe", {});
        const go = view.elements[0];
        const attempts = [];
        for (const edit of [() => { go.ref = "e2"; }, () => { view.elements.push({ ref: "e9" }); }, () => { view.snapshot_id = "snp_2"; }, () => { delete go.name; }]) {
          try { edit(); attempts.push("allowed"); } catch (error) { attempts.push(error.name); }
        }
        call("browser.click", { snapshot_id: view.snapshot_id, ref: go.ref, expected_document_revision: view.document_revision });
        return { attempts, ref: go.ref, frozen: Object.isFrozen(view) && Object.isFrozen(view.elements) && Object.isFrozen(go) };
        "#,
        &dispatcher,
    )
    .await;
    assert_eq!(output["ok"], json!(true), "{output:#}");
    let result: Value = serde_json::from_str(output["result_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        result["attempts"],
        json!(["TypeError", "TypeError", "TypeError", "TypeError"])
    );
    assert_eq!(result["ref"], json!("e1"));
    assert_eq!(result["frozen"], json!(true));
    let click = &dispatcher.calls()[1];
    assert_eq!(
        click.1,
        json!({"snapshot_id": "snp_1", "ref": "e1", "expected_document_revision": "rev_1"}),
        "the dispatcher saw the handle as the page gave it: the sandbox re-resolved nothing"
    );
}

// ---------------------------------------------------------------------------------------------
// Sessions and state
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn state_persists_across_the_cells_of_one_session_and_nowhere_else() {
    let dispatcher = Dispatcher::echo();
    let host = host();
    let first = run_in(
        &host,
        "run_a",
        json!({"script": "state.count = 1; state.items = ['a']; return state.count;"}),
        &dispatcher,
    )
    .await;
    assert_eq!(first.output["result_json"], json!("1"));
    assert!(first.output["state_bytes"].as_u64().unwrap() > 0);
    let second = run_in(
        &host,
        "run_a",
        json!({"script": "state.count += 1; state.items.push('b'); return state;"}),
        &dispatcher,
    )
    .await;
    assert_eq!(
        second.output["result_json"],
        json!("{\"count\":2,\"items\":[\"a\",\"b\"]}"),
        "{:#}",
        second.output
    );
    // Another session of the same run, and the same session name in another run, see nothing.
    let other_session = run_in(
        &host,
        "run_a",
        json!({"script": "return Object.keys(state);", "session_id": "other"}),
        &dispatcher,
    )
    .await;
    assert_eq!(other_session.output["result_json"], json!("[]"));
    let other_run = run_in(
        &host,
        "run_b",
        json!({"script": "return Object.keys(state);"}),
        &dispatcher,
    )
    .await;
    assert_eq!(other_run.output["result_json"], json!("[]"));
    // Nothing but data crosses: a function put in state does not.
    let functions = run_in(
        &host,
        "run_c",
        json!({"script": "state.f = () => 1; state.n = 1; return 0;"}),
        &dispatcher,
    )
    .await;
    assert_eq!(functions.output["ok"], json!(true));
    let again = run_in(
        &host,
        "run_c",
        json!({"script": "return { n: state.n, f: typeof state.f };"}),
        &dispatcher,
    )
    .await;
    assert_eq!(
        again.output["result_json"],
        json!("{\"n\":1,\"f\":\"undefined\"}")
    );
    // A reset clears it.
    let reset = run_in(
        &host,
        "run_a",
        json!({"script": "return Object.keys(state);", "reset": true}),
        &dispatcher,
    )
    .await;
    assert_eq!(reset.output["result_json"], json!("[]"));
    assert_eq!(dispatcher.count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn state_is_cleared_by_a_limit_a_terminal_condition_and_idle_time_but_not_by_a_script_error()
{
    let dispatcher = Dispatcher::new(|_, _, _| HostResult::Failed {
        code: "COMPUTER_STOPPED_BY_USER".to_string(),
        message: "stopped".to_string(),
        retryable: false,
        details: None,
    });
    let host = ComposeHost::new(config_with(&[("idle_timeout_ms", "1000")]));
    let set = || json!({"script": "state.kept = 'yes'; return 1;"});
    let read = || json!({"script": "return state.kept ?? null;"});

    // A script error: the mutations of the failed cell are discarded and the state before it stays.
    run_in(&host, "run_a", set(), &dispatcher).await;
    let failed = run_in(
        &host,
        "run_a",
        json!({"script": "state.kept = 'changed'; throw new Error('x');"}),
        &dispatcher,
    )
    .await;
    assert_eq!(code_of(&failed.output), "COMPOSE_SCRIPT_ERROR");
    assert_eq!(
        run_in(&host, "run_a", read(), &dispatcher).await.output["result_json"],
        json!("\"yes\"")
    );

    // A terminal condition clears it.
    let ended = run_in(
        &host,
        "run_a",
        json!({"script": "call('x.a', {});"}),
        &dispatcher,
    )
    .await;
    assert_eq!(code_of(&ended.output), "COMPOSE_TERMINATED");
    assert_eq!(
        run_in(&host, "run_a", read(), &dispatcher).await.output["result_json"],
        json!("null")
    );

    // A limit clears it.
    run_in(&host, "run_a", set(), &dispatcher).await;
    let limited = run_in(
        &host,
        "run_a",
        json!({"script": "while (true) {}", "deadline_ms": 200}),
        &dispatcher,
    )
    .await;
    assert_eq!(code_of(&limited.output), "COMPOSE_LIMIT_EXCEEDED");
    assert_eq!(
        run_in(&host, "run_a", read(), &dispatcher).await.output["result_json"],
        json!("null")
    );

    // Idle time clears it (checked when any session is next used: there is no timer).
    run_in(&host, "run_a", set(), &dispatcher).await;
    assert_eq!(host.session_count(), 1);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        run_in(&host, "run_a", read(), &dispatcher).await.output["result_json"],
        json!("null")
    );

    // State that does not fit is refused and not stored.
    let host = ComposeHost::new(config_with(&[("state_max_bytes", "2048")]));
    let too_big = run_in(
        &host,
        "run_a",
        json!({"script": "state.blob = 'a'.repeat(5000); return 1;"}),
        &dispatcher,
    )
    .await;
    assert_eq!(
        too_big.output["error"]["limit"],
        json!("state"),
        "{:#}",
        too_big.output
    );
    assert_eq!(
        run_in(
            &host,
            "run_a",
            json!({"script": "return Object.keys(state);"}),
            &dispatcher
        )
        .await
        .output["result_json"],
        json!("[]")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn only_so_many_sessions_are_held_and_the_least_recently_used_makes_room() {
    let dispatcher = Dispatcher::echo();
    let host = ComposeHost::new(config_with(&[("max_sessions", "2")]));
    for session in ["s1", "s2", "s3"] {
        run_in(
            &host,
            "run_a",
            json!({"script": "state.x = 1; return 0;", "session_id": session}),
            &dispatcher,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(host.session_count(), 2);
    let oldest = run_in(
        &host,
        "run_a",
        json!({"script": "return Object.keys(state);", "session_id": "s1"}),
        &dispatcher,
    )
    .await;
    assert_eq!(
        oldest.output["result_json"],
        json!("[]"),
        "the oldest session was dropped"
    );
    let newest = run_in(
        &host,
        "run_a",
        json!({"script": "return Object.keys(state);", "session_id": "s3"}),
        &dispatcher,
    )
    .await;
    assert_eq!(newest.output["result_json"], json!("[\"x\"]"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redelivered_dispatch_is_answered_from_the_journal_and_never_runs_the_script_twice() {
    let dispatcher = Dispatcher::echo();
    let host = host();
    let args = json!({"script": "call('demo.effect', { n: 1 }); return 'once';"});
    let context = ComposeContext {
        run_id: "run_a",
        dispatch_token: "dsp_fixed",
    };
    let first = host
        .run(
            &args,
            &context,
            Arc::clone(&dispatcher) as Arc<dyn HostCaller>,
        )
        .await;
    assert_eq!(first.output["result_json"], json!("\"once\""));
    let again = host
        .run(
            &args,
            &context,
            Arc::clone(&dispatcher) as Arc<dyn HostCaller>,
        )
        .await;
    assert!(again.replayed);
    assert_eq!(again.output, first.output);
    assert_eq!(dispatcher.count(), 1, "the script's effect happened once");
}

// ---------------------------------------------------------------------------------------------
// Real boundary: a script against a real headless Chrome
// ---------------------------------------------------------------------------------------------

/// The host calls of a script, dispatched to a real [`BrowserDriver`] over a real Chrome.
struct ChromeCaller {
    driver: quansio_qworkerd::browser::BrowserDriver,
    next: Mutex<u32>,
    log: Mutex<Vec<String>>,
}

#[async_trait]
impl HostCaller for ChromeCaller {
    async fn call(&self, tool: &str, args: Value) -> HostResult {
        let n = {
            let mut next = self.next.lock().unwrap_or_else(PoisonError::into_inner);
            *next += 1;
            *next
        };
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(tool.to_string());
        let reply = common::run(&self.driver, &common::call(n), tool, args).await;
        match reply["status"].as_str() {
            Some("ok") => HostResult::Ok(reply["output"].clone()),
            Some("in_doubt") => HostResult::Unknown {
                detail: reply["output"].to_string(),
            },
            _ => HostResult::Failed {
                code: reply["error"]["code"]
                    .as_str()
                    .unwrap_or("BROWSER_PAGE_FAILURE")
                    .to_string(),
                message: reply["error"]["message"].as_str().unwrap_or("").to_string(),
                retryable: reply["error"]["retryable"].as_bool().unwrap_or(false),
                details: reply["error"].get("details").cloned(),
            },
        }
    }
}

const PAGE: &str = r#"<html><head><title>Compose</title></head><body>
<h1>Compose page</h1>
<button id="go" onclick="fetch('/hit'); document.getElementById('log').textContent += 'x';">Go</button>
<button id="other">Other</button>
<div id="log"></div>
</body></html>"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_script_drives_a_real_browser_and_a_spent_handle_is_stale_not_re_resolved() {
    let Some(browser) = common::Browser::launch() else {
        common::blocked("no Chrome on this host");
        return;
    };
    let site = common::Site::start().await;
    site.html("/", PAGE);
    site.route("/hit", common::Route::Count);
    let dir = tempfile::tempdir().expect("session dir");
    let caller = Arc::new(ChromeCaller {
        driver: common::driver(&browser, dir.path(), |_| {}),
        next: Mutex::new(0),
        log: Mutex::new(Vec::new()),
    });
    let host = host();
    let script = format!(
        r#"
        const page = call("browser.navigate", {{ url: "{url}" }});
        const view = call("browser.observe", {{ tab_id: page.tab_id }});
        const go = view.elements.find((element) => element.name === "Go");
        log("observed", view.elements.length, "elements");
        const clicked = call("browser.click", {{
          tab_id: page.tab_id, expected_document_revision: view.document_revision,
          snapshot_id: view.snapshot_id, ref: go.ref,
        }});
        let again;
        try {{
          call("browser.click", {{
            tab_id: page.tab_id, expected_document_revision: view.document_revision,
            snapshot_id: view.snapshot_id, ref: go.ref,
          }});
          again = "clicked a second time";
        }} catch (error) {{
          again = error.code;
        }}
        return {{ clicked: clicked.clicked, again, handleStillFrozen: Object.isFrozen(go) }};
        "#,
        url = site.url("/")
    );
    let args = json!({"script": script});
    let token = token();
    let reply = host
        .run(
            &args,
            &ComposeContext {
                run_id: "run_chrome",
                dispatch_token: &token,
            },
            Arc::clone(&caller) as Arc<dyn HostCaller>,
        )
        .await;
    let output = reply.output;
    assert_eq!(output["ok"], json!(true), "{output:#}");
    let result: Value = serde_json::from_str(output["result_json"].as_str().unwrap()).unwrap();
    assert_eq!(result["clicked"], json!(true));
    assert_eq!(
        result["again"],
        json!("BROWSER_STALE_PAGE"),
        "the snapshot was spent by the first click, so the second is refused by the driver, which the sandbox did not work around"
    );
    assert_eq!(result["handleStillFrozen"], json!(true));
    assert!(
        common::eventually(Duration::from_secs(3), || async { site.hits("/hit") == 1 }).await,
        "the page was clicked exactly once: {} hits",
        site.hits("/hit")
    );
    assert_eq!(site.hits("/hit"), 1);
    assert_eq!(output["host_calls"], json!(4));
    assert!(output["logs"].as_str().unwrap().starts_with("observed "));
    let seen = caller
        .log
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(
        seen,
        vec![
            "browser.navigate",
            "browser.observe",
            "browser.click",
            "browser.click"
        ]
    );
    // The page's own text came back as data, labelled by the driver, and the cell's output carries the label too.
    assert_eq!(output["trust"], json!("UNTRUSTED_EXTERNAL"));
    let _ = BTreeMap::<u8, u8>::new();
}
