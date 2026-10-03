//! Typed permission status and the guide a missing permission produces (ADP-002, DOMAIN.md §8.6.1 E).
//!
//! Pure logic, no database: what the status looks like on the wire, which permission each action needs,
//! and what the person is told. The behaviour of a run that meets a missing permission (it parks, it is
//! not retried) is in `computer_sequence.rs` against real PostgreSQL; the native probes are in
//! `native/macos` (`PermissionStatusTests`).

use quansio_machine::computer_use::bridge::NativeComputerAction;
use quansio_machine::computer_use::config::ComputerUseConfig;
use quansio_machine::computer_use::permission::{
    drives_foreground, gate, required_permissions, AttentionReason, ForegroundControl, GuideAction,
    Permission, PermissionState, PermissionStatus,
};
use serde_json::{json, Value};

const APP: &str = "com.apple.TextEdit";

fn read(include_screenshot: bool) -> NativeComputerAction {
    NativeComputerAction::Read {
        application: APP.into(),
        max_nodes: 8,
        max_depth: 2,
        include_screenshot,
    }
}

fn click() -> NativeComputerAction {
    NativeComputerAction::Click {
        application: APP.into(),
        x: 1,
        y: 2,
        button: "left".into(),
    }
}

fn type_text() -> NativeComputerAction {
    NativeComputerAction::Type {
        application: APP.into(),
        text: "x".into(),
    }
}

fn key() -> NativeComputerAction {
    NativeComputerAction::SystemKey {
        application: APP.into(),
        keys: vec!["command".into(), "c".into()],
    }
}

fn clipboard() -> NativeComputerAction {
    NativeComputerAction::Clipboard {
        application: APP.into(),
        operation: "write".into(),
        text: "x".into(),
    }
}

#[test]
fn the_status_has_exactly_four_permissions_in_four_states() {
    let status = PermissionStatus {
        accessibility: PermissionState::Granted,
        screen_recording: PermissionState::Denied,
        input_monitoring: PermissionState::NotDetermined,
        microphone: PermissionState::Restricted,
    };
    let wire = status.to_json();
    assert_eq!(
        wire,
        json!({
            "accessibility": "granted",
            "screen_recording": "denied",
            "input_monitoring": "not_determined",
            "microphone": "restricted",
        })
    );
    assert_eq!(PermissionStatus::from_json(&wire), Ok(status));
    assert_eq!(Permission::ALL.len(), 4);
    for state in [
        PermissionState::Granted,
        PermissionState::Denied,
        PermissionState::NotDetermined,
        PermissionState::Restricted,
    ] {
        assert_eq!(PermissionState::parse(state.as_str()), Some(state));
    }
    for permission in Permission::ALL {
        assert_eq!(Permission::parse(permission.as_str()), Some(permission));
    }
    assert_eq!(PermissionState::parse("maybe"), None);
}

#[test]
fn a_status_that_is_only_partly_understood_is_not_used() {
    let good = PermissionStatus::all_granted().to_json();
    // A missing permission is not assumed granted, and an unknown one is not ignored.
    for removed in [
        "accessibility",
        "screen_recording",
        "input_monitoring",
        "microphone",
    ] {
        let mut wire = good.clone();
        wire.as_object_mut().expect("object").remove(removed);
        assert!(PermissionStatus::from_json(&wire).is_err(), "{removed}");
    }
    let mut extra = good.clone();
    extra["camera"] = json!("granted");
    assert!(PermissionStatus::from_json(&extra).is_err());
    let mut unknown = good.clone();
    unknown["accessibility"] = json!("allowed");
    assert!(PermissionStatus::from_json(&unknown).is_err());
    let mut wrong_type = good;
    wrong_type["accessibility"] = json!(true);
    assert!(PermissionStatus::from_json(&wrong_type).is_err());
    assert!(PermissionStatus::from_json(&Value::Null).is_err());
}

#[test]
fn each_action_names_the_permissions_it_needs() {
    assert_eq!(
        required_permissions(&read(false)),
        vec![Permission::Accessibility]
    );
    assert_eq!(
        required_permissions(&read(true)),
        vec![Permission::Accessibility, Permission::ScreenRecording]
    );
    assert_eq!(
        required_permissions(&click()),
        vec![Permission::Accessibility]
    );
    assert_eq!(
        required_permissions(&type_text()),
        vec![Permission::Accessibility]
    );
    assert_eq!(
        required_permissions(&key()),
        vec![Permission::Accessibility]
    );
    assert!(required_permissions(&clipboard()).is_empty());
    // Input actions drive the foreground; reads and the clipboard do not.
    assert!(
        drives_foreground(&click()) && drives_foreground(&type_text()) && drives_foreground(&key())
    );
    assert!(!drives_foreground(&read(true)) && !drives_foreground(&clipboard()));
}

