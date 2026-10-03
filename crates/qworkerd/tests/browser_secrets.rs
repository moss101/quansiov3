//! Secret placeholders and sensitive fields, against a real Google Chrome (ADP-001, DOMAIN.md §8.4.1 C).
//!
//! Three acceptance statements are proved here: a secret never appears in model context, tool arguments
//! at rest, ledger digests, logs or events; a secret is refused on an origin outside its allow-list;
//! and plain text is refused in a password, one-time-code or card field.
//!
//! The origins are real. Three HTTP servers on three loopback ports are three origins to Chrome, and a
//! page on one embeds a frame from another, so "the top frame *and* the target frame" is a real
//! cross-origin frame rather than a stand-in. What the page ends up holding is read back from the page
//! itself, as a boolean (`value === canary`), so the tests never print the secret they are checking.
//! The canary is searched for in every persisted artifact and in every reply the model would see.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{
    blocked, call, driver, driver_with_secrets, evaluate, observe, open_page, ref_named, run,
    Browser, Route, Site,
};
use quansio_qworkerd::browser::secrets::{ResolvedSecret, SecretPort, SecretRequest, SecretText};
use serde_json::{json, Value};

const CANARY: &str = "hunter2-CANARY-7731";

fn login_page(frame_port: u16) -> String {
    format!(
        r#"<html><head><title>Login</title></head><body>
<input id="u" name="user" type="text" aria-label="Username">
<input id="pw" name="pw" type="password" aria-label="Password">
<input id="otp" name="code" type="text" autocomplete="one-time-code" aria-label="One-time code">
<input id="cc" name="card" type="text" autocomplete="cc-number" aria-label="Card number">
<input id="plain" name="comment" type="text" aria-label="Comment">
<iframe id="f" src="http://127.0.0.1:{frame_port}/frame" width="420" height="80"></iframe>
</body></html>"#
    )
}

fn frame_page() -> String {
    format!(
        r#"<html><body><input id="fpw" name="fpw" type="password" aria-label="Frame password"
oninput="if (this.value === '{CANARY}') fetch('/typed_ok');"></body></html>"#
    )
}

struct Fixture {
    browser: Browser,
    top: Site,
    frame: Site,
    other: Site,
    dir: tempfile::TempDir,
}

async fn fixture() -> Option<Fixture> {
    let Some(browser) = Browser::launch() else {
        blocked("no Chrome on this host");
        return None;
    };
    let (top, frame, other) = (
        Site::start().await,
        Site::start().await,
        Site::start().await,
    );
    top.html("/login", &login_page(frame.port));
    frame.html("/frame", &frame_page());
    frame.route("/typed_ok", Route::Count);
    other.html("/login", &login_page(frame.port));
    Some(Fixture {
        browser,
        top,
        frame,
        other,
        dir: tempfile::tempdir().expect("session dir"),
    })
}

/// A secret port that resolves `acme.username` and `acme.password`, for the origins in `allowed`.
fn port(
    value: &'static str,
    allowed: Arc<Mutex<Vec<String>>>,
    resolved: Arc<AtomicUsize>,
    delay: Duration,
) -> SecretPort {
    SecretPort::new(move |request: SecretRequest| {
        let allowed = Arc::clone(&allowed);
        let resolved = Arc::clone(&resolved);
        Box::pin(async move {
            resolved.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(delay).await;
            let text = match (request.name.as_str(), request.field.as_str()) {
                ("acme", "password") => value,
                ("acme", "username") => "alice",
                _ => {
                    return Err(quansio_qworkerd::browser::BrowserFault::new(
                        quansio_qworkerd::browser::FaultCode::SecretUnavailable,
                        "no such secret",
                    ))
                }
            };
            Ok(ResolvedSecret {
                value: SecretText::new(text.to_string()),
                allowed_origins: allowed.lock().expect("allowed").clone(),
            })
        })
    })
}

async fn field_is(browser: &Browser, tab: &str, id: &str, expected: &str) -> bool {
    evaluate(
        browser,
        tab,
        &format!(
            "document.getElementById({id:?}).value === {}",
            Value::String(expected.to_string())
        ),
    )
    .await
        == json!(true)
}

