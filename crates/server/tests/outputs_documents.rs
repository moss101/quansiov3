//! Printed documents and diagrams (ADP-006, DOMAIN.md §10.4 C) against a real PostgreSQL database, the
//! development MinIO, the server's real tool dispatcher and a real headless Google Chrome.
//!
//! * the Markdown the plane rendered is printed by the managed browser in the render sandbox, the PDF is
//!   read back by an independent reader, and the words on the printed pages are the source's;
//! * markup outside the allowlist, a page that leaves out the source, and a missing render host are each
//!   refused with the rule or the code that names them, and nothing is stored;
//! * a diagram's SVG is accepted only when it draws the nodes and edges of the source it was made from
//!   and carries nothing a picture should not.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`, `QUANSIO_TEST_MINIO_*` (see `tests/artifacts.rs`) and a
//! Google Chrome; `BLOCKED_EXTERNAL` is printed, and the test returns, without them.

use std::path::PathBuf;

use quansio_server::artifacts::outputs::shapes::DIAGRAM_MEDIA_TYPE;
use quansio_server::artifacts::outputs::svg::{self, DiagramIr, DiagramLimits};
use quansio_server::artifacts::outputs::{document, html};
use serde_json::{json, Value};

mod common;
use common::blocked_marker;
use common::outputs::{content_args, Chrome, ChromeRender, Harness};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn text(dir: &str, name: &str) -> String {
    std::fs::read_to_string(root().join("packs/skills/artifacts").join(dir).join(name))
        .expect("file")
}

fn blocked(reason: &str) {
    eprintln!("BLOCKED_EXTERNAL: {reason}; the test did not run");
}

const LIMITS: DiagramLimits = DiagramLimits {
    max_nodes: 200,
    max_edges: 400,
    svg_max_bytes: 1 << 20,
};

// ---------------------------------------------------------------------------------------------
// What the plane builds is what the authority accepts (no database, no browser)
// ---------------------------------------------------------------------------------------------

#[test]
fn the_documents_and_diagrams_the_plane_builds_pass_the_authoritys_checks() {
    let body = text("golden", "document-rich.body.html");
    let markup = html::check_markup(&body).expect("the rendered document is inside the allowlist");
    assert!(markup.elements > 20);
    let source = text("fixtures", "document-rich.md");
    let words = document::markdown_words(&source);
    assert!(words.len() > 40);
    // Coverage against the source's own words, from the plane's HTML text alone (tags removed).
    let mut flat = String::new();
    let mut in_tag = false;
    for c in body.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                flat.push(' ');
            }
            c if !in_tag => flat.push(c),
            _ => {}
        }
    }
    let flat = flat
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    assert!(document::coverage(&words, &document::words(&flat)) >= 0.98);

    let release =
        DiagramIr::parse(text("golden", "diagram-release.ir.json").as_bytes(), LIMITS).expect("ir");
    let checks = svg::verify(&text("golden", "diagram-release.svg"), &release, LIMITS);
    assert_eq!(checks.len(), 3);
    assert!(checks.iter().all(|check| check.ok), "{checks:#?}");
    for (ir_file, svg_file) in [
        ("diagram-sequence.json", "diagram-sequence.svg"),
        ("diagram-tree.json", "diagram-tree.svg"),
    ] {
        let ir = DiagramIr::parse(text("fixtures", ir_file).as_bytes(), LIMITS).expect("ir");
        let checks = svg::verify(&text("golden", svg_file), &ir, LIMITS);
        assert!(
            checks.iter().all(|check| check.ok),
            "{svg_file}: {checks:#?}"
        );
    }
}

