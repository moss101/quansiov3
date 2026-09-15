//! OPS-002: audit immutability, export/delete, derived-store propagation.

use quansio_server::audit::{AuditEntry, AuditLog};
use quansio_server::control::data_lifecycle::{DataSubject, DERIVED_PLANES};

fn sample(id: &str, tenant: &str, action: &str) -> AuditEntry {
    AuditEntry {
        id: id.into(),
        tenant_id: tenant.into(),
        workspace_id: Some("ws_a".into()),
        actor: "usr_ada".into(),
        action: action.into(),
        target_ref: "usr_bob".into(),
        decision: "allow".into(),
        reason: "policy".into(),
        correlation_id: "corr_1".into(),
        occurred_at: "2026-09-15T08:20:00Z".into(),
        prev_hash: String::new(),
        hash: String::new(),
    }
}

#[test]
fn audit_entries_cannot_be_edited_in_place() {
    let mut log = AuditLog::default();
    log.append(sample("aud_1", "tn_a", "InviteMember"))
        .expect("append");
    log.append(sample("aud_2", "tn_a", "SetPolicy"))
        .expect("append");
    assert!(log.chain_holds("tn_a"));
    let err = log
        .edit_in_place("aud_1", sample("aud_1", "tn_a", "tamper"))
        .expect_err("edit");
    assert!(err.contains("cannot be edited in place"));
    let exported = log.export("tn_a");
    assert_eq!(exported.len(), 2);
    assert_eq!(exported[0].action, "InviteMember");
    assert_eq!(exported[1].prev_hash, exported[0].hash);
    assert_ne!(exported[0].hash, exported[1].hash);
}

#[test]
fn export_and_delete_e2e_respects_legal_hold() {
    let mut subject = DataSubject::populated("tn_a", "usr_bob");
    let snapshot = subject.export();
    assert!(snapshot.contains(&"authoritative".into()));
    for plane in DERIVED_PLANES {
        assert!(snapshot.iter().any(|item| item == plane));
    }
    subject.legal_holds.insert("legal_hold".into());
    let held = subject.delete("legal_hold").expect_err("hold");
    assert!(held.contains("legal hold"));
    assert!(subject.authoritative);
    assert!(subject.derived_remaining());
    subject.legal_holds.clear();
    let visited = subject.delete("user").expect("delete");
    assert!(visited.contains(&"authoritative".into()));
    for plane in DERIVED_PLANES {
        assert!(
            visited.iter().any(|item| item == plane),
            "{plane} not visited"
        );
    }
    assert!(!subject.authoritative);
    assert_eq!(subject.export(), Vec::<String>::new());
}

#[test]
fn derived_deletion_clears_index_memory_and_cache() {
    let mut subject = DataSubject::populated("tn_a", "usr_bob");
    assert!(subject.derived_remaining());
    subject.delete("user").expect("delete");
    assert!(!subject.derived_remaining());
    assert!(subject.planes.iter().all(|plane| !plane.present));
}
