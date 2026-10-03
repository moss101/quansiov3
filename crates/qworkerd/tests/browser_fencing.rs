//! Stale-page fencing against a real Google Chrome (ADP-001, DOMAIN.md §8.4.1 A).
//!
//! The acceptance statement is that a stale revision, snapshot or capture never mutates the page. Every
//! test below proves it the same way: the page has a button whose click is visible *outside* the
//! browser — the click asks a real HTTP server for `/hit`, and the server counts — so "the page was not
//! clicked" is a count the browser cannot fake, not an inference from what the driver reported.
//!
//! The fixtures are real: a headless Chrome on a loopback DevTools port and a real HTTP server. When no
//! Chrome is present the suite reports `BLOCKED_EXTERNAL` and returns.

mod common;

use std::time::Duration;

use common::{blocked, call, driver, evaluate, observe, open_page, ref_named, run, Browser, Site};
use serde_json::{json, Value};

const PAGE: &str = r#"<html><head><title>Fencing</title></head><body>
<h1>Fencing page</h1>
<button id="go" onclick="fetch('/hit'); document.getElementById('log').textContent += 'x';">Go</button>
<button id="other">Other</button>
<div id="log"></div>
<a id="two" href="/two">Second page</a>
</body></html>"#;

async fn fixture() -> Option<(Browser, Site, tempfile::TempDir)> {
    let Some(browser) = Browser::launch() else {
        blocked("no Chrome on this host");
        return None;
    };
    let site = Site::start().await;
    site.html("/", PAGE);
    site.html(
        "/two",
        "<html><head><title>Two</title></head><body><button>Elsewhere</button></body></html>",
    );
    site.route("/hit", common::Route::Count);
    Some((browser, site, tempfile::tempdir().expect("session dir")))
}

fn error_code(reply: &Value) -> &str {
    reply["error"]["code"].as_str().unwrap_or("")
}

#[tokio::test]
async fn a_click_on_the_page_the_model_observed_lands_exactly_once() {
    let Some((browser, site, dir)) = fixture().await else {
        return;
    };
    let driver = driver(&browser, dir.path(), |_| {});
    let (tab, revision) = open_page(&driver, 1, &site.url("/")).await;
    let observation = observe(&driver, 2, &tab).await;
    assert_eq!(observation["page_available"], true);
    assert_eq!(observation["document_revision"], revision);
    let snapshot = observation["snapshot_id"]
        .as_str()
        .expect("snapshot")
        .to_string();
    let go = ref_named(&observation, "Go");

    let reply = run(
        &driver,
        &call(3),
        "browser.click",
        json!({ "tab_id": tab, "expected_document_revision": revision, "snapshot_id": snapshot, "ref": go }),
    )
    .await;
    assert_eq!(reply["status"], "ok", "{reply}");
    assert_eq!(reply["trust"], "UNTRUSTED_EXTERNAL");
    assert_eq!(reply["output"]["clicked"], true);
    assert_eq!(reply["output"]["input_origin"], "agent");
    let counted =
        common::eventually(Duration::from_secs(3), || async { site.hits("/hit") == 1 }).await;
    assert!(
        counted,
        "the click never reached the page: {} hits",
        site.hits("/hit")
    );
    assert_eq!(
        evaluate(&browser, &tab, "document.getElementById('log').textContent").await,
        "x"
    );
}

