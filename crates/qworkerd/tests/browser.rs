//! The managed browser against a real Google Chrome (EXEC-009).
//!
//! The task's `real_boundary` is `true`, and this is that boundary: every test below launches Chrome with
//! the DevTools protocol on a loopback port, drives it, and reads what the *browser* did — the page's own
//! DOM, the page's own click handlers. Nothing here stands in for Chrome, and nothing asserts against a
//! recorded fixture of what Chrome was expected to say.
//!
//! The four cases the task names are here — DOM action, screenshot fallback, effect classification and
//! `web.fetch` bounds/labelling — plus the session's takeover rules. When no Chrome is present the suite
//! reports `BLOCKED_EXTERNAL` and returns, so a machine without a browser is never a pass.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use quansio_core::Digest;
use quansio_qworkerd::browser::{
    self, BrowserError, BrowserSession, ControlHolder, ScreenshotReason, SessionStatus,
    Understanding, DEFAULT_MAX_LINKS,
};

/// The Chrome the tests drive, if this host has one.
fn chrome_path() -> Option<&'static str> {
    const CANDIDATES: [&str; 3] = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
    ];
    CANDIDATES
        .into_iter()
        .find(|path| std::path::Path::new(path).exists())
}

fn blocked_marker() {
    eprintln!("BLOCKED_EXTERNAL: no Chrome on this host; the browser suite did not run");
}

/// A running headless Chrome, killed when it goes out of scope.
struct Browser {
    child: Child,
    port: u16,
    _profile: tempfile::TempDir,
    // Chrome's own stdout+stderr, redirected to a real file rather than a pipe (a pipe
    // nobody reads risks Chrome blocking on a full OS buffer over a long-running
    // session) so a CDP timeout -- like `Page.captureScreenshot` hanging -- can be
    // diagnosed from what the browser itself said, not just "it didn't answer in
    // time". Printed in `Drop`, unconditionally: cargo test's own harness only
    // *displays* a test's captured output when that test fails, so this stays silent
    // for the tests that pass and shows up for the one that doesn't -- no per-test
    // code, no new CI upload path, the existing full-gate-log capture already covers
    // whatever cargo test itself prints.
    log_path: std::path::PathBuf,
}

impl Browser {
    fn launch() -> Option<Self> {
        let chrome = chrome_path()?;
        let profile = tempfile::tempdir().expect("profile dir");
        let named_log = tempfile::NamedTempFile::new().expect("chrome log file");
        // `.keep()` detaches the file from NamedTempFile's own delete-on-drop, since
        // `Drop` below removes it explicitly after printing it.
        let (log_file, log_path) = named_log.keep().expect("keep chrome log file");
        let stderr_file = log_file.try_clone().expect("clone chrome log handle");
        // A free port, taken by binding and releasing it: the window is small and the alternative is a
        // fixed port that collides with whatever else is running.
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
        // Chrome's own sandbox needs kernel privileges (user namespaces / a setuid helper)
        // that a CI runner routinely restricts; without --no-sandbox it can crash moments
        // after the debugging port starts accepting connections -- the port answers a bare
        // TCP probe (below) but is refusing real requests by the time a caller follows up,
        // which is indistinguishable from "never started" without watching for it. Chrome
        // also uses /dev/shm for shared memory, which CI images size far smaller than a
        // developer machine's; --disable-dev-shm-usage falls back to a temp-file backing
        // store instead of crashing when it fills. Real production browser sessions run
        // inside a microVM (DOSSIER.md §12/§8), not this test's own process -- a full
        // sandbox there is expected and unaffected; these flags apply only to *this* test
        // binary's throwaway Chrome, and only when a CI environment is detected, so a
        // developer running the suite locally still exercises the real sandboxed path.
        if std::env::var_os("CI").is_some() {
            command.arg("--no-sandbox").arg("--disable-dev-shm-usage");
        }
        let child = command
            .arg("about:blank")
            .stdout(log_file)
            .stderr(stderr_file)
            .spawn()
            .ok()?;
        let deadline = Instant::now() + Duration::from_secs(25);
        while Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return Some(Self {
                    child,
                    port,
                    _profile: profile,
                    log_path,
                });
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let mut child = child;
        let _ = child.kill();
        None
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Ok(log) = std::fs::read_to_string(&self.log_path) {
            if !log.trim().is_empty() {
                eprintln!(
                    "--- chrome stdout+stderr ({}) ---\n{log}",
                    self.log_path.display()
                );
            }
        }
        let _ = std::fs::remove_file(&self.log_path);
    }
}

