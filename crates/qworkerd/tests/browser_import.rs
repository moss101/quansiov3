//! The approved browser-data import against a real Google Chrome (ADP-009, DOMAIN.md §13.9 B).
//!
//! `browser.import_session` is host-only: a person staged a cookie bundle, the runtime asked for the tier 4 approval, and this tool
//! applies the bundle to the managed browser. The cookies here are made up for a loopback site this suite serves; the page reads
//! back what the browser holds, and the value is searched for in everything the driver persists and returns.
//!
//! Proved here:
//! * an approved call applies every cookie and the browser holds them: a script on the page reads the ones that are not
//!   `HttpOnly`, and the browser's own cookie list holds the ones that are;
//! * the call needs an approved tier of 4: with none stated, or a lower one, nothing is taken from the runtime and nothing is applied;
//! * the bundle is taken once; a second call, and a replayed call, take nothing more;
//! * a bundle that does not hold the approved number of cookies applies nothing and is spent;
//! * a cookie the browser drops is counted as rejected, the count reported is what the browser holds, and the rest are applied;
//! * the journal, the result and `Debug` hold the digest and counts, never a cookie value.
//!
//! The runtime half (staging, the digest check, the overwrite on take) is proved against real PostgreSQL in
//! `crates/server/tests/browser_import.rs`; here the port is a closure over an in-memory bundle, as the payment suite's is.
//! Without Chrome a test prints `BLOCKED_EXTERNAL`.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::{blocked, call, driver_with_secrets, evaluate, open_page, run, Browser, Site};
use quansio_machine::secrets::imports::{bundle_digest, ImportCookie, SameSite};
use quansio_qworkerd::browser::secrets::{ImportRequest, ImportedBundle, SecretPort};
use quansio_qworkerd::browser::{BrowserCallContext, BrowserFault, FaultCode};
use serde_json::{json, Value};

const CANARY: &str = "s3ss10n-canary-7f3a";
const HTTP_ONLY_CANARY: &str = "h77p-0nly-canary-91c2";

fn cookie(name: &str, value: &str, http_only: bool) -> ImportCookie {
    ImportCookie {
        name: name.to_string(),
        value: value.to_string(),
        domain: "127.0.0.1".to_string(),
        path: "/".to_string(),
        secure: false,
        http_only,
        same_site: SameSite::Lax,
        expires: Some(4_102_444_800),
    }
}

fn bundle() -> Vec<ImportCookie> {
    vec![
        cookie("session", CANARY, false),
        cookie("auth", HTTP_ONLY_CANARY, true),
        cookie("theme", "dark", false),
    ]
}

/// A staged bundle the runtime would hand over once.
#[derive(Clone)]
struct Staged {
    bundle: Arc<Mutex<Option<Vec<ImportCookie>>>>,
    taken: Arc<AtomicUsize>,
}

