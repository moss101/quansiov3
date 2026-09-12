//! Event family and subject naming tests (DOMAIN.md §9.1–§9.2).
//!
//! These need no database: they pin the family list to the generated catalog and prove
//! that an unknown family is rejected with a typed error rather than written.

use quansio_core::{CorrelationId, EventId, Sequence, TypedId, UlidGenerator};
use quansio_events::{
    parse_family, Actor, EventDraft, EventError, EventFamily, EventType, RuntimeEvent,
    EVENT_FAMILIES,
};

/// `schemas/catalog/event-families.yaml` is generated from DOMAIN.md; the crate's list
/// must match it exactly or the two naming authorities have drifted.
#[test]
fn canonical_families_match_the_generated_catalog() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../schemas/catalog/event-families.yaml"
    );
    let text = std::fs::read_to_string(path).expect("read generated event-families.yaml");
    let catalog: Vec<String> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- "))
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        catalog,
        EVENT_FAMILIES
            .iter()
            .map(|family| (*family).to_string())
            .collect::<Vec<_>>(),
        "crates/events family list must match schemas/catalog/event-families.yaml"
    );
    assert_eq!(EventFamily::all().len(), catalog.len());
}

#[test]
fn unknown_family_is_rejected_with_a_typed_error() {
    let error = EventType::parse("bogus.created").expect_err("unknown family must fail");
    assert!(
        matches!(&error, EventError::UnknownEventFamily { family } if family == "bogus"),
        "unexpected error: {error:?}"
    );
    assert!(matches!(
        parse_family("not_a_family"),
        Err(EventError::UnknownEventFamily { .. })
    ));
}

#[test]
fn malformed_types_are_rejected() {
    for value in [
        "no_dot",
        "run.",
        ".started",
        "run.NotLower",
        "run.has space",
        "",
    ] {
        let error = EventType::parse(value).expect_err("malformed type must fail");
        assert!(
            matches!(error, EventError::MalformedEventType { .. }),
            "{value:?} produced {error:?}"
        );
    }
}

#[test]
fn known_types_parse_and_subjects_follow_domain_naming() {
    let event_type = EventType::parse("run.verification_passed").expect("canonical type");
    assert_eq!(event_type.family(), EventFamily::Run);
    assert_eq!(event_type.name(), "verification_passed");
    assert_eq!(event_type.to_string(), "run.verification_passed");

    let mut generator = UlidGenerator::new();
    let event_id: EventId = EventId::generate(&mut generator);
    let correlation_id = CorrelationId::generate(&mut generator);
    let draft = EventDraft::new(
        "run",
        "run_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
        3,
        event_type,
        correlation_id,
        Actor::system("events-test"),
    )
    .with_event_id(event_id)
    .with_payload(serde_json::json!({"verdict": "pass"}));

    let event = RuntimeEvent::from_draft(draft, "tn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC", Sequence::FIRST);
    assert_eq!(
        event.subject(),
        "q.tn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC.run.run.verification_passed"
    );

    let json = event.to_json_value();
    assert_eq!(json["type"], "run.verification_passed");
    assert_eq!(json["event_id"], event_id.to_string());
    assert_eq!(json["tenant_id"], "tn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC");
    assert_eq!(json["sequence"], 1);
    assert_eq!(json["actor"]["kind"], "system");
    assert_eq!(json["payload"]["verdict"], "pass");
    assert!(
        json.get("command_id").is_none(),
        "optional fields are omitted"
    );
    assert!(json.get("causation_id").is_none());
    assert!(json.get("workspace_id").is_none());
    assert!(json.get("generation").is_none());
}