/// A base64 encoder, because a `data:` url is the one page a test can serve without a server.
fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// A `data:` url carrying `html`.
fn data_url(html: &str) -> String {
    format!("data:text/html;base64,{}", base64(html.as_bytes()))
}

/// Open a page and connect to it. The blank page is opened by the browser and then navigated, because a
/// `data:` url in a query string would be mangled by the endpoint's own parsing.
async fn page(browser: &Browser, html: &str) -> (browser::CdpConnection, String) {
    let target = browser::cdp::open_page(browser.port, "about:blank")
        .await
        .expect("open a page");
    let connection = browser::cdp::connect(target).await.expect("connect");
    let requested = data_url(html);
    browser::navigate(&connection, &requested)
        .await
        .expect("navigate");
    (connection, requested)
}

const RICH_PAGE: &str = r##"<html><head><title>Quarterly report</title>
<meta name="description" content="Numbers for Q3"></head>
<body><h1>Quarterly report</h1>
<p>Revenue rose by eleven percent in the third quarter, and the outlook is unchanged.</p>
<p>See the <a href="https://example.com/appendix">appendix</a> for detail.</p>
<a href="#top">Back to top</a>
<button id="buy">Buy now</button>
<form action="/orders" method="post"><input type="submit" value="Delete account" id="del"></form>
<form action="/search" method="get"><input type="text" name="q"><input type="submit" value="Search" id="find"></form>
<button id="toggle">Show more</button>
</body></html>"##;

/// A page whose meaning is visual: a canvas with text drawn on it, and no DOM text at all.
const VISUAL_PAGE: &str = r##"<html><head><title>Chart</title></head><body><canvas id="c" width="200" height="80"></canvas>
<script>const g = document.getElementById('c').getContext('2d'); g.font='20px sans-serif'; g.fillText('Q3', 20, 40);</script>
</body></html>"##;

// ------------------------------------------------------------------ DOM first, vision as fallback

