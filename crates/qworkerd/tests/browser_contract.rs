//! The browser tool contract: DOMAIN.md and the generated catalog say what the code does, and every tool
//! result is shaped the way its declaration says (ADP-001, DOMAIN.md §7.5, §15).
//!
//! Two kinds of check. The first reads the authority documents and compares them with the code: every
//! `BROWSER_*` code is in DOMAIN.md §15 and the generated catalog, with the `retryable` and
//! `requires_new_observation` flags the code returns — so a flag cannot drift from the document. The
//! second drives the tools that the other suites do not (`extract`, an observation with a screenshot) on a
//! real Chrome and lets the dispatcher validate each result against the declared `output_schema`.

mod common;

use common::{blocked, call, driver, observe, open_page, run, Browser, Site};
use quansio_qworkerd::browser::FaultCode;
use serde_json::json;

fn repository_file(relative: &str) -> String {
    std::fs::read_to_string(format!("{}/../../{relative}", env!("CARGO_MANIFEST_DIR")))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

#[test]
fn every_browser_error_code_is_documented_with_the_flags_the_code_returns() {
    let domain = repository_file("DOMAIN.md");
    let catalog = repository_file("schemas/catalog/errors.yaml");
    for code in FaultCode::ALL {
        let name = code.as_str();
        // The family row of the §15 table names it.
        assert!(
            domain
                .lines()
                .any(|line| line.starts_with("| Browser") && line.contains(&format!("`{name}`"))),
            "{name} is not in the §15 Browser family row"
        );
        // The flags table gives the same two booleans the code returns.
        let row = domain
            .lines()
            .find(|line| line.starts_with(&format!("| `{name}` |")))
            .unwrap_or_else(|| panic!("{name} has no row in the §15 flags table"));
        let cells: Vec<&str> = row.trim_matches('|').split('|').map(str::trim).collect();
        assert_eq!(
            cells[1],
            format!("`{}`", code.retryable()),
            "{name}: retryable"
        );
        assert_eq!(
            cells[2],
            format!("`{}`", code.requires_new_observation()),
            "{name}: requires_new_observation"
        );
        // And the generated catalog the server's error enum is checked against.
        assert!(
            catalog.contains(&format!("code: {name}\n  family: Browser")),
            "{name} is not in errors.yaml"
        );
    }
    // Nothing in the document is missing from the code.
    let documented = domain
        .lines()
        .filter(|line| line.starts_with("| `BROWSER_"))
        .count();
    assert_eq!(
        documented,
        FaultCode::ALL.len(),
        "the flags table names a code the driver does not have"
    );
    assert!(FaultCode::parse("BROWSER_STALE_PAGE").is_some());
    assert!(FaultCode::parse("BROWSER_NOPE").is_none());
}

#[test]
fn the_three_new_tools_and_the_side_effect_table_are_in_the_domain() {
    let domain = repository_file("DOMAIN.md");
    for tool in [
        "browser.observe",
        "browser.press_key",
        "browser.handle_dialog",
    ] {
        assert!(domain.contains(tool), "{tool} is not in DOMAIN.md §7.5");
    }
    for class in [
        "read",
        "read_sensitive",
        "local_mutation",
        "open_world_mutation",
        "potential_external_effect",
    ] {
        assert!(
            domain.contains(&format!("| `{class}` |")),
            "{class} is not in the side-effect table"
        );
    }
    for term in [
        "DocumentRevision",
        "In-doubt outcome",
        "SecretPlaceholder",
        "DecisionReceipt",
    ] {
        assert!(
            domain.contains(&format!("**{term}"))
                || domain.contains(&format!("**Snapshot / {term}")),
            "{term} is not in the glossary"
        );
    }
}

#[tokio::test]
async fn extract_and_a_screenshot_observation_return_what_their_declarations_describe() {
    let Some(browser) = Browser::launch() else {
        blocked("no Chrome on this host");
        return;
    };
    let site = Site::start().await;
    site.html(
        "/",
        "<html><head><title>Contract</title></head><body><p id=\"p\">Hello <b>there</b></p><canvas id=\"c\" width=\"10\" height=\"10\"></canvas></body></html>",
    );
    let dir = tempfile::tempdir().expect("dir");
    let driver = driver(&browser, dir.path(), |_| {});
    let (tab, _) = open_page(&driver, 1, &site.url("/")).await;

    // `extract` in all three formats; the dispatcher validates each against the declared schema.
    for (format, expected) in [
        ("text", "Hello there"),
        ("html", "<b>there</b>"),
        ("json", "\"tag\":\"p\""),
    ] {
        let reply = run(
            &driver,
            &call(2),
            "browser.extract",
            json!({ "tab_id": tab, "selector": "#p", "format": format }),
        )
        .await;
        assert_eq!(reply["status"], "ok", "{format}: {reply}");
        assert_eq!(reply["trust"], "UNTRUSTED_EXTERNAL");
        assert!(
            reply["output"]["content"]
                .as_str()
                .is_some_and(|c| c.contains(expected)),
            "{format}: {reply}"
        );
        assert_eq!(reply["output"]["truncated"], false);
    }
    let missing = run(
        &driver,
        &call(3),
        "browser.extract",
        json!({ "tab_id": tab, "selector": "#nope" }),
    )
    .await;
    assert_eq!(
        missing["error"]["code"], "BROWSER_TARGET_NOT_FOUND",
        "{missing}"
    );
    assert_eq!(missing["error"]["retryable"], true);
    assert_eq!(missing["error"]["requires_new_observation"], true);
    // A bad format is an invalid request, not a guess.
    let bad = run(
        &driver,
        &call(4),
        "browser.extract",
        json!({ "tab_id": tab, "selector": "#p", "format": "pdf" }),
    )
    .await;
    assert_eq!(bad["error"]["code"], "BROWSER_INVALID_REQUEST", "{bad}");

    // An observation that asks for a screenshot carries the capture and the evidence bytes.
    let reply = driver
        .dispatch(
            "browser.observe",
            &json!({ "session_id": common::SESSION, "tab_id": tab, "include_screenshot": true }),
            &call(5),
        )
        .await;
    assert_eq!(reply.status.as_str(), "ok", "{:?}", reply.to_json());
    let png = reply.evidence.as_ref().expect("a screenshot");
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    let capture = &reply.output["capture"];
    assert!(capture["capture_id"]
        .as_str()
        .is_some_and(|id| id.starts_with("cap_")));
    assert_eq!(capture["tab_id"], json!(tab));
    assert!(capture["width"].as_u64().is_some_and(|w| w > 0));
    // Observing twice with nothing changed gives a new snapshot id each time and the same revision.
    let first = observe(&driver, 6, &tab).await;
    let second = observe(&driver, 7, &tab).await;
    assert_ne!(first["snapshot_id"], second["snapshot_id"]);
    assert_eq!(first["document_revision"], second["document_revision"]);
}
