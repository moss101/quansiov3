//! Isolated document rendering against a real Google Chrome (ADP-006, DOMAIN.md §10.4 B and C).
//!
//! `real_boundary` is true, and this is that boundary for the render path: every test launches
//! Chrome with the DevTools protocol on a loopback port, renders a document through
//! `browser::render`, and reads what the *browser* did: the laid-out page's measurements, its
//! screenshot, the PDF it printed, and the hits a real HTTP server on loopback received. When no
//! Chrome is present the suite reports `BLOCKED_EXTERNAL` and returns, so a host without a browser
//! is never a pass.
//!
//! PDF content is checked by an independent reader, `tests/support/pdf_text.py` (zlib and the
//! font maps, no browser), which needs `python3` on the PATH; without it that one test reports
//! `BLOCKED_EXTERNAL` and returns.

use std::collections::BTreeMap;
use std::time::Duration;

use quansio_qworkerd::browser::render::{
    self, Measure, PdfJob, PinnedAsset, RenderError, RenderRequest, SlideLimits,
};
use serde_json::Value;

mod common;
use common::{blocked, Browser, Site};

const LIMITS: SlideLimits = SlideLimits {
    overflow_tolerance_px: 1.0,
    min_contrast_text: 4.5,
    min_contrast_large: 3.0,
};

const CSP: &str = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; \
    img-src data: https://assets.quansio.invalid; style-src 'unsafe-inline' https://assets.quansio.invalid\">";

fn slide(index: usize, inner: &str) -> String {
    format!(
        "<section class=\"slide\" data-slide=\"{index}\" style=\"width:1280px;height:720px;overflow:hidden;\
         background:#ffffff;color:#111111;font:32px Helvetica,Arial,sans-serif;box-sizing:border-box;padding:48px\">{inner}</section>"
    )
}

fn document(body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">{CSP}<style>html,body{{margin:0;padding:0}}\
         @page{{size:1280px 720px;margin:0}}.slide{{page-break-after:always}}</style></head><body>{body}</body></html>"
    )
}

fn request(html: String) -> RenderRequest {
    RenderRequest {
        html,
        assets: BTreeMap::new(),
        viewport: (1280, 720),
        measure: Measure::Slides(LIMITS),
        pdf: None,
        screenshot: false,
        max_screenshot_bytes: 4 * 1024 * 1024,
        deadline: Duration::from_secs(60),
    }
}

fn slide_report(output: &render::RenderOutput, at: usize) -> Value {
    output.measurements["slides"][at].clone()
}

fn png_size(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    let width = u32::from_be_bytes(png[16..20].try_into().expect("width"));
    let height = u32::from_be_bytes(png[20..24].try_into().expect("height"));
    (width, height)
}

fn page_count(pdf: &[u8]) -> usize {
    let text = String::from_utf8_lossy(pdf);
    text.matches("/Type /Page\n")
        .count()
        .max(text.matches("/Type /Page ").count())
        .max(text.matches("/Type /Page>").count())
        .max(text.matches("/Type /Page/").count())
}

#[tokio::test]
async fn a_slide_is_laid_out_measured_and_photographed_by_the_browser() {
    let Some(browser) = Browser::launch() else {
        return blocked("no Chrome on this host");
    };
    let mut req = request(document(&slide(
        0,
        "<h1 style=\"margin:0\">Quarterly review</h1><p>Revenue grew in every region.</p>",
    )));
    req.screenshot = true;
    let output = render::render(browser.port, &req).await.expect("render");

    let report = slide_report(&output, 0);
    assert_eq!(report["index"], "0");
    assert_eq!(
        (report["width"].as_u64(), report["height"].as_u64()),
        (Some(1280), Some(720))
    );
    assert!(report["text"]
        .as_str()
        .expect("text")
        .contains("Quarterly review"));
    assert!(report["text"]
        .as_str()
        .expect("text")
        .contains("Revenue grew in every region."));
    assert_eq!(report["overflow"]["count"], 0, "{report}");
    assert_eq!(
        report["contrast"]["failures"].as_array().map(Vec::len),
        Some(0),
        "{report}"
    );
    assert!(report["contrast"]["checked"].as_u64().unwrap_or(0) >= 2);
    assert_eq!(report["missing_alt"].as_array().map(Vec::len), Some(0));
    assert!(output.blocked.is_empty(), "{:?}", output.blocked);
    assert!(output.browser.contains("Chrome"), "{}", output.browser);

    let png = output.screenshot.expect("screenshot");
    assert_eq!(png_size(&png), (1280, 720));
    assert!(png.len() > 1000, "a real picture is not a few bytes");
}