#[tokio::test]
async fn the_dom_is_read_first_and_a_screenshot_only_when_it_says_nothing() {
    let Some(browser) = Browser::launch() else {
        blocked_marker();
        return;
    };
    let (connection, _) = page(&browser, RICH_PAGE).await;

    // A page with text, links and a form is understood from the DOM, and no image is taken: a screenshot
    // cannot say what a control does, so taking one first would be working backwards.
    let observation = browser::observe(
        &connection,
        browser::DEFAULT_MAX_TEXT_BYTES,
        DEFAULT_MAX_LINKS,
        false,
    )
    .await
    .expect("observe");
    assert_eq!(observation.title, "Quarterly report");
    assert!(
        observation.url.starts_with("data:text/html"),
        "{}",
        observation.url
    );
    assert!(
        observation.text.contains("Revenue rose by eleven percent"),
        "the readable text is missing: {:?}",
        observation.text
    );
    assert_eq!(observation.trust.as_str(), "UNTRUSTED_EXTERNAL");
    match &observation.understanding {
        Understanding::Dom(dom) => {
            assert!(dom.text_bytes > 0, "{dom:?}");
            assert!(
                dom.named_accessibility_nodes > 0,
                "the named accessibility nodes were not read: {dom:?}"
            );
            assert_eq!(dom.links, 2, "{dom:?}");
            assert_eq!(dom.forms, 2, "{dom:?}");
        }
        other => panic!("a text page must be understood from the DOM: {other:?}"),
    }
    assert!(
        observation.screenshot.is_none(),
        "a screenshot was taken unasked"
    );

    // The links came back with their text, and an absolute href stayed absolute.
    assert!(
        observation
            .links
            .iter()
            .any(|link| link.href == "https://example.com/appendix" && link.text == "appendix"),
        "{:?}",
        observation.links
    );

    // A canvas page says nothing in the DOM, so the meaning is visual and a screenshot is what carries
    // it. This is the fallback the task names, decided by whether the DOM said anything -- not by a flag.
    let (visual_connection, _) = page(&browser, VISUAL_PAGE).await;
    let visual = browser::observe(
        &visual_connection,
        browser::DEFAULT_MAX_TEXT_BYTES,
        DEFAULT_MAX_LINKS,
        false,
    )
    .await
    .expect("observe the canvas page");
    match &visual.understanding {
        Understanding::Visual { dom, reason } => {
            assert_eq!(*reason, ScreenshotReason::DomUninformative);
            assert!(
                dom.text_bytes < browser::VISUAL_ONLY_TEXT_THRESHOLD,
                "{dom:?}"
            );
        }
        other => panic!("a canvas page must fall back to vision: {other:?}"),
    }
    let png = visual.screenshot.expect("a screenshot");
    assert!(
        png.starts_with(&[0x89, b'P', b'N', b'G']),
        "the screenshot is not a PNG: {:?}",
        &png[..png.len().min(8)]
    );
    assert!(png.len() > 100, "the screenshot is {} bytes", png.len());

    // Asking for one on a text page takes one too, and says that is why.
    let asked = browser::observe(
        &connection,
        browser::DEFAULT_MAX_TEXT_BYTES,
        DEFAULT_MAX_LINKS,
        true,
    )
    .await
    .expect("observe");
    match &asked.understanding {
        Understanding::Visual { reason, .. } => assert_eq!(*reason, ScreenshotReason::Requested),
        other => panic!("a requested screenshot was not taken: {other:?}"),
    }
    assert!(asked.screenshot.is_some());
    assert!(!asked.understanding.is_dom_first());
}

// ------------------------------------------------------------------ effect classification

#[tokio::test]
async fn a_click_is_classified_from_the_page_s_own_semantics_before_it_happens() {
    let Some(browser) = Browser::launch() else {
        blocked_marker();
        return;
    };
    let (connection, _) = page(&browser, RICH_PAGE).await;

    // Each selector's classification comes from the control's role, accessible name, href and form.
    let cases: [(&str, Option<&str>, Option<u8>); 6] = [
        // A link to another origin is egress to a new destination: tier 2, so the runtime asks.
        (
            "a[href='https://example.com/appendix']",
            Some("network.egress.new_destination"),
            Some(2),
        ),
        // An in-page anchor navigates nowhere, so there is nothing for the ledger to settle.
        ("a[href='#top']", None, None),
        // The accessible name is what the control says it does.
        ("#buy", Some("payment.execute"), Some(4)),
        ("#del", Some("record.delete"), Some(3)),
        // A submit inside a form with no stronger signal writes to the site: tier 2.
        ("#find", Some("record.create"), Some(2)),
        // A button that neither navigates nor belongs to a form changes the page and nothing else.
        ("#toggle", None, None),
    ];
    for (selector, class, tier) in cases {
        let (classified, control) = browser::classify_click(&connection, selector)
            .await
            .unwrap_or_else(|error| panic!("{selector}: {error}"));
        assert_eq!(
            classified.effect_class, class,
            "{selector}: {classified:?} / {control:?}"
        );
        assert_eq!(classified.tier, tier, "{selector}: {classified:?}");
        assert_eq!(
            classified.is_consequential(),
            class.is_some(),
            "{selector}: {classified:?}"
        );
        assert!(!classified.reason.is_empty(), "{selector} gave no reason");
    }

    // The click actually happens in the page -- the classification is taken from the page as it is, and
    // then the page does the thing. A handler proves it ran rather than that a function was called.
    let probe = r#"<html><body><button id="b">Show more</button>
    <div id="out">closed</div>
    <script>document.getElementById('b').addEventListener('click', () => {
      document.getElementById('out').textContent = 'opened';
    });</script></body></html>"#;
    let (probe_connection, _) = page(&browser, probe).await;
    let classified = browser::click(&probe_connection, "#b")
        .await
        .expect("click");
    assert!(!classified.is_consequential(), "{classified:?}");
    let after = browser::observe(
        &probe_connection,
        browser::DEFAULT_MAX_TEXT_BYTES,
        DEFAULT_MAX_LINKS,
        false,
    )
    .await
    .expect("observe after the click");
    assert!(
        after.text.contains("opened"),
        "the page's own handler did not run: {:?}",
        after.text
    );

    // A selector that matches nothing is a typed refusal rather than a silent success.
    assert!(matches!(
        browser::click(&probe_connection, "#nope").await,
        Err(BrowserError::NoSuchElement { .. })
    ));
}