async fn field_is_empty(browser: &Browser, tab: &str, id: &str) -> bool {
    field_is(browser, tab, id, "").await
}

fn type_args(tab: &str, observation: &Value, name: &str, text: &str) -> Value {
    json!({
        "tab_id": tab,
        "expected_document_revision": observation["document_revision"],
        "snapshot_id": observation["snapshot_id"],
        "ref": ref_named(observation, name),
        "text": text,
    })
}

fn every_file(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

#[tokio::test]
async fn plain_text_is_refused_in_password_one_time_code_and_card_fields() {
    let Some(f) = fixture().await else {
        return;
    };
    let resolved = Arc::new(AtomicUsize::new(0));
    let allowed = Arc::new(Mutex::new(vec![f.top.origin()]));
    let driver = driver_with_secrets(
        &f.browser,
        f.dir.path(),
        port(CANARY, allowed, Arc::clone(&resolved), Duration::ZERO),
        |_| {},
    );
    let (tab, _) = open_page(&driver, 1, &f.top.url("/login")).await;
    let mut n = 2;
    for (name, id, kind) in [
        ("Password", "pw", "password"),
        ("One-time code", "otp", "one_time_code"),
        ("Card number", "cc", "card_number"),
    ] {
        n += 2;
        let observation = observe(&driver, n, &tab).await;
        // The observation tells the model which fields are sensitive, so it knows to use a placeholder.
        let element = observation["elements"]
            .as_array()
            .expect("elements")
            .iter()
            .find(|e| e["name"] == name)
            .expect("the field is listed");
        assert_eq!(element["sensitive"], kind, "{element}");

        let reply = run(
            &driver,
            &call(n + 1),
            "browser.type",
            type_args(&tab, &observation, name, "123456"),
        )
        .await;
        assert_eq!(reply["status"], "error", "{reply}");
        assert_eq!(
            reply["error"]["code"], "BROWSER_SENSITIVE_INPUT_DENIED",
            "{reply}"
        );
        assert_eq!(reply["error"]["retryable"], false);
        assert_eq!(reply["error"]["details"]["kind"], kind);
        assert!(
            field_is_empty(&f.browser, &tab, id).await,
            "plain text reached the {name} field"
        );
    }

    // Literal characters mixed into a placeholder do not make it acceptable either.
    n += 2;
    let observation = observe(&driver, n, &tab).await;
    let reply = run(
        &driver,
        &call(n + 1),
        "browser.type",
        type_args(&tab, &observation, "Password", "x{{secret:acme.password}}"),
    )
    .await;
    assert_eq!(
        reply["error"]["code"], "BROWSER_SENSITIVE_INPUT_DENIED",
        "{reply}"
    );
    assert!(field_is_empty(&f.browser, &tab, "pw").await);
    // Every one of those was refused before any secret was materialized.
    assert_eq!(
        resolved.load(Ordering::SeqCst),
        0,
        "a refused call still materialized a credential"
    );
}

#[tokio::test]
async fn a_placeholder_is_resolved_at_insertion_and_the_value_leaves_no_trace() {
    let Some(f) = fixture().await else {
        return;
    };
    let allowed = Arc::new(Mutex::new(vec![f.top.origin()]));
    let driver = driver_with_secrets(
        &f.browser,
        f.dir.path(),
        port(
            CANARY,
            allowed,
            Arc::new(AtomicUsize::new(0)),
            Duration::ZERO,
        ),
        |_| {},
    );
    let (tab, _) = open_page(&driver, 1, &f.top.url("/login")).await;

    let observation = observe(&driver, 2, &tab).await;
    let user = driver
        .dispatch(
            "browser.type",
            &{
                let mut args =
                    type_args(&tab, &observation, "Username", "{{secret:acme.username}}");
                args["session_id"] = json!(common::SESSION);
                args
            },
            &call(3),
        )
        .await;
    assert_eq!(user.status.as_str(), "ok", "{:?}", user.to_json());
    assert!(field_is(&f.browser, &tab, "u", "alice").await);

    let observation = observe(&driver, 4, &tab).await;
    let args = {
        let mut args = type_args(&tab, &observation, "Password", "{{secret:acme.password}}");
        args["session_id"] = json!(common::SESSION);
        args
    };
    let reply = driver.dispatch("browser.type", &args, &call(5)).await;
    assert_eq!(reply.status.as_str(), "ok", "{:?}", reply.to_json());
    assert_eq!(
        reply.classified.as_ref().and_then(|c| c.effect_class),
        Some("credential.access")
    );
    assert_eq!(reply.classified.as_ref().and_then(|c| c.tier), Some(4));
    let wire = reply.to_json();
    assert_eq!(wire["output"]["sensitive_field"], "password");
    assert_eq!(
        wire["output"]["used_secrets"],
        json!([{ "name": "acme", "field": "password" }])
    );
    // The page holds the real value...
    assert!(
        field_is(&f.browser, &tab, "pw", CANARY).await,
        "the secret was not inserted"
    );

    // ...and nothing the model, a log or the ledger could see does.
    let after = observe(&driver, 6, &tab).await;
    let seen = [
        wire.to_string(),
        format!("{reply:?}"),
        after.to_string(),
        args.to_string(),
        format!("{:?}", driver.guard_report()),
    ];
    for text in &seen {
        assert!(
            !text.contains(CANARY),
            "the secret reached a model-facing value: {text}"
        );
    }
    for path in every_file(f.dir.path()) {
        let bytes = std::fs::read(&path).expect("read");
        assert!(
            !String::from_utf8_lossy(&bytes).contains(CANARY),
            "the secret was persisted in {}",
            path.display()
        );
    }
    // The journal did record the typing, in placeholder form only.
    let journal = std::fs::read_to_string(f.dir.path().join("actions.jsonl")).expect("journal");
    assert!(journal.lines().count() >= 6);
}

#[tokio::test]
async fn the_ledger_digest_is_over_the_placeholder_form_not_the_value() {
    let Some(f) = fixture().await else {
        return;
    };
    let allowed = Arc::new(Mutex::new(vec![f.top.origin()]));
    let driver = driver_with_secrets(
        &f.browser,
        f.dir.path(),
        port(
            CANARY,
            allowed,
            Arc::new(AtomicUsize::new(0)),
            Duration::ZERO,
        ),
        |_| {},
    );
    let (tab, _) = open_page(&driver, 1, &f.top.url("/login")).await;
    let observation = observe(&driver, 2, &tab).await;
    let placeholder = "{{secret:acme.password}}";
    let reply = run(
        &driver,
        &call(3),
        "browser.type",
        type_args(&tab, &observation, "Password", placeholder),
    )
    .await;
    assert_eq!(reply["status"], "ok", "{reply}");

    // What the journal recorded as the action's digest.
    let journal = std::fs::read_to_string(f.dir.path().join("actions.jsonl")).expect("journal");
    let recorded = journal
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|record| record["op"] == "start" && record["call_id"] == "tc_call0003")
        .expect("the type action was journaled")["digest"]
        .as_str()
        .expect("digest")
        .to_string();

    // The digest of the arguments as the model wrote them, and of the same arguments with the value.
    let digest_of = |text: &str| {
        let args = json!({
            "tab_id": tab,
            "expected_document_revision": observation["document_revision"],
            "target": { "snapshot_id": observation["snapshot_id"], "ref": ref_named(&observation, "Password") },
            "text": text,
            "clear": false,
        });
        quansio_core::Digest::of_canonical_json(&quansio_tools::canonical_json(
            &json!({ "tool": "browser.type", "args": args }),
        ))
        .as_str()
        .to_string()
    };
    assert_eq!(
        recorded,
        digest_of(placeholder),
        "the digest is not over the placeholder form"
    );
    assert_ne!(
        recorded,
        digest_of(CANARY),
        "the digest is over the substituted value"
    );
    assert!(!journal.contains(CANARY), "the value was persisted");
}