#[tokio::test]
async fn content_that_does_not_fit_is_measured_as_overflowing() {
    let Some(browser) = Browser::launch() else {
        return blocked("no Chrome on this host");
    };
    let many: String = (0..40)
        .map(|n| {
            format!(
                "<p style=\"margin:0 0 8px\">Line number {n} of a body that is far too long</p>"
            )
        })
        .collect();
    let output = render::render(browser.port, &request(document(&slide(0, &many))))
        .await
        .expect("render");
    let report = slide_report(&output, 0);
    assert!(
        report["overflow"]["count"].as_u64().unwrap_or(0) > 0,
        "{report}"
    );

    // The control: the same slide with a body that fits measures clean in the same browser.
    let fits = render::render(
        browser.port,
        &request(document(&slide(0, "<p style=\"margin:0\">Short.</p>"))),
    )
    .await
    .expect("render");
    assert_eq!(slide_report(&fits, 0)["overflow"]["count"], 0);
}

#[tokio::test]
async fn text_that_is_hard_to_read_and_pictures_without_a_name_are_measured() {
    let Some(browser) = Browser::launch() else {
        return blocked("no Chrome on this host");
    };
    let pixel = "data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==";
    let inner = format!(
        "<p style=\"color:#bbbbbb;margin:0\">pale grey on white</p>\
         <img src=\"{pixel}\" width=\"40\" height=\"40\">\
         <img src=\"{pixel}\" width=\"40\" height=\"40\" alt=\"A single pixel, enlarged\">\
         <img src=\"{pixel}\" width=\"40\" height=\"40\" alt=\"\" role=\"presentation\">"
    );
    let output = render::render(browser.port, &request(document(&slide(0, &inner))))
        .await
        .expect("render");
    let report = slide_report(&output, 0);
    let failures = report["contrast"]["failures"].as_array().expect("failures");
    assert_eq!(failures.len(), 1, "{report}");
    assert!(failures[0]["ratio"].as_f64().unwrap_or(99.0) < 4.5);
    assert_eq!(failures[0]["text"], "pale grey on white");
    // Only the first image has no accessible name; the named one and the decorative one pass.
    assert_eq!(
        report["missing_alt"].as_array().map(Vec::len),
        Some(1),
        "{report}"
    );
}

#[tokio::test]
async fn an_empty_slide_has_no_text_and_no_figure() {
    let Some(browser) = Browser::launch() else {
        return blocked("no Chrome on this host");
    };
    let output = render::render(browser.port, &request(document(&slide(0, ""))))
        .await
        .expect("render");
    let report = slide_report(&output, 0);
    assert_eq!(report["text_chars"], 0);
    assert_eq!(report["figures"], 0);
}