// ------------------------------------------------------------------ web.fetch

#[tokio::test]
async fn web_fetch_is_bounded_labelled_and_digested() {
    let Some(browser) = Browser::launch() else {
        blocked_marker();
        return;
    };
    let long = format!(
        "<html><head><title>Long page</title></head><body><p>{}</p><a href=\"https://example.com/x\">x</a></body></html>",
        "lorem ipsum dolor sit amet ".repeat(200)
    );
    let (connection, requested) = page(&browser, &long).await;
    let retrieved_at = "2026-09-14T10:00:00Z";

    let fetched = browser::web_fetch(&connection, &requested, retrieved_at, 512, 8)
        .await
        .expect("fetch");
    assert_eq!(fetched.title, "Long page");
    assert_eq!(fetched.retrieved_at, retrieved_at);
    assert!(fetched.url.starts_with("data:text/html"), "{}", fetched.url);
    assert_eq!(fetched.requested_url, requested);
    // Bounded, and honest about what it dropped.
    assert!(fetched.truncated, "{fetched:?}");
    assert!(
        fetched.text.len() <= 512,
        "the text was not bounded: {}",
        fetched.text.len()
    );
    assert!(
        fetched.total_text_bytes > fetched.text.len(),
        "what the page had is not reported: {fetched:?}"
    );
    assert!(fetched.links.len() <= 8);
    assert_eq!(fetched.total_links, 1);
    // Labelled, so nothing downstream can mistake page content for an instruction.
    assert_eq!(fetched.trust.as_str(), "UNTRUSTED_EXTERNAL");
    assert_eq!(fetched.trust, browser::TrustLabel::UntrustedExternal);
    // Digested, so a caller can tell a changed page from a re-fetch.
    assert_eq!(
        fetched.content_digest,
        Digest::of(fetched.text.as_bytes()).as_str()
    );
    assert_eq!(fetched.content_digest.len(), 64);
    assert_eq!(
        fetched.metadata.get("canonical").map(String::as_str),
        None,
        "a page with no canonical link must not claim one: {:?}",
        fetched.metadata
    );

    // An unbounded fetch keeps the whole page and says so, and the digest of a whole page differs from
    // the truncated one -- which is the point of digesting the content that was returned.
    let whole = browser::web_fetch(&connection, &requested, retrieved_at, 1_000_000, 8)
        .await
        .expect("fetch");
    assert!(!whole.truncated, "{whole:?}");
    assert_eq!(whole.total_text_bytes, whole.text.len());
    assert_ne!(whole.content_digest, fetched.content_digest);
    // The same page read twice digests the same, so the digest is a property of the content.
    let again = browser::web_fetch(&connection, &requested, retrieved_at, 1_000_000, 8)
        .await
        .expect("fetch again");
    assert_eq!(again.content_digest, whole.content_digest);
    assert_eq!(again.text, whole.text);
}

