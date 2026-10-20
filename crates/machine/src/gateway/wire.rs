//! The wire form of the worker control channel (EXEC-002, DOMAIN.md §8.1–§8.3).
//!
//! One JSON document per line, in both directions, over a stream the **worker opened**: a
//! `customer_private_worker` dials in and the server never dials it ([`super::Dial::AwaitInbound`]).
//! The server speaks [`ToWorker`] (the lease it holds for the target, and dispatches fenced by it) and the
//! worker speaks [`FromWorker`] (who it is, what each dispatch settled as, and what it observes).
//!
//! These are plain data. Authority stays where it was: the worker re-validates every dispatch against the
//! lease it was told ([`super::MachineGateway::validate`]), and the server accepts a result only for a
//! dispatch it sent and still waits for.

use serde::{Deserialize, Serialize};

use super::{ActionEnvelope, EnvelopeKind, LeaseClaim};
use crate::control::browser::ReceiptWire;

/// The most evidence one dispatch may carry back, in bytes before base64 (a viewport screenshot is a fraction of this).
pub const MAX_EVIDENCE_BYTES: usize = 5 * 1024 * 1024;

/// The longest frame either side accepts, so a peer cannot make the other buffer without bound.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// The lease a worker is told it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseWire {
    /// Lease id (`lse_…`).
    pub lease_id: String,
    /// Target the lease is for.
    pub target_id: String,
    /// Generation it was granted at.
    pub generation: i64,
    /// Canonical ISO-8601 UTC expiry.
    pub expires_at: String,
}

impl From<&LeaseClaim> for LeaseWire {
    fn from(claim: &LeaseClaim) -> Self {
        Self {
            lease_id: claim.lease_id.clone(),
            target_id: claim.target_id.clone(),
            generation: claim.generation,
            expires_at: claim.expires_at.clone(),
        }
    }
}

impl From<LeaseWire> for LeaseClaim {
    fn from(wire: LeaseWire) -> Self {
        Self {
            lease_id: wire.lease_id,
            target_id: wire.target_id,
            generation: wire.generation,
            held: true,
            expires_at: wire.expires_at,
        }
    }
}

/// An [`ActionEnvelope`] on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeWire {
    /// `dispatch`, `cancel`, `checkpoint` or `evidence_upload`.
    pub kind: String,
    /// Effect the action settles.
    pub effect_id: String,
    /// Fence token.
    pub dispatch_token: String,
    /// Target.
    pub target_id: String,
    /// Lease the caller held.
    pub lease_id: String,
    /// Generation the lease was granted at.
    pub generation: i64,
    /// Run.
    pub run_id: String,
    /// Step.
    pub step_id: String,
    /// Tool name; empty for kinds that run none.
    pub tool: String,
    /// Declaration version.
    pub tool_version: u32,
    /// Canonical JSON arguments.
    pub args_json: String,
    /// Digest of `args_json`.
    pub args_digest: String,
    /// Output bound.
    pub max_output_bytes: usize,
    /// Deadline.
    pub deadline_at: String,
    /// The highest consequence tier the runtime approved for the call, when it stated one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_tier: Option<u8>,
}

impl From<&ActionEnvelope> for EnvelopeWire {
    fn from(envelope: &ActionEnvelope) -> Self {
        Self {
            kind: envelope.kind.as_str().to_string(),
            effect_id: envelope.effect_id.clone(),
            dispatch_token: envelope.dispatch_token.clone(),
            target_id: envelope.target_id.clone(),
            lease_id: envelope.lease_id.clone(),
            generation: envelope.generation,
            run_id: envelope.run_id.clone(),
            step_id: envelope.step_id.clone(),
            tool: envelope.tool.clone(),
            tool_version: envelope.tool_version,
            args_json: envelope.args_json.clone(),
            args_digest: envelope.args_digest.clone(),
            max_output_bytes: envelope.max_output_bytes,
            deadline_at: envelope.deadline_at.clone(),
            approved_tier: envelope.approved_tier,
        }
    }
}