#[test]
fn markup_outside_the_allowlist_never_reaches_the_sandbox() {
    for (body, rule) in [
        ("<h1>T</h1><script>alert(1)</script>", "output.html.tag"),
        (
            "<p>hi</p><iframe src=\"https://evil.example\"></iframe>",
            "output.html.tag",
        ),
        ("<p onclick=\"x()\">hi</p>", "output.html.attribute"),
        (
            "<img src=\"https://evil.example/t.png\" alt=\"t\">",
            "output.html.url",
        ),
        (
            "<p style=\"background:url(https://evil.example/t.png)\">hi</p>",
            "output.html.url",
        ),
        ("<a href=\"https://evil.example\">x</a>", "output.html.tag"),
        (
            "<form action=\"https://evil.example\"><input></form>",
            "output.html.tag",
        ),
        (
            "<!-- <script>x</script> --><p>hi</p>",
            "output.html.structure",
        ),
    ] {
        assert_eq!(
            html::check_markup(body).expect_err(body).rule,
            rule,
            "{body}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Printing
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_document_is_printed_by_the_managed_browser_and_the_printed_text_is_the_documents() {
    let Some(chrome) = Chrome::launch() else {
        return blocked("no Chrome on this host");
    };
    let render = ChromeRender::new(chrome.port);
    let Some(h) = Harness::prepare("adp006_doc", |host| host.with_render(render.clone())).await
    else {
        blocked_marker();
        return;
    };
    let source = text("fixtures", "document-rich.md");
    let body = text("golden", "document-rich.body.html");
    let document_id = h
        .create(
            "document",
            "report",
            "Quarterly operations review",
            "text/markdown",
            source.as_bytes(),
        )
        .await;

    let printed = h
        .call(
            "document.export_pdf",
            json!({ "artifact_id": document_id, "html": body.trim_end() }),
        )
        .await;
    assert!(printed.ok(), "{printed:#?}");
    assert!(printed.result["pages"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(printed.result["trust"], "AGENT_GENERATED");
    let pdf_id = printed.result["artifact_id"]
        .as_str()
        .expect("id")
        .to_string();
    let versions = h.versions(&pdf_id).await;
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].media_type, "application/pdf");
    assert_eq!(
        versions[0].format_check["ok"], true,
        "{:#?}",
        versions[0].format_check
    );
    let record = &versions[0].quality_record;
    assert_eq!(record["adapter"], "document.export_pdf");
    assert_eq!(record["ok"], true, "{record:#?}");
    let rules: Vec<&str> = record["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .map(|check| check["rule"].as_str().expect("rule"))
        .collect();
    for rule in [
        "output.document.pdf.source_text",
        "output.document.pdf.structure",
        "output.document.pdf.pages",
        "output.document.pdf.network",
    ] {
        assert!(rules.contains(&rule), "{rules:?}");
    }
    assert_eq!(record["inputs"][0]["artifact_id"], document_id);
    assert!(record["environment"]["browser"]
        .as_str()
        .unwrap_or("")
        .contains("Chrome"));

    // The independent reader pulls the text out of the PDF's own bytes.
    let bytes = h.bytes(&versions[0].id).await;
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("doc.pdf");
    std::fs::write(&path, &bytes).expect("write");
    let script = root().join("crates/qworkerd/tests/support/pdf_text.py");
    match std::process::Command::new("python3")
        .arg(&script)
        .arg(&path)
        .output()
    {
        Ok(read) if read.status.success() => {
            let printed_text = String::from_utf8_lossy(&read.stdout).into_owned();
            for phrase in [
                "Quarterly operations review",
                "Availability held at 99.95 percent",
                "Review the postmortems",
                "A system is reliable when its users stop thinking about it.",
                "make release",
                "South",
            ] {
                assert!(
                    printed_text.contains(phrase),
                    "the PDF does not say {phrase:?}:\n{printed_text}"
                );
            }
            assert!(
                !printed_text.contains("<script") && printed_text.contains("<b>tag</b>"),
                "raw HTML in the source is printed as the characters it is made of"
            );
        }
        _ => blocked("no python3 for the independent PDF reader"),
    }

    // The same print again is the same version.
    let again = h
        .call(
            "document.export_pdf",
            json!({ "artifact_id": document_id, "html": body.trim_end() }),
        )
        .await;
    assert!(again.ok());
    assert_eq!(again.result["replayed"], true, "{again:#?}");
    assert_eq!(h.versions(&pdf_id).await.len(), 1);

    // --- refusals: each names its rule or code, and nothing is stored ------------------------------
    let refused = |outcome: &common::outputs::Outcome, needle: &str| {
        assert!(
            !outcome.ok() && outcome.names(needle),
            "expected {needle}: {outcome:#?}"
        );
    };
    let script_body = h
        .call(
            "document.export_pdf",
            json!({ "artifact_id": document_id, "html": format!("{body}<script>fetch('https://evil.example')</script>") }),
        )
        .await;
    refused(&script_body, "output.html.tag");
    let remote_image = h
        .call(
            "document.export_pdf",
            json!({ "artifact_id": document_id, "html": format!("{body}<img src=\"https://evil.example/t.png\" alt=\"t\">") }),
        )
        .await;
    refused(&remote_image, "output.html.url");
    // A page that prints something other than the document: only its heading survives.
    let hollow = h
        .call(
            "document.export_pdf",
            json!({ "artifact_id": document_id, "html": "<h1>Quarterly operations review</h1><p>Nothing else.</p>" }),
        )
        .await;
    refused(&hollow, "output.document.pdf.source_text");
    refused(&hollow, "VALIDATION_SCHEMA");
    // Not a document, and not an artifact of this workspace.
    let wrong_kind = h
        .call(
            "document.export_pdf",
            json!({ "artifact_id": pdf_id, "html": "<p>x</p>" }),
        )
        .await;
    refused(&wrong_kind, "artifact.kind");
    let missing = h
        .call(
            "document.export_pdf",
            json!({ "artifact_id": "art_01J8Z3K6F1N8VQ2X5W9YZZZ999", "html": "<p>x</p>" }),
        )
        .await;
    assert!(!missing.ok(), "{missing:#?}");
    assert_eq!(
        h.versions(&pdf_id).await.len(),
        1,
        "no refusal made a version"
    );
    h.finish().await;
}

#[tokio::test]
async fn without_a_render_host_a_document_is_not_printed() {
    let Some(h) = Harness::prepare("adp006_docnr", |host| host).await else {
        blocked_marker();
        return;
    };
    let document_id = h
        .create(
            "document",
            "report",
            "Doc",
            "text/markdown",
            b"# Doc\n\nBody text.\n",
        )
        .await;
    let outcome = h
        .call(
            "document.export_pdf",
            json!({ "artifact_id": document_id, "html": "<h1>Doc</h1><p>Body text.</p>" }),
        )
        .await;
    assert!(
        !outcome.ok() && outcome.names("ADAPTER_UNAVAILABLE"),
        "{outcome:#?}"
    );
    assert!(h
        .artifact_with_role(&format!("document_pdf:{document_id}"))
        .await
        .is_none());
    h.finish().await;
}

// ---------------------------------------------------------------------------------------------
// Diagrams
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_diagram_is_stored_only_when_the_svg_draws_its_source() {
    let Some(h) = Harness::prepare("adp006_diagram", |host| host).await else {
        blocked_marker();
        return;
    };
    let ir = text("golden", "diagram-release.ir.json");
    let good = text("golden", "diagram-release.svg");
    let ir_id = h
        .create(
            "other",
            "diagram_source",
            "Release path",
            DIAGRAM_MEDIA_TYPE,
            ir.trim_end().as_bytes(),
        )
        .await;
    let versions = h.versions(&ir_id).await;
    assert_eq!(
        versions[0].format_check["ok"], true,
        "{:#?}",
        versions[0].format_check
    );

    let stored = h
        .call(
            "diagram.render",
            json!({ "artifact_id": ir_id, "svg": good.trim_end() }),
        )
        .await;
    assert!(stored.ok(), "{stored:#?}");
    let svg_id = stored.result["artifact_id"]
        .as_str()
        .expect("id")
        .to_string();
    let versions = h.versions(&svg_id).await;
    assert_eq!(versions[0].media_type, "image/svg+xml");
    assert_eq!(
        versions[0].format_check["ok"], true,
        "{:#?}",
        versions[0].format_check
    );
    let record = &versions[0].quality_record;
    assert_eq!(record["adapter"], "diagram.render");
    assert_eq!(record["ok"], true, "{record:#?}");
    assert_eq!(record["checks"].as_array().map(Vec::len), Some(3));
    assert_eq!(record["inputs"][0]["artifact_id"], ir_id);
    assert_eq!(h.bytes(&versions[0].id).await, good.trim_end().as_bytes());

    // Each way the picture can disagree with its source, or be unsafe, is refused for its own rule.
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "a node is not drawn",
            good.replace("data-node=\"E\"", "data-node=\"E2\""),
            "output.diagram.svg.nodes",
        ),
        (
            "a node says something else",
            good.replace(">Deploy<", ">Destroy<"),
            "output.diagram.svg.nodes",
        ),
        (
            "an edge is missing",
            good.replace("data-edge=\"C-&gt;A\"", "data-edge=\"A-&gt;C\""),
            "output.diagram.svg.edges",
        ),
        (
            "a script is inside",
            good.replace("</svg>", "<script>alert(1)</script></svg>"),
            "output.diagram.svg.safe",
        ),
        (
            "an event handler is on a node",
            good.replacen(
                "<g data-node=\"A\"",
                "<g onclick=\"x()\" data-node=\"A\"",
                1,
            ),
            "output.diagram.svg.safe",
        ),
        (
            "a link leaves the document",
            good.replace(
                "</svg>",
                "<image href=\"https://evil.example/t.png\"/></svg>",
            ),
            "output.diagram.svg.safe",
        ),
        (
            "a foreign object is inside",
            good.replace("</svg>", "<foreignObject><div/></foreignObject></svg>"),
            "output.diagram.svg.safe",
        ),
        (
            "it is not SVG",
            "<html><body>hi</body></html>".to_string(),
            "output.diagram.svg.safe",
        ),
    ];
    for (label, bad, rule) in cases {
        let refused = h
            .call(
                "diagram.render",
                json!({ "artifact_id": ir_id, "svg": bad }),
            )
            .await;
        assert!(!refused.ok(), "{label}: {refused:#?}");
        assert!(refused.names(rule), "{label} names {rule}: {refused:#?}");
    }
    assert_eq!(
        h.versions(&svg_id).await.len(),
        1,
        "no refusal made a version"
    );

    // The SVG artifact is the tool's: the model cannot make or edit one, and an uploaded unsafe SVG fails its check.
    let by_role = h
        .call(
            "artifact.create",
            content_args(
                "image",
                &format!("diagram_svg:{ir_id}"),
                "forged",
                "image/svg+xml",
                good.as_bytes(),
            ),
        )
        .await;
    assert!(
        !by_role.ok() && by_role.names("artifact.tool_owned"),
        "{by_role:#?}"
    );
    let unsafe_svg = h
        .call(
            "artifact.create",
            content_args(
                "image",
                "report",
                "unsafe",
                "image/svg+xml",
                b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1 1\"><script>x</script></svg>",
            ),
        )
        .await;
    assert!(
        !unsafe_svg.ok() && unsafe_svg.names("artifact.format"),
        "{unsafe_svg:#?}"
    );

    // A diagram source that is not a graph of its kind is not stored either.
    let broken = h
        .call(
            "artifact.create",
            content_args(
                "other",
                "diagram_source",
                "broken",
                DIAGRAM_MEDIA_TYPE,
                br#"{"schema_version":"v1","kind":"flow","title":"t","nodes":[{"id":"a","label":"A"}],"edges":[{"from":"a","to":"nowhere"}]}"#,
            ),
        )
        .await;
    assert!(
        !broken.ok() && broken.names("artifact.format"),
        "{broken:#?}"
    );
    let _: Value = json!(null);
    h.finish().await;
}
