//! Decks (ADP-006, DOMAIN.md §10.4 B) against a real PostgreSQL database, the development MinIO, the
//! server's real tool dispatcher and a real headless Google Chrome behind the render port.
//!
//! * **the gate**: a pending page, an edited page and a page that fails its measurement each block
//!   `deck.deliver` and `deck.export` with `DECK_NOT_DELIVERABLE` naming the page and the rule, and a
//!   deck delivers only once every page of its current version is `ready`;
//! * **the measurements are the browser's**: overflow, alternative text, contrast and "the page says
//!   what the slide says" are read from the laid-out page, and each failing page is recorded with the
//!   rule that failed it;
//! * **the pages are the authority's**: an uploaded file shaped like the pages is not them, and
//!   `artifact.create` will not make one;
//! * **exports are verified**: a PDF is printed from the stored pages and read back (an independent
//!   reader pulls the text of each page out of the bytes); a PPTX the plane wrote is opened part by
//!   part, and one that does not say what the deck says is refused.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`, `QUANSIO_TEST_MINIO_*` (see `tests/artifacts.rs`) and a
//! Google Chrome (`BLOCKED_EXTERNAL` is printed, and the test returns, without any of them).

use std::collections::BTreeMap;
use std::path::PathBuf;

use quansio_server::artifacts::outputs::deck::{self, DeckPages, SlideState};
use quansio_server::artifacts::outputs::{html, ooxml};
use serde_json::{json, Value};

mod common;
use common::blocked_marker;
use common::outputs::{Chrome, ChromeRender, Harness, Outcome};

type Parts = BTreeMap<String, Vec<u8>>;

const SLIDES: &str = "application/vnd.quansio.slides+json";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(root().join("packs/skills/artifacts/fixtures").join(name)).expect("fixture")
}

fn golden_json(name: &str) -> Value {
    serde_json::from_slice(
        &std::fs::read(root().join("packs/skills/artifacts/golden").join(name)).expect("golden"),
    )
    .expect("golden json")
}

fn golden_bytes(name: &str) -> Vec<u8> {
    std::fs::read(root().join("packs/skills/artifacts/golden").join(name)).expect("golden")
}

fn blocked(reason: &str) {
    eprintln!("BLOCKED_EXTERNAL: {reason}; the test did not run");
}

/// The `deck.render` arguments for some of a golden's pages, with some fragments replaced.
fn render_args(
    deck_id: &str,
    golden: &Value,
    only: Option<&[usize]>,
    replace: &[(usize, String)],
) -> Value {
    let pages: Vec<Value> = golden["pages"]
        .as_array()
        .expect("pages")
        .iter()
        .filter(|page| {
            only.is_none_or(|only| {
                only.contains(&(page["index"].as_u64().expect("index") as usize))
            })
        })
        .map(|page| {
            let index = page["index"].as_u64().expect("index") as usize;
            let fragment = replace.iter().find(|(at, _)| *at == index).map_or_else(
                || page["fragment"].as_str().expect("fragment").to_string(),
                |(_, text)| text.clone(),
            );
            json!({ "index": index, "fragment": fragment })
        })
        .collect();
    json!({ "artifact_id": deck_id, "css": golden["css"], "pages": pages })
}

fn states(outcome: &Outcome) -> Vec<(u64, String)> {
    outcome.result["slides"]
        .as_array()
        .expect("slides")
        .iter()
        .map(|slide| {
            (
                slide["index"].as_u64().expect("index"),
                slide["state"].as_str().expect("state").to_string(),
            )
        })
        .collect()
}

