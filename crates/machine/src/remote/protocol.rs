//! The remote-control wire form (ADP-009, DOMAIN.md §8.6.2), parsed strictly.
//!
//! Everything a viewer or a computer sends upstream over the live-frames WebSocket is one JSON text message of the form
//! `{"remote": {"kind": …}}`. Parsing is the first gate and knows nothing of sessions: an unknown field, a wrong type, a frame past its
//! size bound, bytes that are not the image they claim to be, a coordinate off the screen, a key that is not a key, text with control
//! characters or a modifier named twice is a [`ProtocolError`] before any state is consulted.
//!
//! * **viewer → computer:** `input` — one pointer, scroll, key or text event with a sequence number.
//! * **computer → viewer:** `frame` — one image for one viewer; `conditions` — whether the computer is capturing, unlocked and
//!   permitted, which is what makes input admissible.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use quansio_core::{CanonicalId, Prefix};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::RemoteConfig;

/// What a viewer was granted, and what a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// Watch the screen.
    View,
    /// Watch and drive it.
    Control,
}

impl Scope {
    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Control => "control",
        }
    }

    /// Parse the canonical spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "view" => Some(Self::View),
            "control" => Some(Self::Control),
            _ => None,
        }
    }
}

/// An operating-system permission, as the native bridge reports it (DOMAIN.md §8.6.1 E).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Granted.
    Granted,
    /// The person refused it.
    Denied,
    /// Never asked.
    NotDetermined,
    /// An administrator forbids it.
    Restricted,
}

impl Permission {
    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::NotDetermined => "not_determined",
            Self::Restricted => "restricted",
        }
    }
}

/// What the computer says about itself: the three things that must all be true before input is admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conditions {
    /// Screen capture is running (it has the Screen Recording permission and a stream).
    pub capture_ready: bool,
    /// The screen is not locked.
    pub unlocked: bool,
    /// Screen Recording.
    pub screen_recording: Permission,
    /// Accessibility (posting input).
    pub accessibility: Permission,
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    /// Primary.
    Left,
    /// Secondary.
    Right,
    /// Middle.
    Middle,
}

/// A modifier key held with a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    /// Shift.
    Shift,
    /// Control.
    Control,
    /// Alt / Option.
    Alt,
    /// Command / Windows.
    Meta,
}

/// One input event, as a viewer sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputEvent {
    /// The pointer moved.
    PointerMove {
        /// Pixels from the left of the shared screen.
        x: u32,
        /// Pixels from the top.
        y: u32,
    },
    /// A button went down.
    PointerDown {
        /// Pixels from the left.
        x: u32,
        /// Pixels from the top.
        y: u32,
        /// Which button.
        button: Button,
    },
    /// A button went up.
    PointerUp {
        /// Pixels from the left.
        x: u32,
        /// Pixels from the top.
        y: u32,
        /// Which button.
        button: Button,
    },
    /// The wheel turned.
    Scroll {
        /// Pixels from the left.
        x: u32,
        /// Pixels from the top.
        y: u32,
        /// Horizontal amount.
        dx: i32,
        /// Vertical amount.
        dy: i32,
    },
    /// A key went down.
    KeyDown {
        /// The key.
        key: String,
        /// Modifiers held.
        #[serde(default)]
        modifiers: Vec<Modifier>,
    },
    /// A key went up.
    KeyUp {
        /// The key.
        key: String,
        /// Modifiers held.
        #[serde(default)]
        modifiers: Vec<Modifier>,
    },
    /// Text typed as text.
    Text {
        /// The characters.
        text: String,
    },
}

/// Why a message is not acceptable on its face.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProtocolError(pub String);

fn bad<T>(message: impl Into<String>) -> Result<T, ProtocolError> {
    Err(ProtocolError(message.into()))
}

/// The image kinds a frame may be.
const MIMES: [&str; 3] = ["image/jpeg", "image/png", "image/webp"];

/// Named keys beside a single printable character.
const NAMED_KEYS: [&str; 24] = [
    "Enter",
    "Escape",
    "Tab",
    "Backspace",
    "Delete",
    "Space",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Shift",
    "Control",
    "Alt",
    "Meta",
    "CapsLock",
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
];

