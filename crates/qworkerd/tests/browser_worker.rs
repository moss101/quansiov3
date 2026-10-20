//! `browser.*` envelopes on a browser the worker launches itself (EXEC-009 composition over ADP-001).
//!
//! Everything is real: the worker's own `WorkerHost` fencing each envelope, the executor, a Google Chrome the
//! runtime starts, and a local HTTP site whose hit counters are how many requests the page really made — so
//! "nothing reached the page" is read off the server, not assumed.
//!
//! * **the guard on where the browser may go**: by default the machine's own addresses are refused before Chrome
//!   is asked; the operator's opt-in lifts it;
//! * **it runs**: navigate, observe and click through a real page, and the site sees exactly one click;
//! * **the approved tier reaches the driver**: a click that would settle above it is refused before any input,
//!   and the same click under a higher ceiling goes through;
//! * **at most once**: a re-delivered envelope is answered from the worker's record and the page is not clicked
//!   again;
//! * **a screenshot's pixels are kept for the channel**: the reply stays small and names the bytes; the PNG itself is held by
//!   the worker's evidence sink under the reference the result carries, and is handed over once;
//! * **fail closed**: a worker without a browser refuses; a session id that is not a session id never becomes a
//!   directory; one browser per session.
//!
//! Needs a Chrome on this host; without one the suite reports `BLOCKED_EXTERNAL` and does not run.

mod common;

use std::sync::atomic::{AtomicU64, Ordering};

use common::{blocked, chrome_path, ref_named, Route, Site};
use quansio_machine::gateway::{ActionEnvelope, DispatchRequest, LeaseClaim};
use quansio_qworkerd::browser_runtime::BrowserRuntime;
use quansio_qworkerd::daemon::{InlineEvidence, UnattachedEvidence};
use quansio_qworkerd::executor::LocalToolExecutor;
use quansio_qworkerd::host::EvidenceSink;
use quansio_qworkerd::host::{HostOutcome, WorkerHost};
use quansio_qworkerd::tools::{RootScope, Sandbox};
use serde_json::{json, Value};

const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9YBRW001";
const LEASE: &str = "lse_01J8Z3K6F1N8VQ2X5W9YBRW001";
const SESSION: &str = "bsn_01J8Z3K6F1N8VQ2X5W9YBRW001";
static SEQUENCE: AtomicU64 = AtomicU64::new(1);

const PAGE: &str = r#"<html><head><title>Worker</title></head><body>
<button id="count" onclick="var x = new XMLHttpRequest(); x.open('GET', '/hit', false); x.send();">Count</button>
<button id="buy" onclick="var x = new XMLHttpRequest(); x.open('GET', '/buy', false); x.send();">Buy now</button>
</body></html>"#;

struct World {
    host: WorkerHost<LocalToolExecutor, UnattachedEvidence>,
    site: Site,
    _root: tempfile::TempDir,
    _state: tempfile::TempDir,
}

fn lease() -> LeaseClaim {
    LeaseClaim {
        lease_id: LEASE.to_string(),
        target_id: TARGET.to_string(),
        generation: 1,
        held: true,
        expires_at: "2999-01-01T00:00:00Z".to_string(),
    }
}

async fn world(allow_private: bool) -> Option<World> {
    let Some(chrome) = chrome_path() else {
        blocked("no Chrome on this host");
        return None;
    };
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let sandbox = Sandbox::empty()
        .authorize(RootScope::Workspace, root.path())
        .unwrap();
    let executor = LocalToolExecutor::new(sandbox, "tnt_test").with_browser(
        BrowserRuntime::new(chrome, state.path(), "tnt_test").allow_private_network(allow_private),
    );
    let site = Site::start().await;
    site.html("/", PAGE);
    site.route("/hit", Route::Count);
    site.route("/buy", Route::Count);
    Some(World {
        host: WorkerHost::new(executor, UnattachedEvidence),
        site,
        _root: root,
        _state: state,
    })
}

fn envelope(tool: &str, args: &Value, tier: Option<u8>) -> ActionEnvelope {
    let n = SEQUENCE.fetch_add(1, Ordering::SeqCst);
    let (effect, token) = (format!("eff_{n:026}"), format!("dsp_{n:026}"));
    let envelope = ActionEnvelope::dispatch(&DispatchRequest {
        effect_id: &effect,
        dispatch_token: &token,
        target_id: TARGET,
        lease_id: LEASE,
        generation: 1,
        run_id: "run_01J8Z3K6F1N8VQ2X5W9YBRW001",
        step_id: "stp_01J8Z3K6F1N8VQ2X5W9YBRW001",
        tool,
        tool_version: 1,
        args_json: &args.to_string(),
        max_output_bytes: 1 << 20,
        deadline_at: "2999-01-01T00:00:00Z",
    })
    .unwrap();
    match tier {
        Some(tier) => envelope.with_approved_tier(tier),
        None => envelope,
    }
}