impl EnvelopeWire {
    /// The envelope this describes.
    ///
    /// # Errors
    /// Returns the unknown kind when it is not one the protocol defines: a worker executes only what it can name.
    pub fn into_envelope(self) -> Result<ActionEnvelope, String> {
        let kind = EnvelopeKind::parse(&self.kind).ok_or(self.kind)?;
        Ok(ActionEnvelope {
            kind,
            effect_id: self.effect_id,
            dispatch_token: self.dispatch_token,
            target_id: self.target_id,
            lease_id: self.lease_id,
            generation: self.generation,
            run_id: self.run_id,
            step_id: self.step_id,
            tool: self.tool,
            tool_version: self.tool_version,
            args_json: self.args_json,
            args_digest: self.args_digest,
            max_output_bytes: self.max_output_bytes,
            deadline_at: self.deadline_at,
            approved_tier: self.approved_tier,
        })
    }
}

/// The named keys a person can send to a browser they hold. Printable characters travel as text. A key may be held with
/// modifiers ([`user_key_allowed`]); the control plane refuses anything else before it reaches a worker, and the worker
/// refuses it again.
pub const USER_KEYS: [&str; 13] = [
    "Enter",
    "Tab",
    "Escape",
    "Backspace",
    "Delete",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "Home",
    "End",
    "PageUp",
    "PageDown",
];

/// The largest scroll a single input may carry, in CSS pixels along either axis.
pub const MAX_USER_SCROLL: f64 = 10_000.0;

/// The modifier keys a person can hold with a key, a click or a drag, in the order a chord names them.
pub const USER_MODIFIERS: [&str; 4] = ["Alt", "Control", "Meta", "Shift"];

/// The mouse buttons a person can click with.
pub const USER_BUTTONS: [&str; 3] = ["left", "middle", "right"];

/// The furthest a viewport point may be, in CSS pixels, on either axis.
pub const MAX_USER_POINT: f64 = 20_000.0;

/// Whether `spec` is a key a person may send: one of [`USER_KEYS`], optionally held with modifiers (`Shift+Tab`), or a
/// chord of one ASCII letter or digit with at least one of `Alt`, `Control` or `Meta` (`Control+a`, `Meta+Shift+z`).
///
/// Modifiers are named once each, in [`USER_MODIFIERS`] order, before the key. A printable character without such a
/// modifier is text, not a key, and a function key (`F12`) or any other name is refused.
#[must_use]
pub fn user_key_allowed(spec: &str) -> bool {
    let mut parts: Vec<&str> = spec.split('+').collect();
    let Some(key) = parts.pop() else {
        return false;
    };
    let mut last = None;
    for modifier in &parts {
        let Some(position) = USER_MODIFIERS.iter().position(|known| known == modifier) else {
            return false;
        };
        if last.is_some_and(|previous| position <= previous) {
            return false;
        }
        last = Some(position);
    }
    if USER_KEYS.contains(&key) {
        return true;
    }
    let mut characters = key.chars();
    let single = matches!((characters.next(), characters.next()), (Some(c), None) if c.is_ascii_alphanumeric());
    single
        && parts
            .iter()
            .any(|modifier| matches!(*modifier, "Alt" | "Control" | "Meta"))
}

/// Whether every name in `modifiers` is one of [`USER_MODIFIERS`], each at most once.
#[must_use]
pub fn user_modifiers_allowed(modifiers: &[String]) -> bool {
    modifiers.len() <= USER_MODIFIERS.len()
        && modifiers
            .iter()
            .all(|modifier| USER_MODIFIERS.contains(&modifier.as_str()))
        && modifiers
            .iter()
            .enumerate()
            .all(|(index, modifier)| !modifiers[..index].contains(modifier))
}

/// Whether `value` is a viewport coordinate a person's input may name.
#[must_use]
pub fn user_point_allowed(value: f64) -> bool {
    value.is_finite() && (0.0..=MAX_USER_POINT).contains(&value)
}