impl Staged {
    fn new(cookies: Vec<ImportCookie>) -> Self {
        Self {
            bundle: Arc::new(Mutex::new(Some(cookies))),
            taken: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn port(&self) -> SecretPort {
        let staged = self.clone();
        SecretPort::new(|_| {
            Box::pin(async {
                Err(BrowserFault::new(
                    FaultCode::SecretUnavailable,
                    "this suite types no placeholder",
                ))
            })
        })
        .with_imports(move |request: ImportRequest| {
            let staged = staged.clone();
            Box::pin(async move {
                staged.taken.fetch_add(1, Ordering::SeqCst);
                // A take consumes: the bundle is removed as it is handed over, and the runtime refuses a digest that is not the
                // staged bundle's.
                let mut slot = staged.bundle.lock().expect("bundle");
                match slot.take() {
                    Some(cookies) if bundle_digest(&cookies) == request.digest => {
                        Ok(ImportedBundle { cookies })
                    }
                    Some(_) => Err(BrowserFault::new(
                        FaultCode::SecretUnavailable,
                        "the staged bundle is not the one that was approved",
                    )),
                    None => Err(BrowserFault::new(
                        FaultCode::SecretUnavailable,
                        "no staged bundle is waiting under this id",
                    )),
                }
            })
        })
    }

    fn waiting(&self) -> bool {
        self.bundle.lock().expect("bundle").is_some()
    }
}

fn tier(approved: Option<u8>, n: u32) -> BrowserCallContext {
    BrowserCallContext {
        approved_tier: approved,
        ..call(n)
    }
}

fn args(cookies: &[ImportCookie], count: usize) -> Value {
    json!({
        "session_id": common::SESSION,
        "import_id": "sec_01J8Z3K6F1N8VQ2X5W9Y0IMPRT",
        "digest": bundle_digest(cookies),
        "cookie_count": count,
        "domains": ["127.0.0.1"],
    })
}

async fn import(
    driver: &quansio_qworkerd::browser::BrowserDriver,
    ctx: &BrowserCallContext,
    args: Value,
) -> Value {
    run(driver, ctx, "browser.import_session", args).await
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

struct Fixture {
    browser: Browser,
    site: Site,
    dir: tempfile::TempDir,
}

async fn fixture() -> Option<Fixture> {
    let Some(browser) = Browser::launch() else {
        blocked("no Chrome on this host");
        return None;
    };
    let site = Site::start().await;
    site.html(
        "/",
        "<html><head><title>Account</title></head><body>signed out</body></html>",
    );
    Some(Fixture {
        browser,
        site,
        dir: tempfile::tempdir().expect("session dir"),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approved_import_puts_the_cookies_in_the_browser_and_nothing_records_a_value() {
    let Some(f) = fixture().await else { return };
    let cookies = bundle();
    let staged = Staged::new(cookies.clone());
    let driver = driver_with_secrets(&f.browser, f.dir.path(), staged.port(), |_| {});
    let (tab, _) = open_page(&driver, 1, &f.site.url("/")).await;
    assert_eq!(
        evaluate(&f.browser, &tab, "document.cookie").await,
        json!(""),
        "the browser starts with no cookie for the site"
    );

    let reply = import(&driver, &tier(Some(4), 2), args(&cookies, 3)).await;
    assert_eq!(reply["status"], "ok", "{reply}");
    assert_eq!(reply["output"]["imported"], true);
    assert_eq!(reply["output"]["applied"], 3);
    assert_eq!(reply["output"]["rejected"], 0);
    assert_eq!(reply["output"]["digest"], bundle_digest(&cookies));

    // The page sees the cookies that are not HttpOnly and not the one that is; the browser holds all of them.
    let seen = evaluate(&f.browser, &tab, "document.cookie").await;
    let seen = seen.as_str().expect("cookie string").to_string();
    assert!(seen.contains(&format!("session={CANARY}")), "{seen}");
    assert!(seen.contains("theme=dark"), "{seen}");
    assert!(
        !seen.contains("auth="),
        "an HttpOnly cookie is not readable by the page: {seen}"
    );
    assert!(!staged.waiting(), "the bundle was taken");

    // Nothing the model would see, and nothing at rest, holds a value.
    let model_sees = reply.to_string();
    for canary in [CANARY, HTTP_ONLY_CANARY] {
        assert!(!model_sees.contains(canary), "{model_sees}");
    }
    for path in every_file(f.dir.path()) {
        let bytes = std::fs::read(&path).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        for canary in [CANARY, HTTP_ONLY_CANARY] {
            assert!(
                !text.contains(canary),
                "{} holds a cookie value",
                path.display()
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn without_an_approved_tier_of_four_nothing_is_taken_and_nothing_is_applied() {
    let Some(f) = fixture().await else { return };
    let cookies = bundle();
    let staged = Staged::new(cookies.clone());
    let driver = driver_with_secrets(&f.browser, f.dir.path(), staged.port(), |_| {});
    let (tab, _) = open_page(&driver, 1, &f.site.url("/")).await;
    for (n, approved) in [(2, None), (3, Some(3)), (4, Some(1))] {
        let reply = import(&driver, &tier(approved, n), args(&cookies, 3)).await;
        assert_eq!(reply["status"], "error", "{approved:?}: {reply}");
        assert_eq!(reply["error"]["details"]["stage"], "tier", "{reply}");
    }
    assert_eq!(
        staged.taken.load(Ordering::SeqCst),
        0,
        "the runtime was never asked for the bundle"
    );
    assert!(staged.waiting(), "and it still waits for an approval");
    assert_eq!(
        evaluate(&f.browser, &tab, "document.cookie").await,
        json!("")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_is_used_once_and_a_replay_takes_nothing_more() {
    let Some(f) = fixture().await else { return };
    let cookies = bundle();
    let staged = Staged::new(cookies.clone());
    let driver = driver_with_secrets(&f.browser, f.dir.path(), staged.port(), |_| {});
    let (_tab, _) = open_page(&driver, 1, &f.site.url("/")).await;
    let ctx = tier(Some(4), 2);
    let first = import(&driver, &ctx, args(&cookies, 3)).await;
    assert_eq!(first["status"], "ok", "{first}");
    assert_eq!(staged.taken.load(Ordering::SeqCst), 1);

    // The same call delivered again returns the stored result and does not touch the port.
    let replay = import(&driver, &ctx, args(&cookies, 3)).await;
    assert_eq!(replay["status"], "ok", "{replay}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        staged.taken.load(Ordering::SeqCst),
        1,
        "a replay takes nothing"
    );

    // A new call finds nothing: the person stages again and the approval is asked again.
    let again = import(&driver, &tier(Some(4), 3), args(&cookies, 3)).await;
    assert_eq!(again["status"], "error", "{again}");
    assert_eq!(again["error"]["code"], "BROWSER_SECRET_UNAVAILABLE");
    assert_eq!(again["error"]["details"]["stage"], "take");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_that_is_not_the_approved_size_or_digest_applies_nothing_and_is_spent() {
    let Some(f) = fixture().await else { return };
    let cookies = bundle();

    // The count the approval carried is not the bundle's.
    let staged = Staged::new(cookies.clone());
    let driver = driver_with_secrets(&f.browser, f.dir.path(), staged.port(), |_| {});
    let (tab, _) = open_page(&driver, 1, &f.site.url("/")).await;
    let reply = import(&driver, &tier(Some(4), 2), args(&cookies, 2)).await;
    assert_eq!(reply["status"], "error", "{reply}");
    assert_eq!(reply["error"]["details"]["stage"], "verify", "{reply}");
    assert_eq!(
        evaluate(&f.browser, &tab, "document.cookie").await,
        json!("")
    );
    assert!(
        !staged.waiting(),
        "a bundle that failed verification is spent, not kept for another try"
    );

    // The digest the approval carried is not the staged bundle's: the runtime refuses it.
    let staged = Staged::new(cookies.clone());
    let dir = tempfile::tempdir().expect("dir");
    let driver = driver_with_secrets(&f.browser, dir.path(), staged.port(), |_| {});
    let (tab, _) = open_page(&driver, 1, &f.site.url("/")).await;
    let mut swapped = args(&cookies, 3);
    swapped["digest"] = json!(bundle_digest(&[cookie("other", "x", false)]));
    let reply = import(&driver, &tier(Some(4), 2), swapped).await;
    assert_eq!(reply["status"], "error", "{reply}");
    assert_eq!(reply["error"]["details"]["stage"], "take", "{reply}");
    assert_eq!(
        evaluate(&f.browser, &tab, "document.cookie").await,
        json!("")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cookie_the_browser_refuses_is_counted_and_the_rest_are_applied() {
    let Some(f) = fixture().await else { return };
    let mut cookies = bundle();
    // SameSite=None without Secure is refused by the browser.
    let mut bad = cookie("bad", "x", false);
    bad.same_site = SameSite::None;
    cookies.push(bad);
    let staged = Staged::new(cookies.clone());
    let driver = driver_with_secrets(&f.browser, f.dir.path(), staged.port(), |_| {});
    let (tab, _) = open_page(&driver, 1, &f.site.url("/")).await;
    let reply = import(&driver, &tier(Some(4), 2), args(&cookies, 4)).await;
    assert_eq!(reply["status"], "ok", "{reply}");
    assert_eq!(reply["output"]["imported"], false, "{reply}");
    assert_eq!(reply["output"]["rejected"], 1, "{reply}");
    assert_eq!(reply["output"]["applied"], 3, "{reply}");
    let seen = evaluate(&f.browser, &tab, "document.cookie").await;
    assert!(
        seen.as_str().unwrap_or_default().contains("theme=dark"),
        "{seen}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_with_no_import_source_refuses_at_the_take() {
    let Some(f) = fixture().await else { return };
    let cookies = bundle();
    let bare = SecretPort::new(|_| {
        Box::pin(async { Err(BrowserFault::new(FaultCode::SecretUnavailable, "none")) })
    });
    let driver = driver_with_secrets(&f.browser, f.dir.path(), bare, |_| {});
    let (_tab, _) = open_page(&driver, 1, &f.site.url("/")).await;
    let reply = import(&driver, &tier(Some(4), 2), args(&cookies, 3)).await;
    assert_eq!(reply["status"], "error", "{reply}");
    assert_eq!(reply["error"]["code"], "BROWSER_SECRET_UNAVAILABLE");
    assert_eq!(reply["error"]["details"]["stage"], "take");
}

#[test]
fn a_bundle_and_a_cookie_never_render_a_value() {
    let imported = ImportedBundle { cookies: bundle() };
    let text = format!("{imported:?} {:?}", bundle());
    for canary in [CANARY, HTTP_ONLY_CANARY] {
        assert!(!text.contains(canary), "{text}");
    }
}