#[tokio::test]
async fn a_secret_is_refused_on_an_origin_outside_its_allow_list() {
    let Some(f) = fixture().await else {
        return;
    };
    let allowed = Arc::new(Mutex::new(vec![f.top.origin()]));
    let driver = driver_with_secrets(
        &f.browser,
        f.dir.path(),
        port(
            CANARY,
            Arc::clone(&allowed),
            Arc::new(AtomicUsize::new(0)),
            Duration::ZERO,
        ),
        |_| {},
    );

    // The same page on a different origin: the field is the same, the origin is not the secret's.
    let (tab, _) = open_page(&driver, 1, &f.other.url("/login")).await;
    let observation = observe(&driver, 2, &tab).await;
    let reply = run(
        &driver,
        &call(3),
        "browser.type",
        type_args(&tab, &observation, "Password", "{{secret:acme.password}}"),
    )
    .await;
    assert_eq!(reply["status"], "error", "{reply}");
    assert_eq!(
        reply["error"]["code"], "BROWSER_SECRET_ORIGIN_MISMATCH",
        "{reply}"
    );
    assert_eq!(reply["error"]["retryable"], false);
    assert_eq!(reply["error"]["details"]["which"], "top");
    assert!(!reply.to_string().contains(CANARY));
    assert!(
        field_is_empty(&f.browser, &tab, "pw").await,
        "the secret was typed on the wrong origin"
    );

    // An allow-list entry that is not an origin allows nothing.
    *allowed.lock().expect("allowed") = vec!["not an origin".to_string()];
    let (tab, _) = open_page(&driver, 4, &f.top.url("/login")).await;
    let observation = observe(&driver, 5, &tab).await;
    let reply = run(
        &driver,
        &call(6),
        "browser.type",
        type_args(&tab, &observation, "Password", "{{secret:acme.password}}"),
    )
    .await;
    assert_eq!(
        reply["error"]["code"], "BROWSER_SECRET_ORIGIN_MISMATCH",
        "{reply}"
    );
    assert!(field_is_empty(&f.browser, &tab, "pw").await);
}