/// An order about a browser session, from the control plane to the worker that runs it (EXEC-009 composition).
///
/// These carry what only the server knows — who holds the session, what a person decided, what a person did while
/// holding it. The worker applies them to the session's driver; the driver still refuses what it must.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "order", rename_all = "snake_case")]
pub enum BrowserOrder {
    /// Who holds the session: `agent` or `user`. Idempotent — the worker moves its driver to match and does nothing if it
    /// already does — so the server states it before an agent dispatch and when a person takes over or hands back.
    Control {
        /// The session.
        session_id: String,
        /// `agent` or `user`.
        holder: String,
        /// Canonical instant.
        at: String,
    },
    /// A user's decision on a certificate error.
    Certificate {
        /// The receipt the control plane minted for the user.
        receipt: ReceiptWire,
    },
    /// A user's decision on a permission.
    Permission {
        /// The receipt the control plane minted for the user.
        receipt: ReceiptWire,
    },
    /// Start or stop relaying frames of the session's current tab. The server asks for them while a person is looking.
    Screencast {
        /// The session.
        session_id: String,
        /// Whether frames should flow.
        enabled: bool,
        /// Also send one picture of the current page now. A viewer who arrives while the stream is already running would
        /// otherwise see nothing until the page changes, because the browser sends a frame only when it does.
        #[serde(default)]
        refresh: bool,
    },
    /// A click the user made while holding the session.
    UserClick {
        /// The session.
        session_id: String,
        /// The tab.
        tab_id: String,
        /// Viewport x.
        x: f64,
        /// Viewport y.
        y: f64,
        /// `left` (the default), `middle` or `right` ([`USER_BUTTONS`]).
        #[serde(default = "left_button")]
        button: String,
        /// The press's click count: 1 for a click (the default), 2 for the second click of a double click, 3 for the third
        /// of a triple click. Each is delivered as one press after the earlier ones.
        #[serde(default = "one_click")]
        click_count: u8,
        /// Modifier keys held during the click ([`USER_MODIFIERS`]).
        #[serde(default)]
        modifiers: Vec<String>,
    },
    /// A drag the user made while holding the session: the left button pressed at one viewport point, moved, and released
    /// at another (selecting text, moving a slider, dragging an item).
    UserDrag {
        /// The session.
        session_id: String,
        /// The tab.
        tab_id: String,
        /// Where the button went down.
        from_x: f64,
        /// See `from_x`.
        from_y: f64,
        /// Where it came up.
        to_x: f64,
        /// See `to_x`.
        to_y: f64,
        /// Modifier keys held during the drag ([`USER_MODIFIERS`]).
        #[serde(default)]
        modifiers: Vec<String>,
    },
    /// A key the user pressed while holding the session, perhaps with modifiers ([`user_key_allowed`]).
    UserKey {
        /// The session.
        session_id: String,
        /// The tab.
        tab_id: String,
        /// The key.
        key: String,
    },
    /// A scroll the user made while holding the session: the wheel turned by `delta_x`, `delta_y` at a viewport point.
    UserScroll {
        /// The session.
        session_id: String,
        /// The tab.
        tab_id: String,
        /// Viewport x of the pointer.
        x: f64,
        /// Viewport y of the pointer.
        y: f64,
        /// Horizontal wheel delta in CSS pixels.
        delta_x: f64,
        /// Vertical wheel delta in CSS pixels.
        delta_y: f64,
    },
    /// Text the user typed while holding the session.
    UserText {
        /// The session.
        session_id: String,
        /// The tab.
        tab_id: String,
        /// The text.
        text: String,
    },
}

impl BrowserOrder {
    /// The session the order is about.
    #[must_use]
    pub fn session_id(&self) -> &str {
        match self {
            Self::Control { session_id, .. }
            | Self::Screencast { session_id, .. }
            | Self::UserClick { session_id, .. }
            | Self::UserKey { session_id, .. }
            | Self::UserScroll { session_id, .. }
            | Self::UserDrag { session_id, .. }
            | Self::UserText { session_id, .. } => session_id,
            Self::Certificate { receipt } | Self::Permission { receipt } => &receipt.session_id,
        }
    }
}

fn left_button() -> String {
    "left".to_string()
}

const fn one_click() -> u8 {
    1
}