fn key_ok(key: &str) -> bool {
    if NAMED_KEYS.contains(&key) {
        return true;
    }
    // F6..F12 too, without a longer table.
    if let Some(number) = key
        .strip_prefix('F')
        .and_then(|digits| digits.parse::<u8>().ok())
    {
        return (1..=12).contains(&number) && !key.starts_with("F0");
    }
    let mut chars = key.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if !c.is_control())
}

/// One frame for one viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The computer the frame is of.
    pub target_id: String,
    /// The viewer it is for.
    pub viewer_user_id: String,
    /// Strictly increasing within the session.
    pub seq: u64,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// `image/jpeg`, `image/png` or `image/webp`.
    pub mime: String,
    /// The image, base64.
    pub data: String,
}

/// The computer's readiness report for one viewer's session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionsReport {
    /// The computer.
    pub target_id: String,
    /// The viewer the report is for.
    pub viewer_user_id: String,
    /// What it says.
    pub conditions: Conditions,
}

/// One input event from the viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputMessage {
    /// The computer it is for.
    pub target_id: String,
    /// Strictly increasing within the session.
    pub seq: u64,
    /// The event.
    pub event: InputEvent,
}

/// Which way a clipboard transfer goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// The viewer puts something on the computer's clipboard.
    ToHost,
    /// The computer hands something to the viewer.
    ToViewer,
}

impl Direction {
    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ToHost => "to_host",
            Self::ToViewer => "to_viewer",
        }
    }
}

/// One file of a clipboard transfer: a plain name and its bytes. Never a path.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipFile {
    /// A plain file name.
    pub name: String,
    /// The bytes, base64.
    pub data: String,
}

/// One clipboard transfer, on its own channel beside input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clipboard {
    /// The computer.
    pub target_id: String,
    /// The viewer the session belongs to.
    pub viewer_user_id: String,
    /// Which way.
    pub direction: Direction,
    /// Strictly increasing per direction within the session.
    pub seq: u64,
    /// Text, when it is text.
    pub text: Option<String>,
    /// Files, when it is files.
    pub files: Vec<ClipFile>,
}

/// A parsed upstream message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upstream {
    /// From the viewer: it is still there.
    Ping {
        /// The computer.
        target_id: String,
    },
    /// From either side, in the direction it names.
    Clipboard(Clipboard),
    /// From the computer.
    Frame(Frame),
    /// From the computer.
    Conditions(ConditionsReport),
    /// From the viewer.
    Input(InputMessage),
}

