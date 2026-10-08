//! The computer's own side of remote control (ADP-009, DOMAIN.md 8.6.2): it keeps its **own** session from the person's consent given
//! here and re-admits every relayed input and clipboard transfer against its **own current** state, whatever the server said.
//!
//! Capturing a real screen and posting real input need Screen Recording and Accessibility (ADP-002, `QUANSIO_TEST_MACOS_COMPUTER_USE=1`)
//! and are the native helper's: they are `BLOCKED_EXTERNAL`. The three ports below are test stand-ins that record what reached them;
//! these tests prove the rules (what may reach a port), not the capture or the injection.

use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use quansio_machine::remote::protocol::{Clipboard, Conditions, InputEvent, Permission, Scope};
use quansio_machine::remote::session::{Computer, Identity, Refusal, StopReason};
use quansio_machine::remote::RemoteConfig;
use quansio_qworkerd::remote::{CapturePort, ConditionsPort, Consent, InputPort, RemoteHost};
use serde_json::{json, Value};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0TENANT";
const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const OWNER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0OWNER0";
const VIEWER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0VIEWER";
const STRANGER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0STRNGR";
const T0: i64 = 5_000_000;

#[derive(Default)]
struct Log {
    captures: usize,
    injected: Vec<InputEvent>,
    clipboards: Vec<Clipboard>,
}