#[tokio::test]
async fn the_page_reaches_neither_the_network_nor_the_disk_and_the_refusals_are_recorded() {
    let Some(browser) = Browser::launch() else {
        return blocked("no Chrome on this host");
    };
    let site = Site::start().await;
    site.route("/hit.png", common::Route::Count);
    site.route("/hit.css", common::Route::Count);
    // A stylesheet on this machine's own disk that would hide the probe if it were ever read.
    let local = tempfile::tempdir().expect("dir");
    let local_css = local.path().join("hide.css");
    std::fs::write(&local_css, ".probe{display:none}").expect("css");
    // No content-security-policy here: the browser-level refusal must hold on its own.
    let html = format!(
        "<!doctype html><html><head><link rel=\"stylesheet\" href=\"{css}\">\
         <link rel=\"stylesheet\" href=\"file://{local}\"></head><body>\
         <section data-slide=\"0\"><p>hello</p><p class=\"probe\">probe</p><img src=\"{img}\" alt=\"remote\">\
         </section></body></html>",
        css = site.url("/hit.css"),
        img = site.url("/hit.png"),
        local = local_css.display(),
    );
    let output = render::render(browser.port, &request(html))
        .await
        .expect("render");
    assert_eq!(
        site.hits("/hit.png"),
        0,
        "an image was fetched from the network"
    );
    assert_eq!(
        site.hits("/hit.css"),
        0,
        "a stylesheet was fetched from the network"
    );
    let seen: Vec<(&str, &str)> = output
        .blocked
        .iter()
        .map(|b| (b.scheme.as_str(), b.resource_type.as_str()))
        .collect();
    assert!(seen.contains(&("http", "Stylesheet")), "{seen:?}");
    assert!(seen.contains(&("http", "Image")), "{seen:?}");
    assert!(output.text.contains("hello"));
    assert!(
        output.text.contains("probe"),
        "a stylesheet on the local disk was read"
    );
}

#[tokio::test]
async fn only_a_pinned_asset_is_served_and_only_from_its_own_host() {
    let Some(browser) = Browser::launch() else {
        return blocked("no Chrome on this host");
    };
    let mut req = request(format!(
        "<!doctype html><html><head><link rel=\"stylesheet\" href=\"https://assets.quansio.invalid/hide.css\">\
         <link rel=\"stylesheet\" href=\"https://assets.quansio.invalid/unpinned.css\"></head><body>\
         <section data-slide=\"0\"><p class=\"probe\">visible only without the pinned css</p>\
         <p class=\"other\">stays</p></section></body></html>"
    ));
    req.assets.insert(
        "hide.css".to_string(),
        PinnedAsset {
            media_type: "text/css".to_string(),
            bytes: b".probe{display:none}".to_vec(),
        },
    );
    let output = render::render(browser.port, &req).await.expect("render");
    assert!(
        !output.text.contains("visible only without"),
        "the pinned stylesheet was not applied"
    );
    assert!(output.text.contains("stays"));
    // The unpinned name on the pinned host is refused and recorded.
    assert_eq!(output.blocked.len(), 1, "{:?}", output.blocked);
    assert_eq!(output.blocked[0].host, "assets.quansio.invalid");

    let bad = RenderError::AssetName("../x".to_string());
    let mut with_bad_name = request("<p>x</p>".to_string());
    with_bad_name.assets.insert(
        "../x".to_string(),
        PinnedAsset {
            media_type: "text/css".to_string(),
            bytes: Vec::new(),
        },
    );
    let refused = render::render(browser.port, &with_bad_name)
        .await
        .expect_err("bad name");
    assert_eq!(refused.to_string(), bad.to_string());
}

#[tokio::test]
async fn a_script_in_the_document_does_not_run() {
    let Some(browser) = Browser::launch() else {
        return blocked("no Chrome on this host");
    };
    // No content-security-policy: scripts are disabled in the renderer itself.
    let output = render::render(
        browser.port,
        &RenderRequest {
            measure: Measure::Text,
            ..request(
                "<!doctype html><html><body><p id=\"x\">before</p>\
                 <script>document.getElementById('x').textContent='after';</script>\
                 <img src=\"data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==\" \
                 onload=\"document.getElementById('x').textContent='after'\"></body></html>"
                    .to_string(),
            )
        },
    )
    .await
    .expect("render");
    assert!(output.text.contains("before"), "{:?}", output.text);
    assert!(!output.text.contains("after"));
}