#[tokio::test]
async fn a_stale_revision_snapshot_or_fingerprint_never_clicks() {
    let Some((browser, site, dir)) = fixture().await else {
        return;
    };
    let driver = driver(&browser, dir.path(), |_| {});
    let (tab, revision) = open_page(&driver, 1, &site.url("/")).await;
    let observation = observe(&driver, 2, &tab).await;
    let snapshot = observation["snapshot_id"]
        .as_str()
        .expect("snapshot")
        .to_string();
    let go = ref_named(&observation, "Go");
    let target = |revision: &str| json!({ "tab_id": tab, "expected_document_revision": revision, "snapshot_id": snapshot, "ref": go });

    // 1. A revision the page never had.
    let reply = run(
        &driver,
        &call(3),
        "browser.click",
        target("rev_does_not_exist"),
    )
    .await;
    assert_eq!(error_code(&reply), "BROWSER_STALE_PAGE", "{reply}");
    assert_eq!(reply["error"]["retryable"], true);
    assert_eq!(reply["error"]["requires_new_observation"], true);
    assert_eq!(reply["error"]["details"]["reason"], "document_revision");

    // 2. A ref in a snapshot that does not exist.
    let reply = run(
        &driver,
        &call(4),
        "browser.click",
        json!({ "tab_id": tab, "expected_document_revision": revision, "snapshot_id": "snap_nope", "ref": go }),
    )
    .await;
    assert_eq!(error_code(&reply), "BROWSER_STALE_PAGE", "{reply}");

    // 3. The element behind the ref became a different thing (same node, different name).
    evaluate(
        &browser,
        &tab,
        "document.getElementById('go').textContent = 'Delete account'",
    )
    .await;
    let reply = run(&driver, &call(5), "browser.click", target(&revision)).await;
    assert_eq!(error_code(&reply), "BROWSER_STALE_PAGE", "{reply}");
    assert_eq!(reply["error"]["details"]["reason"], "fingerprint_changed");
    assert_eq!(reply["error"]["details"]["field"], "name");
    evaluate(
        &browser,
        &tab,
        "document.getElementById('go').textContent = 'Go'",
    )
    .await;

    // 4. Something is in front of the element: the point would land on something else.
    evaluate(
        &browser,
        &tab,
        "(() => { const c = document.createElement('div'); c.id = 'cover'; c.style.cssText = 'position:fixed;left:0;top:0;width:100%;height:100%;background:#0001'; document.body.appendChild(c); })()",
    )
    .await;
    let reply = run(&driver, &call(6), "browser.click", target(&revision)).await;
    assert_eq!(error_code(&reply), "BROWSER_STALE_PAGE", "{reply}");
    assert_eq!(reply["error"]["details"]["reason"], "hit_test_mismatch");
    evaluate(&browser, &tab, "document.getElementById('cover').remove()").await;

    // 5. The page navigated: the revision moved on, and the old one is refused.
    let moved = run(
        &driver,
        &call(7),
        "browser.navigate",
        json!({ "tab_id": tab, "expected_document_revision": revision, "url": site.url("/two") }),
    )
    .await;
    assert_eq!(moved["status"], "ok", "{moved}");
    assert_ne!(moved["output"]["document_revision"], json!(revision));
    let reply = run(&driver, &call(8), "browser.click", target(&revision)).await;
    assert_eq!(error_code(&reply), "BROWSER_STALE_PAGE", "{reply}");

    // None of the five reached the page that was clicked: the server saw no `/hit` at all.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(site.hits("/hit"), 0, "a stale action mutated the page");
}

