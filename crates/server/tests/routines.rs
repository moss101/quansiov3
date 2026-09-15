//! APP-010 schedule restart and event dedupe.

use quansio_server::control::routines::{fire_key, should_fire, ROUTINE_COMMAND};

#[test]
fn restart_does_not_duplicate_a_scheduled_fire() {
    let key = fire_key("rtn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", "2026-09-15T06:00:00Z");
    let mut seen = Vec::new();
    assert!(should_fire(&seen, &key));
    seen.push(key.clone());
    assert!(!should_fire(&seen, &key), "restart redelivery must not fire twice");
    assert_eq!(ROUTINE_COMMAND, "TriggerRoutineNow");
}