impl Upstream {
    /// The computer a message is about.
    #[must_use]
    pub fn target_id(&self) -> &str {
        match self {
            Self::Frame(frame) => &frame.target_id,
            Self::Conditions(report) => &report.target_id,
            Self::Input(input) => &input.target_id,
            Self::Ping { target_id } => target_id,
            Self::Clipboard(clip) => &clip.target_id,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFrame {
    target_id: String,
    viewer_user_id: String,
    seq: u64,
    width: u32,
    height: u32,
    mime: String,
    data: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConditions {
    target_id: String,
    viewer_user_id: String,
    capture_ready: bool,
    unlocked: bool,
    screen_recording: Permission,
    accessibility: Permission,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInput {
    target_id: String,
    seq: u64,
    event: InputEvent,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPing {
    target_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClipboard {
    target_id: String,
    viewer_user_id: String,
    direction: Direction,
    seq: u64,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    files: Vec<ClipFile>,
}

/// A file name that is one plain component: no separator, no dot-only name, no control character, bounded.
fn file_name_ok(name: &str) -> bool {
    let count = name.chars().count();
    (1..=128).contains(&count)
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '\u{0}'))
}

fn target(value: &str) -> Result<(), ProtocolError> {
    CanonicalId::parse_typed(value, Prefix::ExecutionTarget)
        .map_or_else(|_| bad("target_id is a tgt_ id"), |_| Ok(()))
}

fn user(value: &str) -> Result<(), ProtocolError> {
    CanonicalId::parse_typed(value, Prefix::User)
        .map_or_else(|_| bad("viewer_user_id is a usr_ id"), |_| Ok(()))
}

/// Magic bytes of the three image kinds.
fn is_image(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/png" => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "image/webp" => bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        _ => false,
    }
}

impl InputEvent {
    /// Check the event against the configuration's bounds. `frame` is the size of the last frame the viewer was shown, so a pointer
    /// cannot name a pixel the viewer never saw.
    ///
    /// # Errors
    /// Returns the bound that is broken.
    pub fn check(
        &self,
        config: &RemoteConfig,
        frame: Option<(u32, u32)>,
    ) -> Result<(), ProtocolError> {
        let inside = |x: u32, y: u32| -> Result<(), ProtocolError> {
            if x > config.coordinate_max || y > config.coordinate_max {
                return bad("a coordinate is past the largest screen");
            }
            if let Some((width, height)) = frame {
                if x >= width || y >= height {
                    return bad("a coordinate is outside the screen the viewer was shown");
                }
            }
            Ok(())
        };
        let modifiers_ok = |modifiers: &[Modifier]| -> Result<(), ProtocolError> {
            let mut seen = std::collections::HashSet::new();
            if modifiers.len() > 4 || !modifiers.iter().all(|modifier| seen.insert(*modifier)) {
                return bad("a modifier is named twice");
            }
            Ok(())
        };
        match self {
            Self::PointerMove { x, y }
            | Self::PointerDown { x, y, .. }
            | Self::PointerUp { x, y, .. } => inside(*x, *y),
            Self::Scroll { x, y, dx, dy } => {
                inside(*x, *y)?;
                if dx.unsigned_abs() > 10_000 || dy.unsigned_abs() > 10_000 {
                    return bad("a scroll amount is past 10000");
                }
                Ok(())
            }
            Self::KeyDown { key, modifiers } | Self::KeyUp { key, modifiers } => {
                if !key_ok(key) {
                    return bad("key is a named key or one printable character");
                }
                modifiers_ok(modifiers)
            }
            Self::Text { text } => {
                let count = text.chars().count();
                if count == 0 || count > config.text_max_chars {
                    return bad(format!("text is 1 to {} characters", config.text_max_chars));
                }
                if text.chars().any(|c| c.is_control() && c != '\n') {
                    return bad("text holds a control character");
                }
                Ok(())
            }
        }
    }
}

impl Clipboard {
    /// Check the transfer against the configuration's bounds: text or files (not both, not neither), the byte and file caps, plain
    /// file names, no duplicate name.
    ///
    /// # Errors
    /// Returns the bound that is broken.
    pub fn check(&self, config: &RemoteConfig) -> Result<(), ProtocolError> {
        match (&self.text, self.files.is_empty()) {
            (Some(_), false) => return bad("a clipboard transfer is text or files, not both"),
            (None, true) => return bad("a clipboard transfer carries text or files"),
            _ => {}
        }
        if let Some(text) = &self.text {
            if text.is_empty() || text.len() > config.clipboard_max_bytes {
                return bad(format!(
                    "clipboard text is 1 to {} bytes",
                    config.clipboard_max_bytes
                ));
            }
            if text.contains('\u{0}') {
                return bad("clipboard text holds a NUL");
            }
        }
        if self.files.len() > config.clipboard_max_files {
            return bad(format!(
                "a transfer carries at most {} files",
                config.clipboard_max_files
            ));
        }
        let mut total = 0_usize;
        let mut names = std::collections::HashSet::new();
        for file in &self.files {
            if !file_name_ok(&file.name) || !names.insert(file.name.as_str()) {
                return bad("a file name is one plain, unique name");
            }
            // The encoded length bounds the decoded one before anything is decoded.
            if file.data.len() > config.clipboard_max_bytes.div_ceil(3) * 4 {
                return bad(format!(
                    "a transfer carries at most {} bytes",
                    config.clipboard_max_bytes
                ));
            }
            let bytes = STANDARD
                .decode(file.data.as_bytes())
                .map_err(|_| ProtocolError("a file's data is base64".to_string()))?;
            total += bytes.len();
            if bytes.is_empty() || total > config.clipboard_max_bytes {
                return bad(format!(
                    "a transfer carries 1 to {} bytes",
                    config.clipboard_max_bytes
                ));
            }
        }
        Ok(())
    }
}

impl Upstream {
    /// Parse one upstream text message.
    ///
    /// # Errors
    /// Returns what is wrong with the message on its face.
    pub fn parse(text: &str, config: &RemoteConfig) -> Result<Self, ProtocolError> {
        if text.len() > config.frame_max_bytes + 4096 {
            return bad("the message is larger than a frame");
        }
        let value: Value = serde_json::from_str(text)
            .map_err(|_| ProtocolError("the message is not JSON".to_string()))?;
        let Value::Object(mut outer) = value else {
            return bad("the message is an object");
        };
        if outer.len() != 1 {
            return bad("the message has one key, remote");
        }
        let Some(Value::Object(mut inner)) = outer.remove("remote") else {
            return bad("the message has one key, remote");
        };
        let Some(Value::String(kind)) = inner.remove("kind") else {
            return bad("remote.kind is required");
        };
        let rest = Value::Object(inner);
        let parsed = |error: serde_json::Error| ProtocolError(format!("a {kind} message: {error}"));
        match kind.as_str() {
            "frame" => {
                let raw: RawFrame = serde_json::from_value(rest).map_err(parsed)?;
                target(&raw.target_id)?;
                user(&raw.viewer_user_id)?;
                if raw.data.len() > config.frame_max_bytes {
                    return bad(format!(
                        "a frame is at most {} bytes",
                        config.frame_max_bytes
                    ));
                }
                if raw.width == 0
                    || raw.height == 0
                    || raw.width > config.coordinate_max
                    || raw.height > config.coordinate_max
                {
                    return bad("a frame has a size inside the largest screen");
                }
                if !MIMES.contains(&raw.mime.as_str()) {
                    return bad("a frame is image/jpeg, image/png or image/webp");
                }
                let bytes = STANDARD
                    .decode(raw.data.as_bytes())
                    .map_err(|_| ProtocolError("a frame's data is base64".to_string()))?;
                if !is_image(&raw.mime, &bytes) {
                    return bad("a frame's bytes are not the image it says it is");
                }
                Ok(Self::Frame(Frame {
                    target_id: raw.target_id,
                    viewer_user_id: raw.viewer_user_id,
                    seq: raw.seq,
                    width: raw.width,
                    height: raw.height,
                    mime: raw.mime,
                    data: raw.data,
                }))
            }
            "conditions" => {
                let raw: RawConditions = serde_json::from_value(rest).map_err(parsed)?;
                target(&raw.target_id)?;
                user(&raw.viewer_user_id)?;
                Ok(Self::Conditions(ConditionsReport {
                    target_id: raw.target_id,
                    viewer_user_id: raw.viewer_user_id,
                    conditions: Conditions {
                        capture_ready: raw.capture_ready,
                        unlocked: raw.unlocked,
                        screen_recording: raw.screen_recording,
                        accessibility: raw.accessibility,
                    },
                }))
            }
            "input" => {
                let raw: RawInput = serde_json::from_value(rest).map_err(parsed)?;
                target(&raw.target_id)?;
                raw.event.check(config, None)?;
                Ok(Self::Input(InputMessage {
                    target_id: raw.target_id,
                    seq: raw.seq,
                    event: raw.event,
                }))
            }
            "ping" => {
                let raw: RawPing = serde_json::from_value(rest).map_err(parsed)?;
                target(&raw.target_id)?;
                Ok(Self::Ping {
                    target_id: raw.target_id,
                })
            }
            "clipboard" => {
                let raw: RawClipboard = serde_json::from_value(rest).map_err(parsed)?;
                target(&raw.target_id)?;
                user(&raw.viewer_user_id)?;
                let clip = Clipboard {
                    target_id: raw.target_id,
                    viewer_user_id: raw.viewer_user_id,
                    direction: raw.direction,
                    seq: raw.seq,
                    text: raw.text,
                    files: raw.files,
                };
                clip.check(config)?;
                Ok(Self::Clipboard(clip))
            }
            _ => bad("remote.kind is frame, conditions, input, ping or clipboard"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
    const VIEWER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0BBBBB";

    fn config() -> RemoteConfig {
        RemoteConfig::builtin()
    }

    fn parse(value: &Value) -> Result<Upstream, ProtocolError> {
        Upstream::parse(&value.to_string(), &config())
    }

    fn jpeg() -> String {
        STANDARD.encode([0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F'])
    }

    fn input(event: Value) -> Value {
        json!({ "remote": { "kind": "input", "target_id": TARGET, "seq": 1, "event": event } })
    }

    #[test]
    fn a_well_formed_message_of_each_kind_parses() {
        let frame = parse(
            &json!({ "remote": { "kind": "frame", "target_id": TARGET, "viewer_user_id": VIEWER, "seq": 1, "width": 1280, "height": 720, "mime": "image/jpeg", "data": jpeg() } }),
        );
        assert!(matches!(frame, Ok(Upstream::Frame(_))), "{frame:?}");
        let conditions = parse(
            &json!({ "remote": { "kind": "conditions", "target_id": TARGET, "viewer_user_id": VIEWER, "capture_ready": true, "unlocked": true, "screen_recording": "granted", "accessibility": "granted" } }),
        );
        assert!(
            matches!(conditions, Ok(Upstream::Conditions(_))),
            "{conditions:?}"
        );
        for event in [
            json!({ "type": "pointer_move", "x": 1, "y": 2 }),
            json!({ "type": "pointer_down", "x": 1, "y": 2, "button": "left" }),
            json!({ "type": "pointer_up", "x": 1, "y": 2, "button": "right" }),
            json!({ "type": "scroll", "x": 1, "y": 2, "dx": 0, "dy": -120 }),
            json!({ "type": "key_down", "key": "a", "modifiers": ["meta", "shift"] }),
            json!({ "type": "key_up", "key": "Enter" }),
            json!({ "type": "key_down", "key": "F12" }),
            json!({ "type": "text", "text": "hello\nworld" }),
        ] {
            assert!(
                matches!(parse(&input(event.clone())), Ok(Upstream::Input(_))),
                "{event}"
            );
        }
    }

    fn clipboard(over: Value) -> Value {
        let mut base = json!({ "kind": "clipboard", "target_id": TARGET, "viewer_user_id": VIEWER, "direction": "to_host", "seq": 1, "text": "hello" });
        for (key, value) in over.as_object().cloned().unwrap_or_default() {
            base[key] = value;
        }
        json!({ "remote": base })
    }

    #[test]
    fn clipboard_and_ping_messages_parse_and_every_bound_is_enforced() {
        assert!(matches!(
            parse(&json!({ "remote": { "kind": "ping", "target_id": TARGET } })),
            Ok(Upstream::Ping { .. })
        ));
        assert!(
            parse(&json!({ "remote": { "kind": "ping", "target_id": TARGET, "x": 1 } })).is_err()
        );
        assert!(matches!(
            parse(&clipboard(json!({}))),
            Ok(Upstream::Clipboard(_))
        ));
        let files = json!({ "text": null, "files": [{ "name": "notes.txt", "data": STANDARD.encode(b"abc") }] });
        assert!(matches!(
            parse(&clipboard(files)),
            Ok(Upstream::Clipboard(_))
        ));
        let max = config().clipboard_max_bytes;
        let file = |name: &str, bytes: usize| json!({ "name": name, "data": STANDARD.encode(vec![7_u8; bytes]) });
        let cases = [
            clipboard(json!({ "text": "" })),
            clipboard(json!({ "text": "x".repeat(max + 1) })),
            clipboard(json!({ "text": "a\u{0000}b" })),
            clipboard(json!({ "text": null })),
            clipboard(json!({ "direction": "sideways" })),
            clipboard(json!({ "extra": 1 })),
            clipboard(json!({ "text": "x", "files": [file("a.txt", 1)] })),
            clipboard(
                json!({ "text": null, "files": [file("a.txt", 1), file("b.txt", 1), file("c.txt", 1)] }),
            ),
            clipboard(json!({ "text": null, "files": [file("a.txt", max + 1)] })),
            clipboard(
                json!({ "text": null, "files": [file("a.txt", max / 2 + 1), file("b.txt", max / 2 + 1)] }),
            ),
            clipboard(json!({ "text": null, "files": [file("a.txt", 0)] })),
            clipboard(json!({ "text": null, "files": [file("a.txt", 1), file("a.txt", 1)] })),
            clipboard(
                json!({ "text": null, "files": [{ "name": "a.txt", "data": "not base64 !!", "x": 1 }] }),
            ),
            clipboard(json!({ "text": null, "files": [{ "name": "a.txt", "data": "***" }] })),
        ];
        for case in cases {
            assert!(parse(&case).is_err(), "{case}");
        }
        for name in [
            "../x",
            "a/b",
            "a\\b",
            ".hidden",
            "..",
            ".",
            "",
            "a:b",
            "a\nb",
            &"n".repeat(129),
        ] {
            assert!(
                parse(&clipboard(
                    json!({ "text": null, "files": [file(name, 1)] })
                ))
                .is_err(),
                "{name:?}"
            );
        }
    }

    #[test]
    fn a_message_that_breaks_the_form_is_refused_before_any_session_is_asked() {
        let frame = |over: Value| {
            let mut base = json!({ "kind": "frame", "target_id": TARGET, "viewer_user_id": VIEWER, "seq": 1, "width": 1280, "height": 720, "mime": "image/jpeg", "data": jpeg() });
            for (key, value) in over.as_object().cloned().unwrap_or_default() {
                base[key] = value;
            }
            json!({ "remote": base })
        };
        let png_as_jpeg = STANDARD.encode([0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0]);
        let cases = [
            json!("text"),
            json!([]),
            json!({}),
            json!({ "remote": {}, "extra": 1 }),
            json!({ "remote": { "kind": "shell", "target_id": TARGET } }),
            json!({ "remote": { "target_id": TARGET } }),
            frame(json!({ "extra": 1 })),
            frame(json!({ "target_id": "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" })),
            frame(json!({ "viewer_user_id": "tgt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" })),
            frame(json!({ "mime": "text/html" })),
            frame(json!({ "mime": "image/png" })),
            frame(json!({ "data": png_as_jpeg })),
            frame(json!({ "data": "not base64 !!!" })),
            frame(json!({ "width": 0 })),
            frame(json!({ "height": 999_999 })),
            frame(json!({ "seq": -1 })),
            frame(json!({ "data": "A".repeat(config().frame_max_bytes + 1) })),
            input(json!({ "type": "pointer_move", "x": 1 })),
            input(json!({ "type": "pointer_move", "x": 1, "y": 2, "z": 3 })),
            input(json!({ "type": "pointer_move", "x": 99_999, "y": 2 })),
            input(json!({ "type": "pointer_down", "x": 1, "y": 2, "button": "back" })),
            input(json!({ "type": "scroll", "x": 1, "y": 2, "dx": 100_000, "dy": 0 })),
            input(json!({ "type": "key_down", "key": "ab" })),
            input(json!({ "type": "key_down", "key": "" })),
            input(json!({ "type": "key_down", "key": "\u{0007}" })),
            input(json!({ "type": "key_down", "key": "F13" })),
            input(json!({ "type": "key_down", "key": "F0" })),
            input(json!({ "type": "key_down", "key": "a", "modifiers": ["shift", "shift"] })),
            input(json!({ "type": "key_down", "key": "a", "modifiers": ["hyper"] })),
            input(json!({ "type": "text", "text": "" })),
            input(json!({ "type": "text", "text": "a\u{0000}b" })),
            input(json!({ "type": "text", "text": "x".repeat(config().text_max_chars + 1) })),
            input(json!({ "type": "open_url", "url": "https://example.com" })),
            json!({ "remote": { "kind": "input", "target_id": TARGET, "seq": 1, "event": { "type": "text", "text": "x" }, "as_host": true } }),
        ];
        for case in cases {
            assert!(parse(&case).is_err(), "{case}");
        }
        assert!(Upstream::parse("{not json", &config()).is_err());
        assert!(Upstream::parse(&" ".repeat(config().frame_max_bytes + 5000), &config()).is_err());
    }

    #[test]
    fn a_pointer_cannot_name_a_pixel_the_viewer_was_never_shown() {
        let event = InputEvent::PointerDown {
            x: 1279,
            y: 719,
            button: Button::Left,
        };
        assert!(event.check(&config(), Some((1280, 720))).is_ok());
        assert!(InputEvent::PointerDown {
            x: 1280,
            y: 0,
            button: Button::Left
        }
        .check(&config(), Some((1280, 720)))
        .is_err());
        assert!(InputEvent::Scroll {
            x: 0,
            y: 720,
            dx: 0,
            dy: 1
        }
        .check(&config(), Some((1280, 720)))
        .is_err());
    }

    #[test]
    fn the_spellings_are_canonical() {
        assert_eq!(Scope::parse("control"), Some(Scope::Control));
        assert!(Scope::View < Scope::Control);
        assert_eq!(Scope::parse("admin"), None);
        assert_eq!(Permission::NotDetermined.as_str(), "not_determined");
    }
}