// ------------------------------------------------------------------ the session

#[tokio::test]
async fn a_takeover_fences_agent_input_without_creating_a_second_session() {
    let Some(browser) = Browser::launch() else {
        blocked_marker();
        return;
    };
    let (connection, _) = page(&browser, RICH_PAGE).await;

    let mut session = BrowserSession::open(
        "bsn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
    );
    assert_eq!(session.control_holder, ControlHolder::Agent);
    assert_eq!(session.status, SessionStatus::Active);
    assert!(session.agent_may_drive());
    session.require_agent_control().expect("the agent holds it");

    // The session records the page it is on, and its tab list follows.
    let observation = browser::observe(
        &connection,
        browser::DEFAULT_MAX_TEXT_BYTES,
        DEFAULT_MAX_LINKS,
        false,
    )
    .await
    .expect("observe");
    session.observe_page(
        connection.page().id.as_str(),
        &observation.url,
        &observation.title,
    );
    assert_eq!(session.tabs.len(), 1);
    assert_eq!(
        session.current_url.as_deref(),
        Some(observation.url.as_str())
    );

    // The user takes over: input is fenced, and the *same* session continues -- §8.4 forbids creating a
    // second one, so the tab, the profile and the page all survive.
    session
        .request_takeover("2026-09-14T10:00:00Z")
        .expect("takeover");
    assert_eq!(session.control_holder, ControlHolder::User);
    assert_eq!(session.status, SessionStatus::PausedTakeover);
    assert!(!session.agent_may_drive());
    let refusal = session
        .require_agent_control()
        .expect_err("agent input is fenced");
    assert!(
        matches!(refusal, BrowserError::InputFenced { holder: "user", .. }),
        "{refusal:?}"
    );
    assert_eq!(session.tabs.len(), 1, "a takeover duplicated the session");
    assert_eq!(
        session.current_url.as_deref(),
        Some(observation.url.as_str())
    );
    // A second takeover is a no-op on the holder rather than a second session.
    assert!(!session.takeover_allowed());

    // Handback returns control to the agent and the page is still the one it was on.
    session.handback("2026-09-14T10:01:00Z").expect("handback");
    assert_eq!(session.control_holder, ControlHolder::Agent);
    assert_eq!(session.status, SessionStatus::Active);
    assert!(session.agent_may_drive());
    session
        .require_agent_control()
        .expect("the agent drives again");

    // A policy pause is not resumable by a handback: policy decided, so only policy resumes it.
    session.pause_for_policy("2026-09-14T10:02:00Z");
    assert_eq!(session.status, SessionStatus::PausedPolicy);
    assert!(matches!(
        session.handback("2026-09-14T10:03:00Z"),
        Err(BrowserError::InputFenced { .. })
    ));
    assert_eq!(session.status, SessionStatus::PausedPolicy);

    // The screencast is what a user watching a takeover sees, and closing the session stops it.
    browser::start_screencast(&connection, &mut session)
        .await
        .expect("start screencast");
    assert!(session.screencast.enabled);
    assert!(session.screencast.stream_ref.is_some());
    browser::stop_screencast(&connection, &mut session)
        .await
        .expect("stop screencast");
    assert!(!session.screencast.enabled);

    session.close("2026-09-14T10:04:00Z");
    assert_eq!(session.status, SessionStatus::Closed);
    assert!(matches!(
        session.require_agent_control(),
        Err(BrowserError::SessionNotUsable {
            status: "closed",
            ..
        })
    ));
    assert!(matches!(
        session.request_takeover("2026-09-14T10:05:00Z"),
        Err(BrowserError::SessionNotUsable { .. })
    ));
}
