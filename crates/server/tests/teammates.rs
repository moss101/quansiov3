//! APP-014 teammate capability narrowing and archive lineage.

use quansio_server::control::teammates::{
    archive, standing_instructions_cannot_widen, template_ids, templates_yaml,
};

#[test]
fn standing_instructions_cannot_widen_capability() {
    let current = vec!["read.internal".to_string()];
    assert!(standing_instructions_cannot_widen(
        &current,
        &["read.internal".into()]
    ));
    assert!(!standing_instructions_cannot_widen(
        &current,
        &["read.internal".into(), "fs.write.host".into()]
    ));
}

#[test]
fn archive_keeps_lineage() {
    let archived = archive(
        "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        Some("ath_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".into()),
    );
    assert_eq!(archived.status, "archived");
    assert!(archived.lineage_retained);
    assert_eq!(
        archived.agent_thread_id.as_deref(),
        Some("ath_01J8Z3K6F1N8VQ2X5W9Y0AAAAA")
    );
}

#[test]
fn templates_are_configuration_not_code() {
    let ids = template_ids(templates_yaml());
    assert!(ids.contains(&"general-assistant".to_string()));
    assert!(ids.contains(&"researcher".to_string()));
    assert!(ids.contains(&"engineer".to_string()));
}