/// Server to worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToWorker {
    /// The lease the worker holds now. Sent on connect and on every renewal; a newer one replaces the old.
    Lease(LeaseWire),
    /// One fenced action.
    Dispatch(Box<EnvelopeWire>),
    /// An order about a browser session.
    Browser(BrowserOrder),
    /// Stop a dispatch the worker was sent. One still waiting for its turn ends `cancelled` (nothing ran); one already
    /// running is stopped and ends `interrupted` (it may have done part of its work). A token the worker has already
    /// answered, or never saw, is ignored: the cancel changes nothing that has settled.
    Cancel {
        /// The dispatch to stop.
        dispatch_token: String,
    },
}

/// How a dispatch ended on the worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OutcomeWire {
    /// The tool ran; `output` is its bounded JSON document.
    Completed {
        /// The document, as UTF-8 text.
        output: String,
        /// Whether the worker's sink discarded output.
        truncated: bool,
        /// Evidence reference, when evidence was captured.
        evidence_ref: Option<String>,
    },
    /// The tool ran and failed.
    Failed {
        /// Why.
        detail: String,
    },
    /// The action was cancelled before it ran.
    Cancelled,
    /// The action had started and was stopped by a cancel: part of it may have happened, so its outcome is not known.
    Interrupted {
        /// What was stopped.
        detail: String,
    },
    /// The worker refused to run it: the envelope did not pass the lease it holds. Nothing ran.
    Rejected {
        /// The rule that fired.
        reason: String,
    },
}

/// Worker to server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromWorker {
    /// The first frame: which target this worker serves.
    Hello {
        /// Tenant the worker serves.
        tenant_id: String,
        /// Target it claims to be.
        target_id: String,
    },
    /// What a dispatch settled as.
    Result {
        /// Dispatch the result answers.
        dispatch_token: String,
        /// Effect it settles.
        effect_id: String,
        /// Whether this answered a re-delivery rather than a run.
        replayed: bool,
        /// How it ended.
        outcome: OutcomeWire,
    },
    /// Evidence bytes a dispatch produced (a screenshot), sent immediately before the `Result` that names the dispatch. The
    /// worker holds no credential for the evidence store, so the bytes travel the control channel and the server records
    /// them. Bounded by [`MAX_EVIDENCE_BYTES`]; a worker that has more sends none.
    Evidence {
        /// Dispatch the bytes belong to.
        dispatch_token: String,
        /// Media type (`image/png`).
        media_type: String,
        /// The bytes, base64.
        data_base64: String,
    },
    /// What a browser session's driver reports about its tabs and actions (DOMAIN.md §8.4.1 E): sent after each browser
    /// action and each order. Bounded and parsed strictly by the control plane, which stores it only for a session of
    /// the target this worker holds.
    BrowserGuard {
        /// The session.
        session_id: String,
        /// The driver's guard report.
        report: serde_json::Value,
    },
    /// One picture of a session's tab, sent only while the server asked for the session's frames. Transient: the server
    /// publishes it to whoever is watching and keeps nothing.
    BrowserFrame {
        /// The session.
        session_id: String,
        /// The tab.
        tab_id: String,
        /// The image's media type.
        media_type: String,
        /// The image, base64.
        data: String,
        /// The page's URL when the frame was taken.
        url: String,
        /// The page's viewport size in CSS pixels, which a click's coordinates are in; `0` when unknown.
        #[serde(default)]
        device_width: u32,
        /// See `device_width`.
        #[serde(default)]
        device_height: u32,
        /// Counts up, so a viewer can drop a frame that arrives late.
        sequence: u64,
    },
    /// What the worker observes about itself (DOMAIN.md §8.2). The server renews the lease on a heartbeat it
    /// accepts, so a worker that goes quiet is fenced when its lease lapses, with no timer on the server.
    Heartbeat {
        /// Target the worker believes it is.
        target_id: String,
        /// Generation it believes it holds.
        generation: i64,
        /// The lifecycle state it observes itself in.
        observed_state: String,
        /// Canonical instant of the observation.
        at: String,
    },
}

/// Serialize one frame, newline-terminated.
///
/// # Errors
/// Returns the serializer's error.
pub fn encode<T: Serialize>(frame: &T) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(frame)?;
    line.push('\n');
    Ok(line)
}