#[tokio::test]
async fn the_target_frame_is_checked_as_well_as_the_top_frame() {
    let Some(f) = fixture().await else {
        return;
    };
    // The top page is the secret's origin; the frame the field lives in is a different origin.
    let allowed = Arc::new(Mutex::new(vec![f.top.origin()]));
    let driver = driver_with_secrets(
        &f.browser,
        f.dir.path(),
        port(
            CANARY,
            Arc::clone(&allowed),
            Arc::new(AtomicUsize::new(0)),
            Duration::ZERO,
        ),
        |_| {},
    );
    let (tab, _) = open_page(&driver, 1, &f.top.url("/login")).await;
    let observation = observe(&driver, 2, &tab).await;
    let framed = observation["elements"]
        .as_array()
        .expect("elements")
        .iter()
        .find(|e| e["name"] == "Frame password")
        .expect("the frame's field is listed");
    assert_eq!(framed["frame_origin"], json!(f.frame.origin()));

    let reply = run(
        &driver,
        &call(3),
        "browser.type",
        type_args(
            &tab,
            &observation,
            "Frame password",
            "{{secret:acme.password}}",
        ),
    )
    .await;
    assert_eq!(
        reply["error"]["code"], "BROWSER_SECRET_ORIGIN_MISMATCH",
        "{reply}"
    );
    assert_eq!(reply["error"]["details"]["which"], "target_frame");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        f.frame.hits("/typed_ok"),
        0,
        "the secret reached a frame it was not issued for"
    );

    // Allowed for both origins, the same call types into the frame.
    *allowed.lock().expect("allowed") = vec![f.top.origin(), f.frame.origin()];
    let observation = observe(&driver, 4, &tab).await;
    let reply = run(
        &driver,
        &call(5),
        "browser.type",
        type_args(
            &tab,
            &observation,
            "Frame password",
            "{{secret:acme.password}}",
        ),
    )
    .await;
    assert_eq!(reply["status"], "ok", "{reply}");
    assert!(
        common::eventually(Duration::from_secs(3), || async {
            f.frame.hits("/typed_ok") >= 1
        })
        .await,
        "the frame never received the secret"
    );
}