#[tokio::test]
async fn a_deck_prints_one_pdf_page_per_slide_and_the_text_is_in_it() {
    let Some(browser) = Browser::launch() else {
        return blocked("no Chrome on this host");
    };
    let body = (0..3)
        .map(|n| {
            slide(
                n,
                &format!(
                    "<h1 style=\"margin:0\">Slide number {n}</h1><p>Finding {n} is sound.</p>"
                ),
            )
        })
        .collect::<String>();
    let mut req = request(document(&body));
    req.pdf = Some(PdfJob {
        paper_width_in: 13.333,
        paper_height_in: 7.5,
        margin_in: 0.0,
        prefer_css_page_size: true,
        max_bytes: 8 * 1024 * 1024,
    });
    let output = render::render(browser.port, &req).await.expect("render");
    let pdf = output.pdf.expect("pdf");
    assert!(pdf.starts_with(b"%PDF-1."), "{:?}", &pdf[..8]);
    assert!(String::from_utf8_lossy(&pdf[pdf.len().saturating_sub(32)..]).contains("%%EOF"));
    assert_eq!(page_count(&pdf), 3, "one page per slide");
    assert_eq!(output.measurements["slide_roots"], 3);

    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("deck.pdf");
    std::fs::write(&path, &pdf).expect("write");
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/pdf_text.py");
    let Ok(extracted) = std::process::Command::new("python3")
        .arg(&script)
        .arg(&path)
        .output()
    else {
        return blocked("no python3 for the independent PDF reader");
    };
    assert!(
        extracted.status.success(),
        "{}",
        String::from_utf8_lossy(&extracted.stderr)
    );
    let text = String::from_utf8_lossy(&extracted.stdout).into_owned();
    let pages: Vec<&str> = text.split('\u{c}').collect();
    for n in 0..3 {
        assert!(
            pages[n].contains(&format!("Slide number {n}"))
                && pages[n].contains(&format!("Finding {n} is sound.")),
            "page {n} says {:?}",
            pages[n]
        );
    }
}

#[tokio::test]
async fn an_output_over_its_bound_is_refused_and_the_context_is_disposed_of() {
    let Some(browser) = Browser::launch() else {
        return blocked("no Chrome on this host");
    };
    let before = browser.page_count().await;
    let mut req = request(document(&slide(0, "<p>x</p>")));
    req.pdf = Some(PdfJob {
        paper_width_in: 13.333,
        paper_height_in: 7.5,
        margin_in: 0.0,
        prefer_css_page_size: true,
        max_bytes: 64,
    });
    let refused = render::render(browser.port, &req)
        .await
        .expect_err("too large");
    assert!(
        matches!(refused, RenderError::TooLarge { what: "PDF", .. }),
        "{refused:?}"
    );

    req.pdf = None;
    req.deadline = Duration::from_millis(1);
    let late = render::render(browser.port, &req)
        .await
        .expect_err("deadline");
    assert!(matches!(late, RenderError::Deadline(_)), "{late:?}");

    // Whatever happened, no page of the render's context is left open.
    let settled = common::eventually(Duration::from_secs(10), || async {
        browser.page_count().await == before
    })
    .await;
    assert!(
        settled,
        "{} pages open, {before} before",
        browser.page_count().await
    );
}

#[tokio::test]
async fn a_render_gets_its_own_browser_context_and_leaves_none_behind() {
    let Some(browser) = Browser::launch() else {
        return blocked("no Chrome on this host");
    };
    use quansio_qworkerd::browser::cdp;
    let socket = cdp::discover(browser.port).await.expect("discover");
    let control = cdp::connect(cdp::PageTarget {
        id: "browser".to_string(),
        title: String::new(),
        url: String::new(),
        websocket_url: socket,
    })
    .await
    .expect("connect");
    let contexts = || async {
        control
            .call("Target.getBrowserContexts", serde_json::json!({}))
            .await
            .expect("contexts")["browserContextIds"]
            .as_array()
            .map_or(0, Vec::len)
    };
    let before = contexts().await;
    for _ in 0..3 {
        render::render(
            browser.port,
            &RenderRequest {
                measure: Measure::Text,
                ..request("<!doctype html><body><p>x</p></body>".to_string())
            },
        )
        .await
        .expect("render");
    }
    assert_eq!(
        contexts().await,
        before,
        "a render left a browser context behind"
    );
}