struct Capture(Arc<Mutex<Log>>);
impl CapturePort for Capture {
    fn capture(&mut self) -> Option<(u32, u32, String, String)> {
        self.0.lock().unwrap().captures += 1;
        Some((
            1280,
            720,
            "image/jpeg".to_string(),
            STANDARD.encode([0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F']),
        ))
    }
}

struct Input(Arc<Mutex<Log>>);
impl InputPort for Input {
    fn inject(&mut self, event: &InputEvent) {
        self.0.lock().unwrap().injected.push(event.clone());
    }
    fn write_clipboard(&mut self, clip: &Clipboard) {
        self.0.lock().unwrap().clipboards.push(clip.clone());
    }
}

struct State(Arc<Mutex<Conditions>>);
impl ConditionsPort for State {
    fn conditions(&mut self) -> Conditions {
        *self.0.lock().unwrap()
    }
}

struct Rig {
    host: RemoteHost<Capture, Input, State>,
    log: Arc<Mutex<Log>>,
    state: Arc<Mutex<Conditions>>,
}

fn ok() -> Conditions {
    Conditions {
        capture_ready: true,
        unlocked: true,
        screen_recording: Permission::Granted,
        accessibility: Permission::Granted,
    }
}

fn computer() -> Computer {
    Computer {
        tenant_id: TENANT.to_string(),
        workspace_id: "ws_01J8Z3K6F1N8VQ2X5W9Y0WSPACE".to_string(),
        target_id: TARGET.to_string(),
        owner_user_id: OWNER.to_string(),
    }
}

fn rig(asked: Scope, granted: Scope) -> Rig {
    let log = Arc::new(Mutex::new(Log::default()));
    let state = Arc::new(Mutex::new(ok()));
    let host = RemoteHost::consent(
        Consent {
            config: RemoteConfig::builtin(),
            computer: computer(),
            owner: Identity::new(OWNER, Some("fam_host".to_string())),
            viewer: Identity::new(VIEWER, Some("fam_viewer".to_string())),
            asked,
            granted,
            seconds: 600,
        },
        T0,
        Capture(log.clone()),
        Input(log.clone()),
        State(state.clone()),
    )
    .expect("consent");
    Rig { host, log, state }
}

fn input(seq: u64, event: Value) -> Value {
    json!({ "type": "remote.input", "target_id": TARGET, "viewer_user_id": VIEWER, "seq": seq, "event": event })
}

fn click(seq: u64) -> Value {
    input(
        seq,
        json!({ "type": "pointer_down", "x": 10, "y": 10, "button": "left" }),
    )
}

fn clip(seq: u64, text: &str) -> Value {
    json!({ "type": "remote.clipboard", "target_id": TARGET, "viewer_user_id": VIEWER, "direction": "to_host", "seq": seq, "text": text })
}

fn injected(rig: &Rig) -> usize {
    rig.log.lock().unwrap().injected.len()
}

#[test]
fn a_frame_is_taken_only_while_the_screen_is_capturing_and_unlocked() {
    let mut rig = rig(Scope::Control, Scope::Control);
    let messages = rig.host.outgoing(T0 + 1000);
    assert_eq!(messages.len(), 2, "a readiness report and a frame");
    assert_eq!(messages[1]["remote"]["kind"], "frame");
    assert_eq!(rig.log.lock().unwrap().captures, 1);
    // Locked: the report says so, and the screen is not even captured.
    rig.state.lock().unwrap().unlocked = false;
    let locked = rig.host.outgoing(T0 + 2000);
    assert_eq!(locked.len(), 1);
    assert_eq!(locked[0]["remote"]["unlocked"], false);
    assert_eq!(
        rig.log.lock().unwrap().captures,
        1,
        "nothing was captured while locked"
    );
    // Capture not running: same.
    *rig.state.lock().unwrap() = Conditions {
        capture_ready: false,
        ..ok()
    };
    assert_eq!(rig.host.outgoing(T0 + 3000).len(), 1);
    assert_eq!(rig.log.lock().unwrap().captures, 1);
    // Back: frames resume with a higher number.
    *rig.state.lock().unwrap() = ok();
    let again = rig.host.outgoing(T0 + 4000);
    assert_eq!(again[1]["remote"]["seq"], 2);
}

#[test]
fn input_reaches_the_machine_only_when_this_computer_itself_admits_it() {
    let mut rig = rig(Scope::Control, Scope::Control);
    // Before a first frame there is nothing to point at.
    assert_eq!(
        rig.host.on_input(&click(1), T0 + 500),
        Err(Refusal::NoFrame)
    );
    rig.host.outgoing(T0 + 1000);
    assert_eq!(rig.host.on_input(&click(1), T0 + 1100), Ok(()));
    assert_eq!(injected(&rig), 1);
    // The relayed message names another viewer, or the same one again, or something outside the shown screen: refused, nothing injected.
    let mut other = click(2);
    other["viewer_user_id"] = json!(STRANGER);
    assert_eq!(
        rig.host.on_input(&other, T0 + 1200),
        Err(Refusal::WrongParty)
    );
    assert_eq!(
        rig.host.on_input(&click(1), T0 + 1300),
        Err(Refusal::StaleSeq)
    );
    assert_eq!(
        rig.host.on_input(
            &input(3, json!({ "type": "pointer_move", "x": 5000, "y": 5 })),
            T0 + 1400
        ),
        Err(Refusal::InvalidInput)
    );
    assert_eq!(
        rig.host.on_input(
            &input(
                4,
                json!({ "type": "open_url", "url": "https://example.com" })
            ),
            T0 + 1500
        ),
        Err(Refusal::InvalidInput)
    );
    assert_eq!(rig.host.on_input(&json!({ "type": "remote.input", "target_id": TARGET, "viewer_user_id": VIEWER, "event": {} }), T0 + 1600), Err(Refusal::InvalidInput));
    assert_eq!(injected(&rig), 1);
    // The server would still relay it; the machine's own current state is what decides. A lock a moment ago counts at once.
    rig.state.lock().unwrap().unlocked = false;
    assert_eq!(
        rig.host.on_input(&click(5), T0 + 1700),
        Err(Refusal::Locked)
    );
    *rig.state.lock().unwrap() = Conditions {
        accessibility: Permission::Denied,
        ..ok()
    };
    assert_eq!(
        rig.host.on_input(&click(6), T0 + 1800),
        Err(Refusal::PermissionMissing("accessibility"))
    );
    *rig.state.lock().unwrap() = Conditions {
        capture_ready: false,
        ..ok()
    };
    assert_eq!(
        rig.host.on_input(&click(7), T0 + 1900),
        Err(Refusal::CaptureNotReady)
    );
    assert_eq!(
        injected(&rig),
        1,
        "nothing reached the machine while it could not take input"
    );
    // Ready again: input resumes, and none of what was refused was kept to be replayed.
    *rig.state.lock().unwrap() = ok();
    assert_eq!(rig.host.on_input(&click(8), T0 + 2000), Ok(()));
    assert_eq!(injected(&rig), 2);
}

#[test]
fn a_view_grant_takes_no_input_and_no_clipboard() {
    let mut rig = rig(Scope::Control, Scope::View);
    rig.host.outgoing(T0 + 1000);
    assert_eq!(
        rig.host.on_input(&click(1), T0 + 1100),
        Err(Refusal::ViewOnly)
    );
    assert_eq!(
        rig.host.on_clipboard(&clip(1, "x"), T0 + 1100),
        Err(Refusal::ViewOnly)
    );
    assert_eq!(
        rig.host.send_clipboard("x", T0 + 1100),
        Err(Refusal::ViewOnly)
    );
    assert_eq!(injected(&rig), 0);
    assert!(rig.log.lock().unwrap().clipboards.is_empty());
}

#[test]
fn stop_a_lapsed_grant_and_an_idle_viewer_each_end_it_here_without_the_server() {
    let mut stopped = rig(Scope::Control, Scope::Control);
    stopped.host.outgoing(T0 + 1000);
    stopped.host.stop();
    assert_eq!(stopped.host.stopped(), Some(StopReason::HostStopped));
    assert!(matches!(
        stopped.host.on_input(&click(1), T0 + 1100),
        Err(Refusal::Stopped(StopReason::HostStopped))
    ));
    assert!(
        stopped.host.outgoing(T0 + 1200).is_empty(),
        "a stopped session sends no report and no frame"
    );
    assert_eq!(injected(&stopped), 0);
    let captures = stopped.log.lock().unwrap().captures;
    assert_eq!(captures, 1);

    let mut lapsed = rig(Scope::Control, Scope::Control);
    lapsed.host.outgoing(T0 + 1000);
    assert!(matches!(
        lapsed.host.on_input(&click(1), T0 + 601_000),
        Err(Refusal::Stopped(StopReason::SessionExpired))
    ));
    assert_eq!(injected(&lapsed), 0);

    let idle_after = RemoteConfig::builtin().session_idle_seconds * 1000;
    let mut idle = rig(Scope::Control, Scope::Control);
    idle.host.outgoing(T0 + 1000);
    assert!(idle.host.outgoing(T0 + 1000 + idle_after).is_empty());
    assert_eq!(idle.host.stopped(), Some(StopReason::Idle));
}

#[test]
fn the_clipboard_is_written_only_when_this_computer_admits_it_and_bounded_here_too() {
    let mut rig = rig(Scope::Control, Scope::Control);
    rig.host.outgoing(T0 + 1000);
    assert_eq!(rig.host.on_clipboard(&clip(1, "hello"), T0 + 1100), Ok(()));
    assert_eq!(rig.log.lock().unwrap().clipboards.len(), 1);
    // Wrong direction, wrong viewer, repeated number, oversize, a path-like name: each refused, nothing written.
    let mut wrong_way = clip(2, "x");
    wrong_way["direction"] = json!("to_viewer");
    assert_eq!(
        rig.host.on_clipboard(&wrong_way, T0 + 1200),
        Err(Refusal::WrongParty)
    );
    let mut other = clip(2, "x");
    other["viewer_user_id"] = json!(STRANGER);
    assert_eq!(
        rig.host.on_clipboard(&other, T0 + 1300),
        Err(Refusal::WrongParty)
    );
    assert_eq!(
        rig.host.on_clipboard(&clip(1, "again"), T0 + 1400),
        Err(Refusal::StaleSeq)
    );
    let big = "x".repeat(RemoteConfig::builtin().clipboard_max_bytes + 1);
    assert_eq!(
        rig.host.on_clipboard(&clip(3, &big), T0 + 1500),
        Err(Refusal::InvalidInput)
    );
    let mut path = clip(4, "");
    path["text"] = Value::Null;
    path["files"] = json!([{ "name": "../x", "data": STANDARD.encode(b"x") }]);
    assert_eq!(
        rig.host.on_clipboard(&path, T0 + 2600),
        Err(Refusal::InvalidInput)
    );
    rig.state.lock().unwrap().unlocked = false;
    assert_eq!(
        rig.host.on_clipboard(&clip(5, "locked"), T0 + 3700),
        Err(Refusal::Locked)
    );
    assert_eq!(rig.log.lock().unwrap().clipboards.len(), 1);
    // The computer hands text to the viewer under the same rules.
    *rig.state.lock().unwrap() = ok();
    let sent = rig
        .host
        .send_clipboard("copied here", T0 + 4800)
        .expect("sent");
    assert_eq!(sent["remote"]["direction"], "to_viewer");
    assert_eq!(sent["remote"]["seq"], 1);
    rig.state.lock().unwrap().unlocked = false;
    assert_eq!(
        rig.host.send_clipboard("secret", T0 + 5900),
        Err(Refusal::Locked)
    );
}

#[test]
fn a_consent_the_machine_would_not_give_is_refused() {
    let log = Arc::new(Mutex::new(Log::default()));
    let state = Arc::new(Mutex::new(ok()));
    let host = |owner: &str, asked, granted, seconds| {
        RemoteHost::consent(
            Consent {
                config: RemoteConfig::builtin(),
                computer: computer(),
                owner: Identity::new(owner, None),
                viewer: Identity::new(VIEWER, None),
                asked,
                granted,
                seconds,
            },
            T0,
            Capture(log.clone()),
            Input(log.clone()),
            State(state.clone()),
        )
        .err()
    };
    assert_eq!(
        host(OWNER, Scope::View, Scope::Control, 60),
        Some(Refusal::Overgrant),
        "a grant never widens the request"
    );
    assert_eq!(
        host(OWNER, Scope::Control, Scope::Control, 999_999),
        Some(Refusal::Overgrant),
        "a grant is bounded in time"
    );
    assert_eq!(
        host(STRANGER, Scope::Control, Scope::Control, 60),
        Some(Refusal::WrongParty),
        "only the computer's owner consents"
    );
}

/// Capturing the real screen and posting real input are the native helper's and need operating-system permissions no test can grant
/// (ADP-002). The production `CapturePort`, `InputPort` and `ConditionsPort` over that helper are not built here, so this suite
/// reports the boundary honestly instead of passing for it.
#[test]
fn real_capture_and_injection_are_blocked_external() {
    println!(
        "BLOCKED_EXTERNAL: remote control on a real screen needs the native helper's capture and input ports (ADP-002) and \
         QUANSIO_TEST_MACOS_COMPUTER_USE=1 with Screen Recording and Accessibility granted; the ports are not built, nothing stands in"
    );
}
