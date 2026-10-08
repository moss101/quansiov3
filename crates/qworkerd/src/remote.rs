//! The computer's own side of remote control (ADP-009, DOMAIN.md §8.6.2).
//!
//! The server relays, but the computer is where input becomes real, so it checks again and trusts nothing it is sent. [`RemoteHost`]
//! keeps its **own** `Session` from the person's consent given on this computer, re-admits every relayed input against it and against
//! the computer's **own current** readiness (capture, lock, permissions — read from [`ConditionsPort`], never from the message), and only
//! then hands the event to [`InputPort`]. Frames are produced from [`CapturePort`] only while the same machine admits them. Consent
//! lapses, Stop and a lock all act here without waiting for the server.
//!
//! The three ports are the native helper's (ADP-002: Screen Recording to capture, Accessibility to post input, the lock state). They need
//! operating-system permissions a test cannot grant, so the production ports are `BLOCKED_EXTERNAL`; the tests drive this with ports they
//! implement, which proves the rules and not the capture.

use quansio_machine::remote::protocol::{
    ClipFile, Clipboard, Conditions, ConditionsReport, Direction, Frame, InputEvent, InputMessage,
    Scope,
};
use quansio_machine::remote::session::{
    Computer, Decision, Identity, Refusal, Session, StopReason,
};
use quansio_machine::remote::RemoteConfig;
use serde_json::{json, Value};

/// Takes a picture of the screen.
pub trait CapturePort: Send {
    /// A frame as `(width, height, mime, base64 data)`, or `None` when nothing can be captured.
    fn capture(&mut self) -> Option<(u32, u32, String, String)>;
}

/// Posts input to the machine.
pub trait InputPort: Send {
    /// Perform one event. Called only after every check passed.
    fn inject(&mut self, event: &InputEvent);
    /// Put a transfer on the machine's clipboard (text, or files under a name the machine chooses the folder for). Called only after
    /// every check passed.
    fn write_clipboard(&mut self, clip: &Clipboard);
}

/// What the machine says about itself, now.
pub trait ConditionsPort: Send {
    /// The current capture, lock and permission state.
    fn conditions(&mut self) -> Conditions;
}

/// What the person at the computer agreed to.
#[derive(Debug, Clone)]
pub struct Consent {
    /// The bounds in force.
    pub config: RemoteConfig,
    /// This computer.
    pub computer: Computer,
    /// The person at it, who consents (and who must be the computer's owner).
    pub owner: Identity,
    /// The one viewer the consent is for.
    pub viewer: Identity,
    /// What the viewer asked for.
    pub asked: Scope,
    /// What the person allows: at most what was asked.
    pub granted: Scope,
    /// For how long, in seconds.
    pub seconds: i64,
}

/// One viewer's session, from the computer's side.
pub struct RemoteHost<C: CapturePort, I: InputPort, S: ConditionsPort> {
    config: RemoteConfig,
    owner: Identity,
    session: Session,
    capture: C,
    input: I,
    conditions: S,
    frame_seq: u64,
    clip_seq: u64,
    stopped: Option<StopReason>,
}

impl<C: CapturePort, I: InputPort, S: ConditionsPort> RemoteHost<C, I, S> {
    /// The person at this computer said yes (`consent`). This is the person's consent here: it builds the computer's own session,
    /// independent of anything the server holds.
    ///
    /// # Errors
    /// The machine's refusal (disabled, a decision by anyone but the computer's owner, or a grant that is not allowed).
    pub fn consent(
        consent: Consent,
        now_ms: i64,
        capture: C,
        input: I,
        conditions: S,
    ) -> Result<Self, Refusal> {
        let Consent {
            config,
            computer,
            owner,
            viewer,
            asked,
            granted,
            seconds,
        } = consent;
        let mut session = Session::request(&config, computer, viewer, asked, now_ms)?;
        session.decide(
            &config,
            &owner,
            Decision::Grant {
                scope: granted,
                seconds,
            },
            now_ms,
        )?;
        Ok(Self {
            config,
            owner,
            session,
            capture,
            input,
            conditions,
            frame_seq: 0,
            clip_seq: 0,
            stopped: None,
        })
    }

    /// Why the session is over, once it is.
    #[must_use]
    pub const fn stopped(&self) -> Option<StopReason> {
        self.stopped
    }

    fn report(&mut self, now_ms: i64) -> Result<(), Refusal> {
        let target = self.session.computer().target_id.clone();
        let viewer = self.session.viewer().user_id.clone();
        let conditions = self.conditions.conditions();
        let report = ConditionsReport {
            target_id: target,
            viewer_user_id: viewer,
            conditions,
        };
        self.session
            .report(&self.config, &self.owner, &report, now_ms)
    }

    /// The person pressed Stop here.
    pub fn stop(&mut self) {
        if self
            .session
            .stop(Some(&self.owner), StopReason::HostStopped)
            .is_ok()
        {
            self.stopped = Some(StopReason::HostStopped);
        }
    }