/// The reply document the worker returned for one envelope.
async fn deliver(world: &mut World, envelope: &ActionEnvelope) -> (Value, bool) {
    let recorded = world
        .host
        .receive(envelope, &lease(), "2026-10-17T00:00:00Z")
        .await
        .expect("the envelope is valid");
    match recorded.outcome {
        HostOutcome::Completed { output, .. } => (
            serde_json::from_slice(&output).expect("a document"),
            recorded.replayed,
        ),
        other => panic!("{other:?}"),
    }
}

async fn call(world: &mut World, tool: &str, args: Value, tier: Option<u8>) -> Value {
    deliver(world, &envelope(tool, &args, tier)).await.0
}

/// Navigate to the fixture and observe it: the tab, its revision and the observation.
async fn open(world: &mut World, tier: Option<u8>) -> (String, Value) {
    let url = world.site.url("/");
    let navigated = call(
        world,
        "browser.navigate",
        json!({"session_id": SESSION, "url": url}),
        tier,
    )
    .await;
    assert_eq!(navigated["status"], "ok", "{navigated}");
    let tab = navigated["output"]["tab_id"].as_str().unwrap().to_string();
    let observed = call(
        world,
        "browser.observe",
        json!({"session_id": SESSION, "tab_id": tab}),
        tier,
    )
    .await;
    assert_eq!(observed["status"], "ok", "{observed}");
    (tab, observed["output"].clone())
}

fn click(tab: &str, observation: &Value, name: &str) -> Value {
    json!({
        "session_id": SESSION,
        "tab_id": tab,
        "expected_document_revision": observation["document_revision"],
        "snapshot_id": observation["snapshot_id"],
        "ref": ref_named(observation, name),
    })
}

#[tokio::test]
async fn by_default_the_machine_s_own_addresses_are_refused_before_chrome_is_asked() {
    let Some(mut world) = world(false).await else {
        return;
    };
    let url = world.site.url("/");
    let reply = call(
        &mut world,
        "browser.navigate",
        json!({"session_id": SESSION, "url": url}),
        None,
    )
    .await;
    assert_eq!(reply["status"], "error", "{reply}");
    assert_eq!(reply["error"]["code"], "BROWSER_NAVIGATION_DENIED");
    assert_eq!(
        world.site.hits("/"),
        0,
        "the site was never asked for the page"
    );
}

#[tokio::test]
async fn a_page_is_navigated_observed_and_clicked_once_and_a_redelivery_does_not_click_again() {
    let Some(mut world) = world(true).await else {
        return;
    };
    let (tab, observation) = open(&mut world, Some(3)).await;
    assert!(observation["text"].is_string() || observation["elements"].is_array());

    let clicking = envelope(
        "browser.click",
        &click(&tab, &observation, "Count"),
        Some(3),
    );
    let (reply, replayed) = deliver(&mut world, &clicking).await;
    assert_eq!(
        (reply["status"].as_str(), replayed),
        (Some("ok"), false),
        "{reply}"
    );
    assert_eq!(
        world.site.hits("/hit"),
        1,
        "the page really made one request"
    );

    let (again, replayed) = deliver(&mut world, &clicking).await;
    assert!(replayed, "a re-delivered envelope is a replay");
    assert_eq!(again["status"], "ok");
    assert_eq!(
        world.site.hits("/hit"),
        1,
        "the click did not happen a second time"
    );
}

#[tokio::test]
async fn a_click_above_the_approved_tier_is_refused_before_any_input_and_goes_through_under_a_higher_one(
) {
    let Some(mut world) = world(true).await else {
        return;
    };
    let (tab, observation) = open(&mut world, Some(2)).await;
    let refused = call(
        &mut world,
        "browser.click",
        click(&tab, &observation, "Buy now"),
        Some(2),
    )
    .await;
    assert_eq!(refused["status"], "error", "{refused}");
    assert_eq!(refused["error"]["code"], "BROWSER_EFFECT_TIER_EXCEEDED");
    assert_eq!(world.site.hits("/buy"), 0, "no input reached the page");

    // The refusal changed nothing about the page, so the same snapshot is still good under a higher ceiling.
    let allowed = call(
        &mut world,
        "browser.click",
        click(&tab, &observation, "Buy now"),
        Some(4),
    )
    .await;
    assert_eq!(allowed["status"], "ok", "{allowed}");
    assert_eq!(world.site.hits("/buy"), 1);
}