fn pages_of(outcome: &Outcome) -> Vec<(u64, String, Vec<String>)> {
    outcome.result["error"]["details"]["pages"]
        .as_array()
        .or_else(|| outcome.result["details"]["pages"].as_array())
        .unwrap_or_else(|| panic!("a DECK_NOT_DELIVERABLE names its pages: {outcome:#?}"))
        .iter()
        .map(|page| {
            (
                page["index"].as_u64().expect("index"),
                page["state"].as_str().expect("state").to_string(),
                page["failed_rules"]
                    .as_array()
                    .expect("rules")
                    .iter()
                    .map(|rule| rule.as_str().expect("rule").to_string())
                    .collect(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// What the plane builds is what the authority accepts (no database, no browser)
// ---------------------------------------------------------------------------------------------

#[test]
fn what_the_plane_builds_passes_the_authoritys_own_checks() {
    let golden = golden_json("slides-layouts.render.json");
    let source = fixture("slides-layouts.json");
    let deck = deck::parse_presentation(&source, 1 << 20).expect("the fixture deck");
    assert_eq!(deck.slides.len(), 7);

    html::check_css(golden["css"].as_str().expect("css"))
        .expect("the plane's stylesheet is allowed");
    for page in golden["pages"].as_array().expect("pages") {
        let index = page["index"].as_u64().expect("index") as usize;
        deck::check_fragment(page["fragment"].as_str().expect("fragment"), index)
            .unwrap_or_else(|refusal| panic!("page {index}: {refusal:?}"));
    }
    // Both sides digest a slide the same way.
    let digests: Vec<String> = deck.slides.iter().map(deck::SlideSource::digest).collect();
    let expected: Vec<String> = golden["source_digests"]
        .as_array()
        .expect("digests")
        .iter()
        .map(|digest| digest.as_str().expect("digest").to_string())
        .collect();
    assert_eq!(
        digests, expected,
        "the plane and the authority digest a slide the same way"
    );

    // The package the plane wrote says what the deck says, part by part.
    let package = golden_bytes("slides-layouts.pptx");
    let expect: Vec<ooxml::SlideExpect> = deck
        .slides
        .iter()
        .map(|slide| ooxml::SlideExpect {
            title: slide.title.clone(),
            lines: slide.lines(),
            notes: slide.notes.clone(),
        })
        .collect();
    let checks = ooxml::verify_pptx(
        &package,
        &expect,
        ooxml::PackageLimits {
            max_parts: 2048,
            max_part_bytes: 8 << 20,
            max_bytes: 16 << 20,
        },
    );
    assert!(checks.len() >= 5, "{checks:#?}");
    assert!(checks.iter().all(|check| check.ok), "{checks:#?}");
}

// ---------------------------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_pending_or_failing_page_blocks_delivery_until_every_page_is_ready() {
    let Some(chrome) = Chrome::launch() else {
        return blocked("no Chrome on this host");
    };
    let render = ChromeRender::new(chrome.port);
    let Some(h) = Harness::prepare("adp006_gate", |host| host.with_render(render.clone())).await
    else {
        blocked_marker();
        return;
    };
    let golden = golden_json("slides-layouts.render.json");
    let deck_id = h
        .create(
            "presentation",
            "report",
            "Layouts",
            SLIDES,
            &fixture("slides-layouts.json"),
        )
        .await;

    // Nothing rendered: every page is pending and the deck is refused.
    let refused = h
        .call("deck.deliver", json!({ "artifact_id": deck_id }))
        .await;
    assert!(!refused.ok(), "{refused:#?}");
    assert!(refused.names("DECK_NOT_DELIVERABLE"), "{refused:#?}");
    let pages = pages_of(&refused);
    assert_eq!(pages.len(), 7);
    assert!(
        pages.iter().all(|(_, state, _)| state == "pending"),
        "{pages:?}"
    );

    // Export is behind the same gate.
    let export = h
        .call(
            "deck.export",
            json!({ "artifact_id": deck_id, "format": "pdf" }),
        )
        .await;
    assert!(export.names("DECK_NOT_DELIVERABLE"), "{export:#?}");

    // Render only some pages: the rest stay pending, and the deck is still refused.
    let partial = h
        .call(
            "deck.render",
            render_args(&deck_id, &golden, Some(&[0, 1, 2]), &[]),
        )
        .await;
    assert!(partial.ok(), "{partial:#?}");
    assert_eq!(partial.result["deliverable"], false);
    let standing = states(&partial);
    assert_eq!(
        standing
            .iter()
            .map(|(_, state)| state.as_str())
            .collect::<Vec<_>>(),
        ["ready", "ready", "ready", "pending", "pending", "pending", "pending"],
        "{partial:#?}"
    );
    let still = h
        .call("deck.deliver", json!({ "artifact_id": deck_id }))
        .await;
    assert!(still.names("DECK_NOT_DELIVERABLE"));
    let blockers: Vec<u64> = pages_of(&still)
        .iter()
        .map(|(index, _, _)| *index)
        .collect();
    assert_eq!(blockers, [3, 4, 5, 6]);

    // Four pages fail four different measurements of the real browser.
    let body = |n: usize| {
        format!("<p style=\"margin:0 0 8px\">Filler line number {n} that makes this body far too long for one page</p>")
    };
    let overflow: String = (0..40).map(body).collect();
    let fragments = [
        // slide 1: far too much text
        (
            1,
            format!(
                "<section class=\"slide layout-content d1\" data-slide=\"1\" data-layout=\"content\"><h1>Where we are</h1>\
                 <div class=\"body\"><p>Revenue is up 12 percent</p><p>Churn is down to 2.1 percent</p><p>Support load is flat</p>{overflow}</div></section>"
            ),
        ),
        // slide 3: the picture loses its description
        (
            3,
            golden["pages"][3]["fragment"]
                .as_str()
                .expect("fragment")
                .replace(" alt=\"A single white pixel, enlarged\"", ""),
        ),
        // slide 5: the page says something other than the slide
        (
            5,
            golden["pages"][5]["fragment"]
                .as_str()
                .expect("fragment")
                .replace("In their words", "Something else entirely"),
        ),
        // slide 6: pale grey text on the dark band
        (
            6,
            golden["pages"][6]["fragment"]
                .as_str()
                .expect("fragment")
                .replace("<div class=\"body\">", "<div class=\"body\" style=\"color:#4a5470\">"),
        ),
    ];
    let broken = h
        .call(
            "deck.render",
            render_args(&deck_id, &golden, Some(&[1, 3, 4, 5, 6]), &fragments),
        )
        .await;
    assert!(
        broken.ok(),
        "a failing page is recorded, not refused: {broken:#?}"
    );
    assert_eq!(broken.result["deliverable"], false);
    let standing = states(&broken);
    assert_eq!(
        standing
            .iter()
            .map(|(_, state)| state.as_str())
            .collect::<Vec<_>>(),
        [
            "ready",
            "failed_quality",
            "ready",
            "failed_quality",
            "ready",
            "failed_quality",
            "failed_quality"
        ],
        "{broken:#?}"
    );
    let rules = |at: usize| -> Vec<String> {
        broken.result["slides"][at]["failed_rules"]
            .as_array()
            .expect("rules")
            .iter()
            .map(|rule| rule.as_str().expect("rule").to_string())
            .collect()
    };
    assert_eq!(rules(1), ["output.deck.page.overflow"]);
    assert_eq!(rules(3), ["output.deck.page.alt_text"]);
    assert_eq!(rules(5), ["output.deck.page.source_text"]);
    assert_eq!(rules(6), ["output.deck.page.contrast"]);

    let blocked_now = h
        .call("deck.deliver", json!({ "artifact_id": deck_id }))
        .await;
    assert!(blocked_now.names("DECK_NOT_DELIVERABLE"));
    let pages = pages_of(&blocked_now);
    assert_eq!(
        pages
            .iter()
            .map(|(index, state, rules)| (*index, state.as_str(), rules.clone()))
            .collect::<Vec<_>>(),
        vec![
            (
                1,
                "failed_quality",
                vec!["output.deck.page.overflow".to_string()]
            ),
            (
                3,
                "failed_quality",
                vec!["output.deck.page.alt_text".to_string()]
            ),
            (
                5,
                "failed_quality",
                vec!["output.deck.page.source_text".to_string()]
            ),
            (
                6,
                "failed_quality",
                vec!["output.deck.page.contrast".to_string()]
            ),
        ]
    );

    // The pages Artifact keeps the failing pages with their checks, and a screenshot per rendered page.
    let pages_artifact = h
        .artifact_with_role(&format!("deck_pages:{deck_id}"))
        .await
        .expect("the pages artifact");
    let versions = h.versions(&pages_artifact).await;
    assert_eq!(
        versions.len(),
        2,
        "the partial render and the broken re-render: {versions:#?}"
    );
    let latest = versions.last().expect("version");
    assert_eq!(latest.media_type, deck::PAGES_MEDIA_TYPE);
    assert_eq!(
        latest.format_check["ok"], true,
        "{:#?}",
        latest.format_check
    );
    assert_eq!(latest.quality_record["adapter"], "deck.render");
    assert_eq!(latest.quality_record["ok"], false);
    assert_eq!(latest.quality_record["trust"], "UNTRUSTED_EXTERNAL");
    assert!(latest.quality_record["environment"]["browser"]
        .as_str()
        .unwrap_or("")
        .contains("Chrome"));
    let record_rules: Vec<(&str, bool)> = latest.quality_record["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .map(|check| {
            (
                check["rule"].as_str().expect("rule"),
                check["ok"].as_bool().expect("ok"),
            )
        })
        .collect();
    assert!(
        record_rules.contains(&("output.deck.page.overflow", false)),
        "{record_rules:?}"
    );
    assert!(
        record_rules.contains(&("output.deck.page.network", true)),
        "{record_rules:?}"
    );
    assert!(
        record_rules.contains(&("output.deck.pages.ready", false)),
        "{record_rules:?}"
    );
    let stored = DeckPages::parse(&h.bytes(&latest.id).await).expect("stored pages parse");
    assert_eq!(stored.slides[1].state, SlideState::FailedQuality);
    assert_eq!(
        stored.slides[1]
            .checks
            .iter()
            .filter(|check| !check.ok)
            .count(),
        1
    );
    assert_eq!(
        stored.slides[0].state,
        SlideState::Ready,
        "slide 0 was carried, not re-rendered"
    );
    assert!(
        !stored.slides[3].evidence_ids.is_empty(),
        "a screenshot of the page the browser laid out"
    );

    // The way out of failed_quality is a page that passes: re-render the four from the plane's own pages.
    let fixed = h
        .call(
            "deck.render",
            render_args(&deck_id, &golden, Some(&[1, 3, 5, 6]), &[]),
        )
        .await;
    assert!(fixed.ok(), "{fixed:#?}");
    assert_eq!(fixed.result["deliverable"], true, "{fixed:#?}");
    assert!(
        states(&fixed).iter().all(|(_, state)| state == "ready"),
        "{fixed:#?}"
    );
    let delivered = h
        .call("deck.deliver", json!({ "artifact_id": deck_id }))
        .await;
    assert!(delivered.ok(), "{delivered:#?}");
    assert_eq!(
        delivered.result["uri"],
        format!("quansio://artifact/{deck_id}")
    );
    assert_eq!(delivered.result["slide_count"], 7);
    assert_eq!(delivered.result["exports"], json!([]));
    assert_eq!(
        delivered.result["content_digest"].as_str().map(str::len),
        Some(64)
    );

    // Edit one slide: the deck is no longer deliverable, and only that page needs a new render.
    let edited_source = fixture("slides-layouts-edited.json");
    let edit = h.update(&deck_id, SLIDES, &edited_source).await;
    assert!(edit.ok(), "{edit:#?}");
    let after_edit = h
        .call("deck.deliver", json!({ "artifact_id": deck_id }))
        .await;
    assert!(after_edit.names("DECK_NOT_DELIVERABLE"));
    let pages = pages_of(&after_edit);
    assert_eq!(
        pages
            .iter()
            .map(|(index, state, _)| (*index, state.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "edited")],
        "only the edited slide stands in the way: {after_edit:#?}"
    );
    assert_eq!(
        after_edit.result["error"]["details"]["stale"]
            .as_bool()
            .or(after_edit.result["details"]["stale"].as_bool()),
        Some(true)
    );
    let edited_golden = golden_json("slides-layouts-edited.render.json");
    let refreshed = h
        .call(
            "deck.render",
            render_args(&deck_id, &edited_golden, Some(&[1]), &[]),
        )
        .await;
    assert!(refreshed.ok(), "{refreshed:#?}");
    assert_eq!(refreshed.result["deliverable"], true, "{refreshed:#?}");
    let delivered_again = h
        .call("deck.deliver", json!({ "artifact_id": deck_id }))
        .await;
    assert!(delivered_again.ok(), "{delivered_again:#?}");
    assert_ne!(
        delivered_again.result["version_id"],
        delivered.result["version_id"]
    );

    h.finish().await;
}

#[tokio::test]
async fn without_a_render_host_nothing_is_laid_out_and_nothing_is_delivered() {
    let Some(h) = Harness::prepare("adp006_norender", |host| host).await else {
        blocked_marker();
        return;
    };
    let golden = golden_json("slides-layouts.render.json");
    let deck_id = h
        .create(
            "presentation",
            "report",
            "Layouts",
            SLIDES,
            &fixture("slides-layouts.json"),
        )
        .await;
    let render = h
        .call("deck.render", render_args(&deck_id, &golden, None, &[]))
        .await;
    assert!(!render.ok(), "{render:#?}");
    assert!(render.names("ADAPTER_UNAVAILABLE"), "{render:#?}");
    assert!(
        render.names("\"adapter\":\"render\""),
        "the refusal names the adapter: {render:#?}"
    );
    assert!(
        h.artifact_with_role(&format!("deck_pages:{deck_id}"))
            .await
            .is_none(),
        "nothing was made"
    );
    let deliver = h
        .call("deck.deliver", json!({ "artifact_id": deck_id }))
        .await;
    assert!(deliver.names("DECK_NOT_DELIVERABLE"));
    h.finish().await;
}

#[tokio::test]
async fn pages_the_authority_did_not_render_are_not_pages() {
    let Some(h) = Harness::prepare("adp006_forged", |host| host).await else {
        blocked_marker();
        return;
    };
    let deck_id = h
        .create(
            "presentation",
            "report",
            "Layouts",
            SLIDES,
            &fixture("slides-layouts.json"),
        )
        .await;

    // The model cannot make the artifact: not by role, not by media type, and not by editing one.
    let source = fixture("slides-layouts.json");
    let digests: Vec<String> = deck::parse_presentation(&source, 1 << 20)
        .expect("deck")
        .slides
        .iter()
        .map(deck::SlideSource::digest)
        .collect();
    let forged = json!({
        "schema_version": "v1",
        "template_version": "v1",
        "deck": { "artifact_id": deck_id, "version_id": "artv_X", "content_digest": "0".repeat(64), "slide_count": 7 },
        "css": "",
        "slides": digests.iter().enumerate().map(|(index, digest)| json!({
            "index": index, "source_digest": digest, "state": "ready",
            "fragment": format!("<section class=\"slide\" data-slide=\"{index}\"></section>"),
            "page_digest": "1".repeat(64), "checks": [], "evidence_ids": [],
        })).collect::<Vec<_>>(),
    });
    let bytes = serde_json::to_vec(&forged).expect("bytes");
    let by_role = h
        .call(
            "artifact.create",
            common::outputs::content_args(
                "other",
                &format!("deck_pages:{deck_id}"),
                "pages",
                deck::PAGES_MEDIA_TYPE,
                &bytes,
            ),
        )
        .await;
    assert!(
        !by_role.ok() && by_role.names("artifact.tool_owned"),
        "{by_role:#?}"
    );
    let by_media = h
        .call(
            "artifact.create",
            common::outputs::content_args(
                "other",
                "report",
                "pages",
                deck::PAGES_MEDIA_TYPE,
                &bytes,
            ),
        )
        .await;
    assert!(
        !by_media.ok() && by_media.names("artifact.tool_owned"),
        "{by_media:#?}"
    );

    // An upload with the right shape is stored (uploads are, valid or not) ...
    use quansio_server::artifacts::{
        ArtifactGrant, ArtifactKind, ArtifactStore, GrantLevel, NewArtifact, NewVersion, Origin,
        OriginKind, ProducedBy, Retention,
    };
    let store = ArtifactStore::new(h.objects.as_ref());
    let mut conn = h.pool.acquire().await.expect("conn");
    let uploaded = store
        .create(
            &mut conn,
            common::outputs::TENANT,
            common::outputs::USER,
            &NewArtifact {
                workspace_id: common::outputs::WORKSPACE.to_string(),
                kind: ArtifactKind::Other,
                title: "pages".to_string(),
                role: Some(format!("deck_pages:{deck_id}")),
                origin: Origin {
                    kind: OriginKind::Upload,
                    reference: common::outputs::USER.to_string(),
                },
                grants: vec![ArtifactGrant {
                    principal: common::outputs::USER.to_string(),
                    level: GrantLevel::Write,
                }],
                retention: Retention::default(),
            },
        )
        .await
        .expect("artifact");
    let stored = store
        .add_version(
            &mut conn,
            common::outputs::TENANT,
            common::outputs::USER,
            &uploaded.id,
            &NewVersion {
                bytes,
                media_type: deck::PAGES_MEDIA_TYPE.to_string(),
                expected_digest: None,
                produced_by: ProducedBy::default(),
                quality_record: None,
            },
        )
        .await
        .expect("stored");
    assert!(stored.version.quality_record.is_none());
    drop(conn);

    // ... and it passes for nothing: every page is still pending.
    let refused = h
        .call("deck.deliver", json!({ "artifact_id": deck_id }))
        .await;
    assert!(refused.names("DECK_NOT_DELIVERABLE"), "{refused:#?}");
    assert!(
        pages_of(&refused)
            .iter()
            .all(|(_, state, _)| state == "pending"),
        "{refused:#?}"
    );
    h.finish().await;
}

// ---------------------------------------------------------------------------------------------
// Exports
// ---------------------------------------------------------------------------------------------

fn rezip(parts: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    // A stored ZIP, written here independently of the reader under test.
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (name, data) in parts {
        let offset = out.len() as u32;
        let crc = ooxml::crc32(data);
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        directory.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        directory.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        directory.extend_from_slice(&crc.to_le_bytes());
        directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
        directory.extend_from_slice(&[0; 12]);
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let at = out.len() as u32;
    out.extend_from_slice(&directory);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(parts.len() as u16).to_le_bytes());
    out.extend_from_slice(&(parts.len() as u16).to_le_bytes());
    out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
    out.extend_from_slice(&at.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn pptx_args(deck_id: &str, package: &[u8]) -> Value {
    use base64::Engine as _;
    json!({
        "artifact_id": deck_id,
        "format": "pptx",
        "content_base64": base64::engine::general_purpose::STANDARD.encode(package),
        "content_digest": quansio_core::Digest::of(package).to_string(),
    })
}

#[tokio::test]
async fn a_deliverable_deck_exports_a_printed_pdf_and_a_verified_pptx() {
    let Some(chrome) = Chrome::launch() else {
        return blocked("no Chrome on this host");
    };
    let render = ChromeRender::new(chrome.port);
    let Some(h) = Harness::prepare("adp006_export", |host| host.with_render(render.clone())).await
    else {
        blocked_marker();
        return;
    };
    let golden = golden_json("slides-layouts.render.json");
    let deck_id = h
        .create(
            "presentation",
            "report",
            "Layouts",
            SLIDES,
            &fixture("slides-layouts.json"),
        )
        .await;
    let package = golden_bytes("slides-layouts.pptx");

    // Not deliverable yet: neither export is made, and the package is not even looked at.
    let early = h.call("deck.export", pptx_args(&deck_id, &package)).await;
    assert!(early.names("DECK_NOT_DELIVERABLE"), "{early:#?}");

    let rendered = h
        .call("deck.render", render_args(&deck_id, &golden, None, &[]))
        .await;
    assert_eq!(rendered.result["deliverable"], true, "{rendered:#?}");

    // The screenshots the browser took of the laid-out pages are evidence: seven real 1280x720 PNGs,
    // each of a different page.
    let evidence: Vec<String> = rendered.result["evidence_ids"]
        .as_array()
        .expect("evidence")
        .iter()
        .map(|id| id.as_str().expect("id").to_string())
        .collect();
    assert_eq!(evidence.len(), 7);
    let mut digests = std::collections::BTreeSet::new();
    for id in &evidence {
        let png = h.evidence_bytes(id).await;
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let (width, height) = (
            u32::from_be_bytes(png[16..20].try_into().expect("width")),
            u32::from_be_bytes(png[20..24].try_into().expect("height")),
        );
        assert_eq!((width, height), (1280, 720));
        assert!(
            png.len() > 2_000,
            "a page with something on it is not a few hundred bytes"
        );
        digests.insert(quansio_core::Digest::of(&png).to_string());
    }
    assert_eq!(digests.len(), 7, "every screenshot is of a different page");

    // --- PDF: printed from the stored pages, one PDF page per slide --------------------------------
    let pdf = h
        .call(
            "deck.export",
            json!({ "artifact_id": deck_id, "format": "pdf" }),
        )
        .await;
    assert!(pdf.ok(), "{pdf:#?}");
    assert_eq!(pdf.result["pages"], 7);
    let pdf_artifact = pdf.result["artifact_id"].as_str().expect("id").to_string();
    let versions = h.versions(&pdf_artifact).await;
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].media_type, "application/pdf");
    assert_eq!(
        versions[0].format_check["ok"], true,
        "{:#?}",
        versions[0].format_check
    );
    assert_eq!(versions[0].quality_record["adapter"], "deck.export");
    assert_eq!(
        versions[0].quality_record["ok"], true,
        "{:#?}",
        versions[0].quality_record
    );
    let rules: Vec<&str> = versions[0].quality_record["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .map(|check| check["rule"].as_str().expect("rule"))
        .collect();
    for rule in [
        "output.deck.pdf.structure",
        "output.deck.pdf.pages",
        "output.deck.pdf.slides",
        "output.deck.pdf.network",
    ] {
        assert!(rules.contains(&rule), "{rules:?}");
    }
    assert_eq!(
        versions[0].quality_record["inputs"]
            .as_array()
            .map(Vec::len),
        Some(2),
        "the deck and the pages it was printed from"
    );
    let bytes = h.bytes(&versions[0].id).await;
    assert!(bytes.starts_with(b"%PDF-1."));

    // The independent reader pulls each slide's text out of the PDF's bytes.
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("deck.pdf");
    std::fs::write(&path, &bytes).expect("write");
    let script = root().join("crates/qworkerd/tests/support/pdf_text.py");
    match std::process::Command::new("python3")
        .arg(&script)
        .arg(&path)
        .output()
    {
        Ok(read) if read.status.success() => {
            let text = String::from_utf8_lossy(&read.stdout).into_owned();
            let pages: Vec<&str> = text.split('\u{c}').collect();
            assert_eq!(pages.len(), 7, "{text:?}");
            let titles = [
                "Quarterly review",
                "Where we are",
                "Two views",
                "The product",
                "Revenue by region",
                "In their words",
                "Next steps",
            ];
            for (page, title) in pages.iter().zip(titles) {
                assert!(page.contains(title), "page says {page:?}, not {title:?}");
            }
            assert!(pages[1].contains("Churn is down to 2.1 percent"));
            assert!(
                pages[4].contains("North") && pages[4].contains("15M"),
                "the chart's labels are text in the PDF"
            );
        }
        _ => blocked("no python3 for the independent PDF reader"),
    }

    // The same export again is the same version.
    let again = h
        .call(
            "deck.export",
            json!({ "artifact_id": deck_id, "format": "pdf" }),
        )
        .await;
    assert!(again.ok(), "{again:#?}");
    assert_eq!(
        again.result["replayed"], true,
        "a PDF printed twice from the same pages is the same bytes: {again:#?}"
    );
    assert_eq!(again.result["version_id"], pdf.result["version_id"]);

    // --- PPTX: opened part by part ---------------------------------------------------------------
    let pptx = h.call("deck.export", pptx_args(&deck_id, &package)).await;
    assert!(pptx.ok(), "{pptx:#?}");
    let versions = h
        .versions(pptx.result["artifact_id"].as_str().expect("id"))
        .await;
    assert_eq!(versions[0].quality_record["ok"], true);
    let pptx_rules: Vec<&str> = versions[0].quality_record["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .map(|check| check["rule"].as_str().expect("rule"))
        .collect();
    for rule in [
        "output.deck.pptx.package",
        "output.deck.pptx.parts",
        "output.deck.pptx.external",
        "output.deck.pptx.slides",
        "output.deck.pptx.text",
        "output.deck.pptx.notes",
    ] {
        assert!(pptx_rules.contains(&rule), "{pptx_rules:?}");
    }
    assert_eq!(versions[0].format_check["ok"], true);

    // Delivery lists the exports made for exactly this deck version.
    let delivered = h
        .call("deck.deliver", json!({ "artifact_id": deck_id }))
        .await;
    assert!(delivered.ok(), "{delivered:#?}");
    let formats: Vec<&str> = delivered.result["exports"]
        .as_array()
        .expect("exports")
        .iter()
        .map(|export| export["format"].as_str().expect("format"))
        .collect();
    assert_eq!(formats, ["pdf", "pptx"]);

    // --- A package that is not this deck's is refused, each for its own rule -----------------------
    let parts = ooxml::read_zip(
        &package,
        ooxml::PackageLimits {
            max_parts: 2048,
            max_part_bytes: 8 << 20,
            max_bytes: 16 << 20,
        },
    )
    .expect("golden package")
    .parts;
    let mutate = |edit: &dyn Fn(&mut Parts)| {
        let mut parts = parts.clone();
        edit(&mut parts);
        rezip(&parts)
    };
    let replace = |parts: &mut BTreeMap<String, Vec<u8>>, name: &str, from: &str, to: &str| {
        let text = String::from_utf8(parts[name].clone()).expect("utf8");
        assert!(text.contains(from), "{name} has {from}");
        parts.insert(name.to_string(), text.replace(from, to).into_bytes());
    };
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "a slide's text is not the deck's",
            mutate(&|p| {
                replace(
                    p,
                    "ppt/slides/slide2.xml",
                    "Churn is down to 2.1 percent",
                    "Churn is up",
                )
            }),
            "output.deck.pptx.text",
        ),
        (
            "a slide is missing",
            mutate(&|p| {
                p.remove("ppt/slides/slide7.xml");
            }),
            "output.deck.pptx.parts",
        ),
        (
            "a relationship leaves the package",
            mutate(&|p| {
                replace(
                    p,
                    "ppt/slides/_rels/slide1.xml.rels",
                    "Target=\"../slideLayouts/slideLayout1.xml\"",
                    "Target=\"https://example.com/x\" TargetMode=\"External\"",
                )
            }),
            "output.deck.pptx.external",
        ),
        (
            "a part is not well-formed",
            mutate(&|p| replace(p, "ppt/theme/theme1.xml", "</a:theme>", "")),
            "output.deck.pptx.parts",
        ),
        (
            "an entity declaration is smuggled in",
            mutate(&|p| {
                replace(
                    p,
                    "ppt/slides/slide1.xml",
                    "<p:sld ",
                    "<!DOCTYPE x [<!ENTITY a \"b\">]><p:sld ",
                )
            }),
            "output.deck.pptx.parts",
        ),
        (
            "the speaker notes are gone",
            mutate(&|p| {
                replace(
                    p,
                    "ppt/notesSlides/notesSlide2.xml",
                    "Lead with the churn number.",
                    "x",
                )
            }),
            "output.deck.pptx.notes",
        ),
        (
            "a macro part rides along",
            mutate(&|p| {
                p.insert("ppt/vbaProject.bin".to_string(), vec![0, 1, 2]);
            }),
            "output.deck.pptx.external",
        ),
        (
            "not a package at all",
            b"PK\x03\x04 nope".to_vec(),
            "output.deck.pptx.package",
        ),
    ];
    for (label, bad, rule) in cases {
        let refused = h.call("deck.export", pptx_args(&deck_id, &bad)).await;
        assert!(!refused.ok(), "{label}: {refused:#?}");
        assert!(
            refused.names(rule),
            "{label} should name {rule}: {refused:#?}"
        );
        assert!(refused.names("VALIDATION_SCHEMA"), "{label}: {refused:#?}");
    }
    // A package whose digest is not the one declared is refused before it is read.
    let mut wrong = pptx_args(&deck_id, &package);
    wrong["content_digest"] = json!("0".repeat(64));
    let refused = h.call("deck.export", wrong).await;
    assert!(refused.names("artifact.content_digest"), "{refused:#?}");
    // None of the refusals made a version.
    let pptx_artifact = pptx.result["artifact_id"].as_str().expect("id");
    assert_eq!(h.versions(pptx_artifact).await.len(), 1);

    h.finish().await;
}