    /// The messages to send upstream now: the computer's readiness report, and a frame when it can capture and the machine admits it.
    pub fn outgoing(&mut self, now_ms: i64) -> Vec<Value> {
        if self.session.tick(&self.config, now_ms).is_some() {
            self.stopped = self.session.stop_reason();
        }
        if self.report(now_ms).is_err() {
            return Vec::new();
        }
        let conditions = self.conditions.conditions();
        let target = self.session.computer().target_id.clone();
        let viewer = self.session.viewer().user_id.clone();
        let mut messages = vec![json!({ "remote": {
            "kind": "conditions", "target_id": target, "viewer_user_id": viewer,
            "capture_ready": conditions.capture_ready, "unlocked": conditions.unlocked,
            "screen_recording": conditions.screen_recording.as_str(), "accessibility": conditions.accessibility.as_str(),
        } })];
        // A locked screen or a missing permission captures nothing at all: the machine below would refuse the frame, and the capture
        // itself is not even attempted.
        if conditions.capture_ready && conditions.unlocked {
            if let Some((width, height, mime, data)) = self.capture.capture() {
                let frame = Frame {
                    target_id: target.clone(),
                    viewer_user_id: viewer.clone(),
                    seq: self.frame_seq + 1,
                    width,
                    height,
                    mime: mime.clone(),
                    data: data.clone(),
                };
                if self
                    .session
                    .admit_frame(&self.config, &self.owner, &frame, now_ms)
                    .is_ok()
                {
                    self.frame_seq += 1;
                    messages.push(json!({ "remote": {
                        "kind": "frame", "target_id": target, "viewer_user_id": viewer, "seq": frame.seq,
                        "width": width, "height": height, "mime": mime, "data": data,
                    } }));
                }
            }
        }
        messages
    }

    /// A `remote.input` live frame the server relayed. Injected only when this computer's own session and own current state admit it.
    ///
    /// # Errors
    /// The refusal; nothing was injected.
    pub fn on_input(&mut self, frame: &Value, now_ms: i64) -> Result<(), Refusal> {
        let target = frame
            .get("target_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let seq = frame
            .get("seq")
            .and_then(Value::as_u64)
            .ok_or(Refusal::InvalidInput)?;
        let event: InputEvent =
            serde_json::from_value(frame.get("event").cloned().unwrap_or(Value::Null))
                .map_err(|_| Refusal::InvalidInput)?;
        // The viewer is the one this consent was given to; the server's word is not what makes it so.
        if frame.get("viewer_user_id").and_then(Value::as_str)
            != Some(self.session.viewer().user_id.as_str())
        {
            return Err(Refusal::WrongParty);
        }
        // Refresh the machine's own readiness before judging: a lock a moment ago counts.
        self.report(now_ms)?;
        let viewer = self.session.viewer().clone();
        self.session.admit_input(
            &self.config,
            &viewer,
            &InputMessage {
                target_id: target,
                seq,
                event: event.clone(),
            },
            now_ms,
        )?;
        self.input.inject(&event);
        Ok(())
    }

    /// A `remote.clipboard` live frame toward this computer. Written only when this computer's own session and current state admit it
    /// (a control grant, an unlocked and permitted screen), after the same bounds the server applied are applied again.
    ///
    /// # Errors
    /// The refusal; nothing was written.
    pub fn on_clipboard(&mut self, frame: &Value, now_ms: i64) -> Result<(), Refusal> {
        if frame.get("direction").and_then(Value::as_str) != Some(Direction::ToHost.as_str()) {
            return Err(Refusal::WrongParty);
        }
        let clip = self.clipboard_of(frame, Direction::ToHost)?;
        self.report(now_ms)?;
        let viewer = self.session.viewer().clone();
        self.session
            .admit_clipboard(&self.config, &viewer, &clip, now_ms)?;
        self.input.write_clipboard(&clip);
        Ok(())
    }

    fn clipboard_of(&self, frame: &Value, direction: Direction) -> Result<Clipboard, Refusal> {
        let text = match frame.get("text") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text.clone()),
            Some(_) => return Err(Refusal::InvalidInput),
        };
        let files: Vec<ClipFile> = match frame.get("files") {
            None | Some(Value::Null) => Vec::new(),
            Some(value) => {
                serde_json::from_value(value.clone()).map_err(|_| Refusal::InvalidInput)?
            }
        };
        Ok(Clipboard {
            target_id: frame
                .get("target_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            viewer_user_id: frame
                .get("viewer_user_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            direction,
            seq: frame
                .get("seq")
                .and_then(Value::as_u64)
                .ok_or(Refusal::InvalidInput)?,
            text,
            files,
        })
    }

    /// Hand text to the viewer, when this computer's own session admits it. The message to send upstream.
    ///
    /// # Errors
    /// The refusal; nothing is to be sent.
    pub fn send_clipboard(&mut self, text: &str, now_ms: i64) -> Result<Value, Refusal> {
        self.report(now_ms)?;
        let target = self.session.computer().target_id.clone();
        let viewer = self.session.viewer().user_id.clone();
        let clip = Clipboard {
            target_id: target.clone(),
            viewer_user_id: viewer.clone(),
            direction: Direction::ToViewer,
            seq: self.clip_seq + 1,
            text: Some(text.to_string()),
            files: Vec::new(),
        };
        self.session
            .admit_clipboard(&self.config, &self.owner, &clip, now_ms)?;
        self.clip_seq += 1;
        Ok(
            json!({ "remote": { "kind": "clipboard", "target_id": target, "viewer_user_id": viewer, "direction": "to_viewer", "seq": clip.seq, "text": text } }),
        )
    }
}