#[tokio::test]
async fn a_page_that_moves_while_the_secret_is_resolved_receives_nothing() {
    let Some(f) = fixture().await else {
        return;
    };
    let allowed = Arc::new(Mutex::new(vec![f.top.origin(), f.other.origin()]));
    // Resolving takes a moment — a broker round trip — and the page navigates in the middle of it.
    let driver = driver_with_secrets(
        &f.browser,
        f.dir.path(),
        port(
            CANARY,
            allowed,
            Arc::new(AtomicUsize::new(0)),
            Duration::from_millis(600),
        ),
        |_| {},
    );
    let (tab, _) = open_page(&driver, 1, &f.top.url("/login")).await;
    let observation = observe(&driver, 2, &tab).await;
    let args = type_args(&tab, &observation, "Password", "{{secret:acme.password}}");
    let worker = {
        let driver = driver.clone();
        tokio::spawn(async move { run(&driver, &call(3), "browser.type", args).await })
    };
    tokio::time::sleep(Duration::from_millis(250)).await;
    evaluate(
        &f.browser,
        &tab,
        &format!("location.href = {:?}", f.other.url("/login")),
    )
    .await;
    let reply = worker.await.expect("the type call finishes");
    assert_eq!(reply["status"], "error", "{reply}");
    assert!(
        ["BROWSER_STALE_PAGE", "BROWSER_SECRET_ORIGIN_MISMATCH"]
            .contains(&reply["error"]["code"].as_str().unwrap_or_default()),
        "{reply}"
    );
    let landed = common::eventually(Duration::from_secs(5), || async {
        evaluate(&f.browser, &tab, "location.origin").await == json!(f.other.origin())
    })
    .await;
    assert!(landed, "the page did not move");
    assert!(
        field_is_empty(&f.browser, &tab, "pw").await,
        "the secret landed on the page the model never saw"
    );
}

#[tokio::test]
async fn a_printable_key_is_refused_in_a_sensitive_field_and_in_a_frame_it_cannot_read() {
    let Some(f) = fixture().await else {
        return;
    };
    let driver = driver(&f.browser, f.dir.path(), |_| {});
    let (tab, _) = open_page(&driver, 1, &f.top.url("/login")).await;
    let mut n = 1;
    let mut press = |key: &'static str| {
        n += 1;
        (n, key)
    };

    // Focus the password field by clicking it, then try to type into it key by key.
    let observation = observe(&driver, 90, &tab).await;
    let click = run(
        &driver,
        &call(91),
        "browser.click",
        json!({
            "tab_id": tab,
            "expected_document_revision": observation["document_revision"],
            "snapshot_id": observation["snapshot_id"],
            "ref": ref_named(&observation, "Password"),
        }),
    )
    .await;
    assert_eq!(click["status"], "ok", "{click}");
    let revision = click["output"]["document_revision"].clone();
    let (id, key) = press("a");
    let reply = run(
        &driver,
        &call(id),
        "browser.press_key",
        json!({ "tab_id": tab, "expected_document_revision": revision, "key": key }),
    )
    .await;
    assert_eq!(
        reply["error"]["code"], "BROWSER_SENSITIVE_INPUT_DENIED",
        "{reply}"
    );
    assert!(
        field_is_empty(&f.browser, &tab, "pw").await,
        "a printable key reached the password field"
    );
    // A chord with a printable key (a paste) is not an exception.
    let (id, key) = press("Control+v");
    let reply = run(
        &driver,
        &call(id),
        "browser.press_key",
        json!({ "tab_id": tab, "expected_document_revision": revision, "key": key }),
    )
    .await;
    assert_eq!(
        reply["error"]["code"], "BROWSER_SENSITIVE_INPUT_DENIED",
        "{reply}"
    );
    // A key that produces no character is fine: Tab moves on.
    let (id, key) = press("Tab");
    let reply = run(
        &driver,
        &call(id),
        "browser.press_key",
        json!({ "tab_id": tab, "expected_document_revision": revision, "key": key }),
    )
    .await;
    assert_eq!(reply["status"], "ok", "{reply}");

    // In an ordinary field the same key is typed.
    let observation = observe(&driver, 92, &tab).await;
    let click = run(
        &driver,
        &call(93),
        "browser.click",
        json!({
            "tab_id": tab,
            "expected_document_revision": observation["document_revision"],
            "snapshot_id": observation["snapshot_id"],
            "ref": ref_named(&observation, "Comment"),
        }),
    )
    .await;
    let revision = click["output"]["document_revision"].clone();
    let reply = run(
        &driver,
        &call(94),
        "browser.press_key",
        json!({ "tab_id": tab, "expected_document_revision": revision, "key": "b" }),
    )
    .await;
    assert_eq!(reply["status"], "ok", "{reply}");
    assert!(field_is(&f.browser, &tab, "plain", "b").await);

    // Focus inside a frame the driver cannot read is treated as sensitive: it cannot say what is there.
    let observation = observe(&driver, 95, &tab).await;
    let click = run(
        &driver,
        &call(96),
        "browser.click",
        json!({
            "tab_id": tab,
            "expected_document_revision": observation["document_revision"],
            "snapshot_id": observation["snapshot_id"],
            "ref": ref_named(&observation, "Frame password"),
        }),
    )
    .await;
    assert_eq!(click["status"], "ok", "{click}");
    let revision = click["output"]["document_revision"].clone();
    let reply = run(
        &driver,
        &call(97),
        "browser.press_key",
        json!({ "tab_id": tab, "expected_document_revision": revision, "key": "z" }),
    )
    .await;
    assert_eq!(
        reply["error"]["code"], "BROWSER_SENSITIVE_INPUT_DENIED",
        "{reply}"
    );
    assert_eq!(reply["error"]["details"]["cross_origin_focus"], true);
}