/// Parse one frame from a line.
///
/// # Errors
/// Returns a message when the line is over [`MAX_FRAME_BYTES`] or is not a frame of the expected type.
pub fn decode<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, String> {
    if line.len() > MAX_FRAME_BYTES {
        return Err(format!(
            "frame of {} bytes exceeds {MAX_FRAME_BYTES}",
            line.len()
        ));
    }
    serde_json::from_str(line.trim_end()).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_envelope_survives_the_wire_and_an_unknown_kind_does_not() {
        let lease = LeaseClaim {
            lease_id: "lse_1".into(),
            target_id: "tgt_1".into(),
            generation: 2,
            held: true,
            expires_at: "2999-01-01T00:00:00Z".into(),
        };
        let wire = ToWorker::Lease((&lease).into());
        let line = encode(&wire).unwrap();
        assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
        assert_eq!(decode::<ToWorker>(&line).unwrap(), wire);

        let mut envelope = EnvelopeWire {
            kind: "dispatch".into(),
            effect_id: "eff_1".into(),
            dispatch_token: "dsp_1".into(),
            target_id: "tgt_1".into(),
            lease_id: "lse_1".into(),
            generation: 2,
            run_id: "run_1".into(),
            step_id: "stp_1".into(),
            tool: "fs.read".into(),
            tool_version: 1,
            args_json: "{}".into(),
            args_digest: "0".repeat(64),
            max_output_bytes: 10,
            deadline_at: "2999-01-01T00:00:00Z".into(),
            approved_tier: Some(3),
        };
        assert_eq!(
            EnvelopeWire::from(&envelope.clone().into_envelope().unwrap()),
            envelope
        );
        envelope.kind = "format_disk".into();
        assert_eq!(envelope.into_envelope().unwrap_err(), "format_disk");
    }

    #[test]
    fn a_person_s_keys_are_named_keys_or_modified_chords_and_nothing_else() {
        for allowed in [
            "Enter",
            "Shift+Tab",
            "Control+a",
            "Meta+Shift+z",
            "Alt+ArrowLeft",
            "Alt+Control+Delete",
            "Meta+7",
        ] {
            assert!(user_key_allowed(allowed), "{allowed} should be allowed");
        }
        for refused in [
            "a",
            "Shift+a",
            "F12",
            "Control+F12",
            "Control+",
            "Control+Control+a",
            "Shift+Control+a",
            "Hyper+a",
            "Control+é",
            "Control+ab",
            "",
        ] {
            assert!(!user_key_allowed(refused), "{refused} should be refused");
        }
        assert!(user_modifiers_allowed(&["Shift".into(), "Alt".into()]));
        assert!(!user_modifiers_allowed(&["Shift".into(), "Shift".into()]));
        assert!(!user_modifiers_allowed(&["Fn".into()]));
        assert!(user_point_allowed(0.0) && user_point_allowed(MAX_USER_POINT));
        assert!(!user_point_allowed(-1.0) && !user_point_allowed(f64::NAN));
    }

    #[test]
    fn a_click_from_an_older_server_is_a_single_left_click() {
        let order: ToWorker = decode(
            r#"{"type":"browser","order":"user_click","session_id":"s","tab_id":"t","x":1.0,"y":2.0}"#,
        )
        .unwrap_or_else(|why| panic!("{why}"));
        let ToWorker::Browser(BrowserOrder::UserClick {
            button,
            click_count,
            modifiers,
            ..
        }) = order
        else {
            panic!("not a click");
        };
        assert_eq!(
            (button.as_str(), click_count, modifiers.len()),
            ("left", 1, 0)
        );
    }

    #[test]
    fn a_frame_over_the_bound_or_of_the_wrong_shape_is_refused() {
        assert!(decode::<FromWorker>(&"x".repeat(MAX_FRAME_BYTES + 1)).is_err());
        assert!(decode::<FromWorker>("{\"type\":\"lease\"}").is_err());
        assert!(decode::<ToWorker>("not json").is_err());
    }
}