#[tokio::test]
async fn a_snapshot_is_good_for_one_action() {
    let Some((browser, site, dir)) = fixture().await else {
        return;
    };
    let driver = driver(&browser, dir.path(), |_| {});
    let (tab, revision) = open_page(&driver, 1, &site.url("/")).await;
    let observation = observe(&driver, 2, &tab).await;
    let snapshot = observation["snapshot_id"]
        .as_str()
        .expect("snapshot")
        .to_string();
    let go = ref_named(&observation, "Go");
    let args = json!({ "tab_id": tab, "expected_document_revision": revision, "snapshot_id": snapshot, "ref": go });

    let first = run(&driver, &call(3), "browser.click", args.clone()).await;
    assert_eq!(first["status"], "ok", "{first}");
    // The click mutated the page, so what the model read is no longer a description of it.
    let second = run(&driver, &call(4), "browser.click", args).await;
    assert_eq!(error_code(&second), "BROWSER_STALE_PAGE", "{second}");
    assert_eq!(
        second["error"]["details"]["reason"],
        "snapshot_unknown_or_invalidated"
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(site.hits("/hit"), 1);

    // A fresh observation gives a fresh ref, and the click works again.
    let again = observe(&driver, 5, &tab).await;
    let third = run(
        &driver,
        &call(6),
        "browser.click",
        json!({
            "tab_id": tab,
            "expected_document_revision": again["document_revision"],
            "snapshot_id": again["snapshot_id"],
            "ref": ref_named(&again, "Go"),
        }),
    )
    .await;
    assert_eq!(third["status"], "ok", "{third}");
}

#[tokio::test]
async fn a_point_is_bound_to_the_capture_it_was_read_from() {
    let Some((browser, site, dir)) = fixture().await else {
        return;
    };
    let driver = driver(&browser, dir.path(), |_| {});
    let (tab, revision) = open_page(&driver, 1, &site.url("/")).await;
    let shot = run(
        &driver,
        &call(2),
        "browser.screenshot",
        json!({ "tab_id": tab }),
    )
    .await;
    assert_eq!(shot["status"], "ok", "{shot}");
    let capture = shot["output"]["capture_id"]
        .as_str()
        .expect("capture")
        .to_string();
    assert_eq!(shot["output"]["document_revision"], json!(revision));

    // Where the button is, in the viewport the capture was taken from.
    let centre = evaluate(
        &browser,
        &tab,
        "(() => { const r = document.getElementById('go').getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; })()",
    )
    .await;
    let (x, y) = (
        centre[0].as_f64().expect("x"),
        centre[1].as_f64().expect("y"),
    );

    // A point on a stale revision is refused.
    let reply = run(
        &driver,
        &call(3),
        "browser.click",
        json!({ "tab_id": tab, "expected_document_revision": "rev_old", "capture_id": capture, "x": x, "y": y }),
    )
    .await;
    assert_eq!(error_code(&reply), "BROWSER_STALE_PAGE", "{reply}");

    // A point outside the capture is not a point on it.
    let reply = run(
        &driver,
        &call(4),
        "browser.click",
        json!({ "tab_id": tab, "expected_document_revision": revision, "capture_id": capture, "x": 99999, "y": 5 }),
    )
    .await;
    assert_eq!(error_code(&reply), "BROWSER_INVALID_REQUEST", "{reply}");

    // Pixels changing near the point is a warning, not a refusal: the page legitimately animates.
    evaluate(
        &browser,
        &tab,
        "document.getElementById('other').style.background = 'red'",
    )
    .await;
    let reply = run(
        &driver,
        &call(5),
        "browser.click",
        json!({ "tab_id": tab, "expected_document_revision": revision, "capture_id": capture, "x": x, "y": y }),
    )
    .await;
    assert_eq!(reply["status"], "ok", "{reply}");
    let warnings = reply["output"]["warnings"]
        .as_array()
        .expect("a warning is raised");
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .is_some_and(|w| w.starts_with("pixels_changed_near_point"))),
        "{warnings:?}"
    );
    assert_eq!(reply["output"]["hit"]["tag"], "button");
    let counted =
        common::eventually(Duration::from_secs(3), || async { site.hits("/hit") == 1 }).await;
    assert!(counted);

    // The capture is spent by the click.
    let reply = run(
        &driver,
        &call(6),
        "browser.click",
        json!({ "tab_id": tab, "expected_document_revision": revision, "capture_id": capture, "x": x, "y": y }),
    )
    .await;
    assert_eq!(error_code(&reply), "BROWSER_STALE_PAGE", "{reply}");
}

#[tokio::test]
async fn a_point_read_before_the_page_scrolled_is_stale() {
    let Some((browser, site, dir)) = fixture().await else {
        return;
    };
    site.html(
        "/long",
        "<html><body><button id=\"b\" onclick=\"fetch('/hit')\">Top button</button><div style=\"height:4000px\">tall</div></body></html>",
    );
    let driver = driver(&browser, dir.path(), |_| {});
    let (tab, revision) = open_page(&driver, 1, &site.url("/long")).await;
    let shot = run(
        &driver,
        &call(2),
        "browser.screenshot",
        json!({ "tab_id": tab }),
    )
    .await;
    let capture = shot["output"]["capture_id"]
        .as_str()
        .expect("capture")
        .to_string();
    evaluate(&browser, &tab, "window.scrollTo(0, 800)").await;
    let reply = run(
        &driver,
        &call(3),
        "browser.click",
        json!({ "tab_id": tab, "expected_document_revision": revision, "capture_id": capture, "x": 20, "y": 20 }),
    )
    .await;
    assert_eq!(error_code(&reply), "BROWSER_STALE_PAGE", "{reply}");
    assert_eq!(reply["error"]["details"]["reason"], "scrolled");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(site.hits("/hit"), 0);
}