#[tokio::test]
async fn a_worker_without_a_browser_refuses_and_a_bad_session_id_never_becomes_a_directory() {
    // No browser configured.
    let root = tempfile::tempdir().unwrap();
    let sandbox = Sandbox::empty()
        .authorize(RootScope::Workspace, root.path())
        .unwrap();
    let bare = WorkerHost::new(
        LocalToolExecutor::new(sandbox, "tnt_test"),
        UnattachedEvidence,
    );
    let recorded = bare
        .receive(
            &envelope("browser.observe", &json!({"session_id": SESSION}), None),
            &lease(),
            "2026-10-17T00:00:00Z",
        )
        .await
        .unwrap();
    match recorded.outcome {
        HostOutcome::Failed { failure, .. } => assert!(
            failure.to_string().contains("BROWSER_UNAVAILABLE"),
            "{failure}"
        ),
        other => panic!("{other:?}"),
    }

    let Some(mut world) = world(true).await else {
        return;
    };
    for bad in ["../escape", "bsn_../../x", "/etc", "not-a-session"] {
        let reply = call(
            &mut world,
            "browser.observe",
            json!({ "session_id": bad }),
            None,
        )
        .await;
        assert_eq!(reply["status"], "error", "{bad}: {reply}");
    }
    let state_has_escape = std::fs::read_dir(world._state.path().join("browser"))
        .is_ok_and(|entries| entries.count() > 0);
    assert!(
        !state_has_escape,
        "no directory was created for a refused session id"
    );
}

#[tokio::test]
async fn a_screenshot_s_pixels_are_held_for_the_channel_and_handed_over_once() {
    let Some(chrome) = chrome_path() else {
        blocked("no Chrome on this host");
        return;
    };
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let sandbox = Sandbox::empty()
        .authorize(RootScope::Workspace, root.path())
        .unwrap();
    let executor = LocalToolExecutor::new(sandbox, "tnt_test").with_browser(
        BrowserRuntime::new(chrome, state.path(), "tnt_test").allow_private_network(true),
    );
    let host = WorkerHost::new(executor, InlineEvidence::default());
    let site = Site::start().await;
    site.html("/", PAGE);
    let navigated = envelope(
        "browser.navigate",
        &json!({"session_id": SESSION, "url": site.url("/")}),
        Some(3),
    );
    let first = host
        .receive(&navigated, &lease(), "2026-10-17T00:00:00Z")
        .await
        .unwrap();
    let HostOutcome::Completed { output, .. } = first.outcome else {
        panic!("navigate did not complete");
    };
    let tab = serde_json::from_slice::<Value>(&output).unwrap()["output"]["tab_id"]
        .as_str()
        .unwrap()
        .to_string();

    let shot = envelope(
        "browser.screenshot",
        &json!({"session_id": SESSION, "tab_id": tab}),
        Some(3),
    );
    let recorded = host
        .receive(&shot, &lease(), "2026-10-17T00:00:00Z")
        .await
        .unwrap();
    let HostOutcome::Completed {
        output,
        evidence_ref,
        ..
    } = recorded.outcome
    else {
        panic!("the screenshot did not complete");
    };
    let reply: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(reply["status"], "ok", "{reply}");
    let reference = evidence_ref.expect("the pixels are named by the result");
    assert!(reference.starts_with("inline:"), "{reference}");
    // The reply is a small document that says how many bytes there are; it carries none of them.
    let named = reply["evidence_bytes"].as_u64().unwrap();
    assert!(
        named > 100 && output.len() < 4096,
        "{named} bytes named in a {}-byte reply",
        output.len()
    );
    let (media_type, bytes) = host
        .evidence()
        .take(&reference)
        .expect("the pixels are held");
    assert_eq!(media_type, "image/png");
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "a real PNG");
    assert_eq!(bytes.len() as u64, named);
    assert!(
        host.evidence().take(&reference).is_none(),
        "handed over once"
    );
}

#[tokio::test]
async fn a_sink_that_keeps_nothing_makes_a_screenshot_fail_rather_than_lose_its_pixels_silently() {
    let Some(mut world) = world(true).await else {
        return;
    };
    let (tab, _) = open(&mut world, Some(3)).await;
    let shot = envelope(
        "browser.screenshot",
        &json!({"session_id": SESSION, "tab_id": tab}),
        Some(3),
    );
    let recorded = world
        .host
        .receive(&shot, &lease(), "2026-10-17T00:00:00Z")
        .await
        .unwrap();
    assert!(
        matches!(recorded.outcome, HostOutcome::Failed { .. }),
        "an unattached sink cannot take the pixels: {:?}",
        recorded.outcome
    );
}