#[test]
fn the_guide_names_the_pane_configuration_states_and_nothing_a_model_wrote() {
    let panes = &ComputerUseConfig::builtin().panes;
    let expected = [
        (Permission::Accessibility, "Privacy_Accessibility"),
        (Permission::ScreenRecording, "Privacy_ScreenCapture"),
        (Permission::InputMonitoring, "Privacy_ListenEvent"),
        (Permission::Microphone, "Privacy_Microphone"),
    ];
    for (permission, pane) in expected {
        assert_eq!(panes.pane(permission), pane);
    }
    for state in [PermissionState::Denied, PermissionState::NotDetermined] {
        let status = PermissionStatus::all_granted().with(Permission::Accessibility, state);
        let attention = gate(&click(), &status, ForegroundControl::Held, panes).expect_err("parks");
        assert_eq!(attention.reason, AttentionReason::PermissionMissing);
        assert_eq!(attention.status, Some(state));
        assert_eq!(
            attention.guide,
            GuideAction::OpenSettings {
                pane: "Privacy_Accessibility".to_string()
            }
        );
        let wire = attention.to_json();
        assert_eq!(wire["code"], json!("COMPUTER_PERMISSION_REQUIRED"));
        assert_eq!(wire["status"], json!(state.as_str()));
        assert_eq!(wire["guide"]["action"], json!("open_settings"));
    }
}

#[test]
fn a_permission_the_action_does_not_need_never_parks_it() {
    let panes = &ComputerUseConfig::builtin().panes;
    // Input Monitoring and Microphone are reported for onboarding; no computer action needs them.
    let status = PermissionStatus::all_granted()
        .with(Permission::InputMonitoring, PermissionState::Denied)
        .with(Permission::Microphone, PermissionState::NotDetermined);
    for action in [read(false), click(), type_text(), key(), clipboard()] {
        assert!(gate(&action, &status, ForegroundControl::Held, panes).is_ok());
    }
}

#[test]
fn a_missing_permission_outranks_a_lapsed_grant_and_the_two_have_different_answers() {
    let panes = &ComputerUseConfig::builtin().panes;
    let missing =
        PermissionStatus::all_granted().with(Permission::Accessibility, PermissionState::Denied);
    // Both wrong: the person is sent to Settings first, since pausing input cannot fix a permission.
    let both = gate(&click(), &missing, ForegroundControl::Lapsed, panes).expect_err("parks");
    assert_eq!(both.reason, AttentionReason::PermissionMissing);
    assert_eq!(both.guide.as_str(), "open_settings");

    // Only the grant lapsed: pause input, and no Settings pane, no permission, no status.
    let lapsed = gate(
        &click(),
        &PermissionStatus::all_granted(),
        ForegroundControl::Lapsed,
        panes,
    )
    .expect_err("parks");
    assert_eq!(lapsed.reason, AttentionReason::ForegroundControlLapsed);
    assert_eq!(lapsed.guide, GuideAction::PauseInput);
    assert_eq!(lapsed.permission, None);
    assert_eq!(lapsed.status, None);
    let wire = lapsed.to_json();
    assert_eq!(wire["code"], json!("COMPUTER_FOREGROUND_CONTROL_LAPSED"));
    assert_eq!(wire["guide"], json!({ "action": "pause_input" }));
    assert_eq!(wire["permission"], Value::Null);

    // Held and everything granted: through.
    assert!(gate(
        &click(),
        &PermissionStatus::all_granted(),
        ForegroundControl::Held,
        panes
    )
    .is_ok());
    // Both parks are delivered as the same existing notification kind and resumed the same way.
    assert_eq!(both.to_json()["notification_kind"], json!("blocked"));
    assert_eq!(lapsed.to_json()["notification_kind"], json!("blocked"));
    assert_eq!(lapsed.to_json()["resume_command"], json!("ResumeRun"));
}

// ------------------------------------------------------------------ the wire shapes other languages parse

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/computer/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    serde_json::from_str(&text).expect("fixture is JSON")
}

#[test]
fn the_attention_items_the_desktop_parses_are_exactly_what_the_runtime_writes() {
    // `apps/desktop/tests/permissions.test.ts` parses these same files, so a renamed field or a changed
    // resume command fails on both sides.
    let panes = &ComputerUseConfig::builtin().panes;
    let denied =
        PermissionStatus::all_granted().with(Permission::Accessibility, PermissionState::Denied);
    let restricted =
        PermissionStatus::all_granted().with(Permission::Accessibility, PermissionState::Restricted);
    let open = gate(&click(), &denied, ForegroundControl::Held, panes).expect_err("parks");
    let admin = gate(&click(), &restricted, ForegroundControl::Held, panes).expect_err("parks");
    let pause = gate(
        &click(),
        &PermissionStatus::all_granted(),
        ForegroundControl::Lapsed,
        panes,
    )
    .expect_err("parks");
    assert_eq!(open.to_json(), fixture("attention_open_settings.json"));
    assert_eq!(admin.to_json(), fixture("attention_restricted.json"));
    assert_eq!(pause.to_json(), fixture("attention_pause_input.json"));
}

#[test]
fn the_status_fixture_is_the_wire_form_the_bridge_and_the_desktop_share() {
    let wire = fixture("permission_status.json");
    let status = PermissionStatus::from_json(&wire).expect("parse");
    assert_eq!(status.to_json(), wire);
    assert_eq!(status.accessibility, PermissionState::Denied);
    assert_eq!(status.screen_recording, PermissionState::Granted);
    assert_eq!(status.input_monitoring, PermissionState::NotDetermined);
    assert_eq!(status.microphone, PermissionState::Restricted);
}