#[tokio::test]
async fn a_placeholder_without_a_broker_or_above_the_approved_tier_is_refused() {
    let Some(f) = fixture().await else {
        return;
    };
    // No broker installed.
    let bare = driver(&f.browser, f.dir.path(), |_| {});
    let (tab, _) = open_page(&bare, 1, &f.top.url("/login")).await;
    let observation = observe(&bare, 2, &tab).await;
    let reply = run(
        &bare,
        &call(3),
        "browser.type",
        type_args(&tab, &observation, "Password", "{{secret:acme.password}}"),
    )
    .await;
    assert_eq!(
        reply["error"]["code"], "BROWSER_SECRET_UNAVAILABLE",
        "{reply}"
    );

    // A broker, but the runtime approved less than a credential access (tier 4).
    let dir = tempfile::tempdir().expect("dir");
    let resolved = Arc::new(AtomicUsize::new(0));
    let allowed = Arc::new(Mutex::new(vec![f.top.origin()]));
    let driver = driver_with_secrets(
        &f.browser,
        dir.path(),
        port(CANARY, allowed, Arc::clone(&resolved), Duration::ZERO),
        |_| {},
    );
    let (tab, _) = open_page(&driver, 1, &f.top.url("/login")).await;
    let observation = observe(&driver, 2, &tab).await;
    let mut ctx = call(3);
    ctx.approved_tier = Some(3);
    let reply = run(
        &driver,
        &ctx,
        "browser.type",
        type_args(&tab, &observation, "Password", "{{secret:acme.password}}"),
    )
    .await;
    assert_eq!(
        reply["error"]["code"], "BROWSER_EFFECT_TIER_EXCEEDED",
        "{reply}"
    );
    assert_eq!(resolved.load(Ordering::SeqCst), 0);
    assert!(field_is_empty(&f.browser, &tab, "pw").await);

    // An unknown secret is unavailable, not a guess.
    let observation = observe(&driver, 4, &tab).await;
    let reply = run(
        &driver,
        &call(5),
        "browser.type",
        type_args(&tab, &observation, "Password", "{{secret:nobody.password}}"),
    )
    .await;
    assert_eq!(
        reply["error"]["code"], "BROWSER_SECRET_UNAVAILABLE",
        "{reply}"
    );

    // A half-written placeholder is refused rather than typed.
    let observation = observe(&driver, 6, &tab).await;
    let reply = run(
        &driver,
        &call(7),
        "browser.type",
        type_args(&tab, &observation, "Comment", "{{secret:acme.password"),
    )
    .await;
    assert_eq!(reply["error"]["code"], "BROWSER_INVALID_REQUEST", "{reply}");
    assert!(field_is_empty(&f.browser, &tab, "plain").await);
}
