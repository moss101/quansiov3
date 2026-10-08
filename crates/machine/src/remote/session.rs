//! The remote-control authorization state machine (ADP-009, DOMAIN.md §8.6.2).
//!
//! One [`Session`] is one viewer on one native computer. It is `Requested` until the person at the computer decides (or the request
//! lapses), `Authorized` only by that person's grant, and `Stopped` for good: a stopped session is never revived, a new request is a
//! new session. Four rules are the point, and each has a test:
//!
//! * **Only the person at the computer decides**, and only that computer's owner: the viewer cannot grant itself, the grant can narrow
//!   a request (control to view) and never widen it, and it is bounded in time.
//! * **Per-viewer.** The viewer is an account and the sign-in it asked with (`session_family`): the same account signed in elsewhere is
//!   another party, and so is anyone else. Frames go only to that viewer; input is accepted only from it.
//! * **Input only when the computer says it can take it.** Control scope, and a readiness report that is fresh and says capture is
//!   ready, the screen is unlocked and Screen Recording and Accessibility are granted. A computer that stops reporting stops admitting
//!   input; a locked screen admits no input and no frames.
//! * **Nothing is queued.** A message that is not admitted now is refused with a [`Refusal`]; there is no backlog to replay later
//!   when the conditions come back.
//!
//! No I/O and no clock: callers pass `now_ms` (Unix milliseconds).

use super::protocol::{
    Clipboard, Conditions, ConditionsReport, Direction, Frame, InputMessage, Permission, Scope,
};
use super::RemoteConfig;

/// An account and the sign-in it is acting through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The signed-in person (`usr_…`).
    pub user_id: String,
    /// The sign-in (session family) the call came with; `None` only where the deployment has no sessions (trusted-header development).
    pub session_family: Option<String>,
}

impl Identity {
    /// An identity.
    #[must_use]
    pub fn new(user_id: impl Into<String>, session_family: Option<String>) -> Self {
        Self {
            user_id: user_id.into(),
            session_family,
        }
    }
}

/// Where a session is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Waiting for the person at the computer.
    Requested,
    /// The person said yes.
    Authorized,
    /// Over.
    Stopped,
}

impl State {
    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Authorized => "authorized",
            Self::Stopped => "stopped",
        }
    }
}

/// Why a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The person at the computer said no.
    Denied,
    /// The request waited too long.
    ConsentExpired,
    /// The person at the computer stopped it (the Stop control).
    HostStopped,
    /// The viewer stopped it.
    ViewerStopped,
    /// The grant ran out, or the session reached its longest.
    SessionExpired,
    /// The viewer sat with no activity (no input, ping or transfer) for the idle limit.
    Idle,
    /// The viewer's sign-in ended.
    ViewerSignedOut,
    /// The computer went away.
    HostLost,
    /// The agent took input back, so a person cannot hold it.
    AgentResumed,
    /// The feature was switched off.
    Disabled,
}

impl StopReason {
    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Denied => "denied",
            Self::ConsentExpired => "consent_expired",
            Self::HostStopped => "host_stopped",
            Self::ViewerStopped => "viewer_stopped",
            Self::SessionExpired => "session_expired",
            Self::Idle => "idle",
            Self::ViewerSignedOut => "viewer_signed_out",
            Self::HostLost => "host_lost",
            Self::AgentResumed => "agent_resumed",
            Self::Disabled => "disabled",
        }
    }
}

/// Why a message or a decision was not admitted. Stable spellings: the relay answers with them and tests hold them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The feature is switched off.
    Disabled,
    /// The sender is not the party this message belongs to (a viewer sending frames, a stranger sending input, the same account on another sign-in).
    WrongParty,
    /// The session is not `Authorized` (still requested, or over).
    NotAuthorized,
    /// The session is over, and why.
    Stopped(StopReason),
    /// The decision came after the request lapsed.
    Expired,
    /// The grant is for view and the message is input.
    ViewOnly,
    /// The grant asked for more than the request, or for longer than allowed.
    Overgrant,
    /// The computer has not reported (or not recently), or reports that capture is not ready.
    CaptureNotReady,
    /// The screen is locked.
    Locked,
    /// A permission the message needs is not granted.
    PermissionMissing(&'static str),
    /// No frame has been shown to the viewer yet, so there is nothing to point at.
    NoFrame,
    /// Too fast.
    RateLimited,
    /// The sequence number did not increase.
    StaleSeq,
    /// The event breaks a bound.
    InvalidInput,
    /// Another session already holds the computer.
    Busy,
}

impl Refusal {
    /// The canonical spelling.
    #[must_use]
    pub fn as_str(self) -> String {
        match self {
            Self::Disabled => "disabled".to_string(),
            Self::WrongParty => "wrong_party".to_string(),
            Self::NotAuthorized => "not_authorized".to_string(),
            Self::Stopped(reason) => format!("stopped:{}", reason.as_str()),
            Self::Expired => "expired".to_string(),
            Self::ViewOnly => "view_only".to_string(),
            Self::Overgrant => "overgrant".to_string(),
            Self::CaptureNotReady => "capture_not_ready".to_string(),
            Self::Locked => "locked".to_string(),
            Self::PermissionMissing(which) => format!("permission_missing:{which}"),
            Self::NoFrame => "no_frame".to_string(),
            Self::RateLimited => "rate_limited".to_string(),
            Self::StaleSeq => "stale_seq".to_string(),
            Self::InvalidInput => "invalid_input".to_string(),
            Self::Busy => "busy".to_string(),
        }
    }
}

/// A person's answer to a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Allow, at this scope (at most what was asked) for this many seconds (at most the configured grant).
    Grant {
        /// What is allowed.
        scope: Scope,
        /// For how long.
        seconds: i64,
    },
    /// Refuse.
    Deny,
}

/// What a call changed, for the caller to record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    /// Before.
    pub from: State,
    /// After.
    pub to: State,
    /// Set when the session ended.
    pub stop: Option<StopReason>,
}

/// One second's count, for rate limits.
#[derive(Debug, Clone, Copy, Default)]
struct Window {
    start_ms: i64,
    count: u32,
}

impl Window {
    fn admit(&mut self, now_ms: i64, limit: u32) -> bool {
        if now_ms - self.start_ms >= 1000 || now_ms < self.start_ms {
            self.start_ms = now_ms;
            self.count = 0;
        }
        if self.count >= limit {
            return false;
        }
        self.count += 1;
        true
    }
}

/// The computer a session is on, as the caller (the server) resolved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Computer {
    /// The tenant.
    pub tenant_id: String,
    /// The workspace.
    pub workspace_id: String,
    /// The target (`tgt_…`).
    pub target_id: String,
    /// The person who owns the computer and alone may decide.
    pub owner_user_id: String,
}

/// One viewer on one computer.
#[derive(Debug, Clone)]
pub struct Session {
    computer: Computer,
    viewer: Identity,
    host_family: Option<String>,
    requested: Scope,
    granted: Option<Scope>,
    state: State,
    stop: Option<StopReason>,
    consent_expires_ms: i64,
    ends_ms: i64,
    last_activity_ms: i64,
    conditions: Option<(Conditions, i64)>,
    last_frame: Option<(u64, u32, u32)>,
    last_input_seq: Option<u64>,
    last_clip_seq: [Option<u64>; 2],
    clips: Window,
    frames: Window,
    inputs: Window,
}

impl Session {
    /// A viewer asks.
    ///
    /// # Errors
    /// [`Refusal::Disabled`] when the feature is off.
    pub fn request(
        config: &RemoteConfig,
        computer: Computer,
        viewer: Identity,
        scope: Scope,
        now_ms: i64,
    ) -> Result<Self, Refusal> {
        if !config.enabled {
            return Err(Refusal::Disabled);
        }
        Ok(Self {
            computer,
            viewer,
            host_family: None,
            requested: scope,
            granted: None,
            state: State::Requested,
            stop: None,
            consent_expires_ms: now_ms + config.consent_ttl_seconds * 1000,
            ends_ms: 0,
            last_activity_ms: now_ms,
            conditions: None,
            last_frame: None,
            last_input_seq: None,
            last_clip_seq: [None, None],
            clips: Window::default(),
            frames: Window::default(),
            inputs: Window::default(),
        })
    }

    /// The computer.
    #[must_use]
    pub const fn computer(&self) -> &Computer {
        &self.computer
    }

    /// The viewer.
    #[must_use]
    pub const fn viewer(&self) -> &Identity {
        &self.viewer
    }

    /// Where it is.
    #[must_use]
    pub const fn state(&self) -> State {
        self.state
    }

    /// Why it ended.
    #[must_use]
    pub const fn stop_reason(&self) -> Option<StopReason> {
        self.stop
    }

    /// What was asked for.
    #[must_use]
    pub const fn requested(&self) -> Scope {
        self.requested
    }

    /// What was granted, once it was.
    #[must_use]
    pub const fn granted(&self) -> Option<Scope> {
        self.granted
    }

    /// When the request lapses (while `Requested`).
    #[must_use]
    pub const fn consent_expires_ms(&self) -> i64 {
        self.consent_expires_ms
    }

    /// When the grant ends (once `Authorized`).
    #[must_use]
    pub const fn ends_ms(&self) -> i64 {
        self.ends_ms
    }

    /// The sign-in of the computer's side, fixed by the grant.
    #[must_use]
    pub fn host_family(&self) -> Option<&str> {
        self.host_family.as_deref()
    }

    /// The last readiness report the computer made.
    #[must_use]
    pub fn conditions(&self) -> Option<Conditions> {
        self.conditions.map(|(conditions, _)| conditions)
    }

    fn viewer_is(&self, who: &Identity) -> bool {
        who == &self.viewer
    }

    fn host_is(&self, who: &Identity) -> bool {
        who.user_id == self.computer.owner_user_id
            && (self.host_family.is_none() || who.session_family == self.host_family)
    }

    fn stopped(&mut self, reason: StopReason) -> Transition {
        let from = self.state;
        self.state = State::Stopped;
        self.stop = Some(reason);
        Transition {
            from,
            to: State::Stopped,
            stop: Some(reason),
        }
    }

    /// Advance the clock: a request that waited too long, a grant that ran out, a session with no viewer activity.
    pub fn tick(&mut self, config: &RemoteConfig, now_ms: i64) -> Option<Transition> {
        match self.state {
            State::Requested if now_ms >= self.consent_expires_ms => {
                Some(self.stopped(StopReason::ConsentExpired))
            }
            State::Authorized if now_ms >= self.ends_ms => {
                Some(self.stopped(StopReason::SessionExpired))
            }
            // A viewer who stopped sending (input, a ping, a clipboard transfer) is a viewer who left: any grant ends, not only control.
            State::Authorized
                if now_ms - self.last_activity_ms >= config.session_idle_seconds * 1000 =>
            {
                Some(self.stopped(StopReason::Idle))
            }
            _ => None,
        }
    }

    /// The person at the computer answers.
    ///
    /// # Errors
    /// [`Refusal::WrongParty`] unless the caller is the computer's owner; [`Refusal::Stopped`] / [`Refusal::Expired`] when it is over or
    /// lapsed; [`Refusal::NotAuthorized`] when it was already decided; [`Refusal::Overgrant`] for more than was asked or longer than allowed.
    pub fn decide(
        &mut self,
        config: &RemoteConfig,
        by: &Identity,
        decision: Decision,
        now_ms: i64,
    ) -> Result<Transition, Refusal> {
        if by.user_id != self.computer.owner_user_id {
            return Err(Refusal::WrongParty);
        }
        if let Some(reason) = self.stop {
            return Err(Refusal::Stopped(reason));
        }
        if let Some(lapsed) = self.tick(config, now_ms) {
            return Err(if lapsed.stop == Some(StopReason::ConsentExpired) {
                Refusal::Expired
            } else {
                Refusal::Stopped(lapsed.stop.unwrap_or(StopReason::SessionExpired))
            });
        }
        if self.state != State::Requested {
            return Err(Refusal::NotAuthorized);
        }
        match decision {
            Decision::Deny => Ok(self.stopped(StopReason::Denied)),
            Decision::Grant { scope, seconds } => {
                if scope > self.requested || seconds <= 0 || seconds > config.grant_max_seconds {
                    return Err(Refusal::Overgrant);
                }
                self.granted = Some(scope);
                self.host_family.clone_from(&by.session_family);
                self.state = State::Authorized;
                self.ends_ms = now_ms + seconds.min(config.session_max_seconds) * 1000;
                self.last_activity_ms = now_ms;
                Ok(Transition {
                    from: State::Requested,
                    to: State::Authorized,
                    stop: None,
                })
            }
        }
    }

    /// Someone stops it: the person at the computer, the viewer, or the system (`None`).
    ///
    /// # Errors
    /// [`Refusal::WrongParty`] for anyone else; [`Refusal::Stopped`] when it is already over.
    pub fn stop(
        &mut self,
        by: Option<&Identity>,
        reason: StopReason,
    ) -> Result<Transition, Refusal> {
        if let Some(reason) = self.stop {
            return Err(Refusal::Stopped(reason));
        }
        if let Some(who) = by {
            let reason_by_party = if who.user_id == self.computer.owner_user_id
                && (self.host_family.is_none() || who.session_family == self.host_family)
            {
                Some(StopReason::HostStopped)
            } else if self.viewer_is(who) {
                Some(StopReason::ViewerStopped)
            } else {
                None
            };
            let Some(actual) = reason_by_party else {
                return Err(Refusal::WrongParty);
            };
            return Ok(self.stopped(actual));
        }
        Ok(self.stopped(reason))
    }

    /// The computer reports whether it is capturing, unlocked and permitted.
    ///
    /// # Errors
    /// [`Refusal::WrongParty`] unless it is the computer's own side; [`Refusal::NotAuthorized`] unless `Authorized`.
    pub fn report(
        &mut self,
        config: &RemoteConfig,
        by: &Identity,
        report: &ConditionsReport,
        now_ms: i64,
    ) -> Result<(), Refusal> {
        self.gate_clock(config, now_ms)?;
        if !self.host_is(by) {
            return Err(Refusal::WrongParty);
        }
        if self.state != State::Authorized {
            return Err(Refusal::NotAuthorized);
        }
        self.conditions = Some((report.conditions, now_ms));
        Ok(())
    }

    fn gate_clock(&mut self, config: &RemoteConfig, now_ms: i64) -> Result<(), Refusal> {
        self.tick(config, now_ms);
        match self.stop {
            Some(reason) => Err(Refusal::Stopped(reason)),
            None => Ok(()),
        }
    }

    /// The conditions as they stand for admitting something now, or the reason they are not met.
    fn ready(&self, config: &RemoteConfig, now_ms: i64, need_input: bool) -> Result<(), Refusal> {
        let Some((conditions, at)) = self.conditions else {
            return Err(Refusal::CaptureNotReady);
        };
        if now_ms - at > config.host_report_max_age_seconds * 1000
            || now_ms < at
            || !conditions.capture_ready
        {
            return Err(Refusal::CaptureNotReady);
        }
        if !conditions.unlocked {
            return Err(Refusal::Locked);
        }
        if conditions.screen_recording != Permission::Granted {
            return Err(Refusal::PermissionMissing("screen_recording"));
        }
        if need_input && conditions.accessibility != Permission::Granted {
            return Err(Refusal::PermissionMissing("accessibility"));
        }
        Ok(())
    }

    /// A frame from the computer, for the viewer.
    ///
    /// # Errors
    /// [`Refusal::WrongParty`] unless it is the computer's own side and the frame names this viewer; [`Refusal::NotAuthorized`],
    /// [`Refusal::CaptureNotReady`], [`Refusal::Locked`], [`Refusal::PermissionMissing`], [`Refusal::StaleSeq`], [`Refusal::RateLimited`].
    pub fn admit_frame(
        &mut self,
        config: &RemoteConfig,
        by: &Identity,
        frame: &Frame,
        now_ms: i64,
    ) -> Result<(), Refusal> {
        self.gate_clock(config, now_ms)?;
        if !self.host_is(by)
            || frame.viewer_user_id != self.viewer.user_id
            || frame.target_id != self.computer.target_id
        {
            return Err(Refusal::WrongParty);
        }
        if self.state != State::Authorized {
            return Err(Refusal::NotAuthorized);
        }
        self.ready(config, now_ms, false)?;
        if self.last_frame.is_some_and(|(seq, _, _)| frame.seq <= seq) {
            return Err(Refusal::StaleSeq);
        }
        if !self.frames.admit(now_ms, config.frame_max_per_second) {
            return Err(Refusal::RateLimited);
        }
        self.last_frame = Some((frame.seq, frame.width, frame.height));
        Ok(())
    }

    /// An input event from the viewer, for the computer.
    ///
    /// # Errors
    /// [`Refusal::WrongParty`] unless it is exactly the viewer (account and sign-in); [`Refusal::NotAuthorized`]; [`Refusal::ViewOnly`];
    /// [`Refusal::CaptureNotReady`], [`Refusal::Locked`], [`Refusal::PermissionMissing`]; [`Refusal::NoFrame`]; [`Refusal::StaleSeq`];
    /// [`Refusal::RateLimited`]; [`Refusal::InvalidInput`].
    pub fn admit_input(
        &mut self,
        config: &RemoteConfig,
        by: &Identity,
        input: &InputMessage,
        now_ms: i64,
    ) -> Result<(), Refusal> {
        self.gate_clock(config, now_ms)?;
        if !self.viewer_is(by) || input.target_id != self.computer.target_id {
            return Err(Refusal::WrongParty);
        }
        if self.state != State::Authorized {
            return Err(Refusal::NotAuthorized);
        }
        if self.granted != Some(Scope::Control) {
            return Err(Refusal::ViewOnly);
        }
        self.ready(config, now_ms, true)?;
        let Some((_, width, height)) = self.last_frame else {
            return Err(Refusal::NoFrame);
        };
        if self.last_input_seq.is_some_and(|seq| input.seq <= seq) {
            return Err(Refusal::StaleSeq);
        }
        if input.event.check(config, Some((width, height))).is_err() {
            return Err(Refusal::InvalidInput);
        }
        if !self.inputs.admit(now_ms, config.input_max_per_second) {
            return Err(Refusal::RateLimited);
        }
        self.last_input_seq = Some(input.seq);
        self.last_activity_ms = now_ms;
        Ok(())
    }

    /// The viewer says it is still there.
    ///
    /// # Errors
    /// [`Refusal::WrongParty`] unless it is exactly the viewer; [`Refusal::NotAuthorized`] unless `Authorized`.
    pub fn ping(
        &mut self,
        config: &RemoteConfig,
        by: &Identity,
        target_id: &str,
        now_ms: i64,
    ) -> Result<(), Refusal> {
        self.gate_clock(config, now_ms)?;
        if !self.viewer_is(by) || target_id != self.computer.target_id {
            return Err(Refusal::WrongParty);
        }
        if self.state != State::Authorized {
            return Err(Refusal::NotAuthorized);
        }
        self.last_activity_ms = now_ms;
        Ok(())
    }

    /// A clipboard transfer. Toward the computer it is the viewer's and counts as input (it needs control and a computer that is
    /// ready, unlocked and permitted); toward the viewer it is the computer's own side, only under a control grant and an unlocked,
    /// capturing screen. Each direction has its own increasing sequence and they share one rate limit.
    ///
    /// # Errors
    /// [`Refusal::WrongParty`] unless the sender is the side the direction names (and the transfer names this viewer);
    /// [`Refusal::NotAuthorized`]; [`Refusal::ViewOnly`]; [`Refusal::CaptureNotReady`], [`Refusal::Locked`],
    /// [`Refusal::PermissionMissing`]; [`Refusal::StaleSeq`]; [`Refusal::RateLimited`]; [`Refusal::InvalidInput`].
    pub fn admit_clipboard(
        &mut self,
        config: &RemoteConfig,
        by: &Identity,
        clip: &Clipboard,
        now_ms: i64,
    ) -> Result<(), Refusal> {
        self.gate_clock(config, now_ms)?;
        let sender_ok = match clip.direction {
            Direction::ToHost => self.viewer_is(by),
            Direction::ToViewer => self.host_is(by),
        };
        if !sender_ok
            || clip.target_id != self.computer.target_id
            || clip.viewer_user_id != self.viewer.user_id
        {
            return Err(Refusal::WrongParty);
        }
        if self.state != State::Authorized {
            return Err(Refusal::NotAuthorized);
        }
        if self.granted != Some(Scope::Control) {
            return Err(Refusal::ViewOnly);
        }
        self.ready(config, now_ms, clip.direction == Direction::ToHost)?;
        let slot = usize::from(clip.direction == Direction::ToViewer);
        if self.last_clip_seq[slot].is_some_and(|seq| clip.seq <= seq) {
            return Err(Refusal::StaleSeq);
        }
        if clip.check(config).is_err() {
            return Err(Refusal::InvalidInput);
        }
        if !self.clips.admit(now_ms, config.clipboard_max_per_second) {
            return Err(Refusal::RateLimited);
        }
        self.last_clip_seq[slot] = Some(clip.seq);
        if clip.direction == Direction::ToHost {
            self.last_activity_ms = now_ms;
        }
        Ok(())
    }

    /// Whether `who` is either side of this session, in any state: who may be told that it ended.
    #[must_use]
    pub fn involves(&self, who: &Identity) -> bool {
        self.viewer_is(who) || self.host_is(who)
    }

    /// Whether `who` is the viewer this session's frames and state belong to, now.
    #[must_use]
    pub fn viewer_receives(&self, who: &Identity) -> bool {
        self.state == State::Authorized && self.viewer_is(who)
    }

    /// Whether `who` is the computer's own side, which receives the viewer's input and the state.
    #[must_use]
    pub fn host_receives(&self, who: &Identity) -> bool {
        self.state == State::Authorized && self.host_is(who)
    }
}

#[cfg(test)]
mod tests {
    use super::super::protocol::{Button, InputEvent};
    use super::*;

    const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
    const OWNER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0OWNER0";
    const VIEWER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0VIEWER";
    const STRANGER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0STRNGR";
    const T0: i64 = 1_000_000;

    fn config() -> RemoteConfig {
        RemoteConfig::builtin()
    }

    fn computer() -> Computer {
        Computer {
            tenant_id: "tn_01J8Z3K6F1N8VQ2X5W9Y0TENANT".to_string(),
            workspace_id: "ws_01J8Z3K6F1N8VQ2X5W9Y0WSPACE".to_string(),
            target_id: TARGET.to_string(),
            owner_user_id: OWNER.to_string(),
        }
    }

    fn viewer() -> Identity {
        Identity::new(VIEWER, Some("fam_viewer".to_string()))
    }

    fn host() -> Identity {
        Identity::new(OWNER, Some("fam_host".to_string()))
    }

    fn requested(scope: Scope) -> Session {
        Session::request(&config(), computer(), viewer(), scope, T0).expect("request")
    }

    fn authorized(scope: Scope) -> Session {
        let mut session = requested(Scope::Control);
        session
            .decide(
                &config(),
                &host(),
                Decision::Grant {
                    scope,
                    seconds: 600,
                },
                T0 + 1000,
            )
            .expect("grant");
        session
    }

    fn ready(session: &mut Session, at: i64) {
        let report = ConditionsReport {
            target_id: TARGET.to_string(),
            viewer_user_id: VIEWER.to_string(),
            conditions: Conditions {
                capture_ready: true,
                unlocked: true,
                screen_recording: Permission::Granted,
                accessibility: Permission::Granted,
            },
        };
        session
            .report(&config(), &host(), &report, at)
            .expect("report");
    }

    fn frame(seq: u64) -> Frame {
        Frame {
            target_id: TARGET.to_string(),
            viewer_user_id: VIEWER.to_string(),
            seq,
            width: 1280,
            height: 720,
            mime: "image/jpeg".to_string(),
            data: String::new(),
        }
    }

    fn click(seq: u64) -> InputMessage {
        InputMessage {
            target_id: TARGET.to_string(),
            seq,
            event: InputEvent::PointerDown {
                x: 10,
                y: 10,
                button: Button::Left,
            },
        }
    }

    fn live(scope: Scope) -> Session {
        let mut session = authorized(scope);
        ready(&mut session, T0 + 2000);
        session
            .admit_frame(&config(), &host(), &frame(1), T0 + 2000)
            .expect("frame");
        session
    }

    #[test]
    fn nothing_is_authorized_until_the_person_at_the_computer_says_so() {
        let mut session = requested(Scope::Control);
        assert_eq!(session.state(), State::Requested);
        // Input, frames, reports and viewer reception all wait for a grant.
        ready_fails_before_grant(&mut session);
        assert!(!session.viewer_receives(&viewer()));
        assert!(!session.host_receives(&host()));
        // The viewer cannot grant itself, nor can a stranger.
        for who in [viewer(), Identity::new(STRANGER, None)] {
            assert_eq!(
                session.decide(
                    &config(),
                    &who,
                    Decision::Grant {
                        scope: Scope::Control,
                        seconds: 60
                    },
                    T0 + 1
                ),
                Err(Refusal::WrongParty)
            );
        }
        assert_eq!(session.state(), State::Requested);
        let transition = session
            .decide(
                &config(),
                &host(),
                Decision::Grant {
                    scope: Scope::Control,
                    seconds: 600,
                },
                T0 + 1000,
            )
            .expect("grant");
        assert_eq!(
            (transition.from, transition.to),
            (State::Requested, State::Authorized)
        );
        assert_eq!(session.granted(), Some(Scope::Control));
        assert_eq!(session.host_family(), Some("fam_host"));
        // A second decision on a decided session is not another grant.
        assert_eq!(
            session.decide(&config(), &host(), Decision::Deny, T0 + 2000),
            Err(Refusal::NotAuthorized)
        );
    }

    fn ready_fails_before_grant(session: &mut Session) {
        let report = ConditionsReport {
            target_id: TARGET.to_string(),
            viewer_user_id: VIEWER.to_string(),
            conditions: Conditions {
                capture_ready: true,
                unlocked: true,
                screen_recording: Permission::Granted,
                accessibility: Permission::Granted,
            },
        };
        assert_eq!(
            session.report(&config(), &host(), &report, T0),
            Err(Refusal::NotAuthorized)
        );
        assert_eq!(
            session.admit_frame(&config(), &host(), &frame(1), T0),
            Err(Refusal::NotAuthorized)
        );
        assert_eq!(
            session.admit_input(&config(), &viewer(), &click(1), T0),
            Err(Refusal::NotAuthorized)
        );
    }

    #[test]
    fn a_denial_ends_it_and_a_stopped_session_is_never_revived() {
        let mut session = requested(Scope::View);
        let transition = session
            .decide(&config(), &host(), Decision::Deny, T0 + 1)
            .expect("deny");
        assert_eq!(transition.stop, Some(StopReason::Denied));
        assert_eq!(
            session.decide(
                &config(),
                &host(),
                Decision::Grant {
                    scope: Scope::View,
                    seconds: 60
                },
                T0 + 2
            ),
            Err(Refusal::Stopped(StopReason::Denied))
        );
        assert_eq!(
            session.stop(Some(&host()), StopReason::HostStopped),
            Err(Refusal::Stopped(StopReason::Denied))
        );
        assert_eq!(session.state(), State::Stopped);
    }

    #[test]
    fn a_request_that_waits_too_long_lapses_and_a_late_grant_is_refused() {
        let mut session = requested(Scope::Control);
        let late = T0 + config().consent_ttl_seconds * 1000;
        assert_eq!(
            session.decide(
                &config(),
                &host(),
                Decision::Grant {
                    scope: Scope::Control,
                    seconds: 60
                },
                late
            ),
            Err(Refusal::Expired)
        );
        assert_eq!(session.state(), State::Stopped);
        assert_eq!(session.stop_reason(), Some(StopReason::ConsentExpired));
    }

    #[test]
    fn a_grant_may_narrow_a_request_and_never_widen_it_or_outlast_the_limit() {
        let mut view = requested(Scope::View);
        assert_eq!(
            view.decide(
                &config(),
                &host(),
                Decision::Grant {
                    scope: Scope::Control,
                    seconds: 60
                },
                T0 + 1
            ),
            Err(Refusal::Overgrant)
        );
        assert_eq!(
            view.decide(
                &config(),
                &host(),
                Decision::Grant {
                    scope: Scope::View,
                    seconds: config().grant_max_seconds + 1
                },
                T0 + 1
            ),
            Err(Refusal::Overgrant)
        );
        assert_eq!(
            view.decide(
                &config(),
                &host(),
                Decision::Grant {
                    scope: Scope::View,
                    seconds: 0
                },
                T0 + 1
            ),
            Err(Refusal::Overgrant)
        );
        assert_eq!(
            view.state(),
            State::Requested,
            "a refused grant leaves the request open"
        );
        let mut narrowed = requested(Scope::Control);
        narrowed
            .decide(
                &config(),
                &host(),
                Decision::Grant {
                    scope: Scope::View,
                    seconds: 60,
                },
                T0 + 1,
            )
            .expect("narrowed");
        assert_eq!(narrowed.granted(), Some(Scope::View));
    }

    #[test]
    fn input_needs_control_and_a_computer_that_is_capturing_unlocked_and_permitted() {
        // View only: input is refused whatever the computer says.
        let mut view = live(Scope::View);
        assert_eq!(
            view.admit_input(&config(), &viewer(), &click(1), T0 + 2100),
            Err(Refusal::ViewOnly)
        );
        // Control, ready: admitted.
        let mut session = live(Scope::Control);
        assert_eq!(
            session.admit_input(&config(), &viewer(), &click(1), T0 + 2100),
            Ok(())
        );

        let report = |capture_ready, unlocked, screen_recording, accessibility| ConditionsReport {
            target_id: TARGET.to_string(),
            viewer_user_id: VIEWER.to_string(),
            conditions: Conditions {
                capture_ready,
                unlocked,
                screen_recording,
                accessibility,
            },
        };
        let cases = [
            (
                report(false, true, Permission::Granted, Permission::Granted),
                Refusal::CaptureNotReady,
            ),
            (
                report(true, false, Permission::Granted, Permission::Granted),
                Refusal::Locked,
            ),
            (
                report(true, true, Permission::Denied, Permission::Granted),
                Refusal::PermissionMissing("screen_recording"),
            ),
            (
                report(true, true, Permission::NotDetermined, Permission::Granted),
                Refusal::PermissionMissing("screen_recording"),
            ),
            (
                report(true, true, Permission::Granted, Permission::Denied),
                Refusal::PermissionMissing("accessibility"),
            ),
            (
                report(true, true, Permission::Granted, Permission::Restricted),
                Refusal::PermissionMissing("accessibility"),
            ),
        ];
        for (index, (changed, expected)) in cases.into_iter().enumerate() {
            let mut session = live(Scope::Control);
            session
                .report(&config(), &host(), &changed, T0 + 2500)
                .expect("report");
            let seq = u64::try_from(index).unwrap_or(0) + 1;
            assert_eq!(
                session.admit_input(&config(), &viewer(), &click(seq), T0 + 2600),
                Err(expected),
                "{expected:?}"
            );
        }
    }

    #[test]
    fn a_computer_that_falls_silent_stops_admitting_input_and_nothing_is_queued() {
        let mut session = live(Scope::Control);
        let silent = T0 + 2000 + config().host_report_max_age_seconds * 1000 + 1;
        assert_eq!(
            session.admit_input(&config(), &viewer(), &click(1), silent),
            Err(Refusal::CaptureNotReady)
        );
        // It reports again: the next event is admitted, and the refused one was not kept.
        ready(&mut session, silent);
        assert_eq!(
            session.admit_input(&config(), &viewer(), &click(2), silent),
            Ok(())
        );
        assert_eq!(
            session.admit_input(&config(), &viewer(), &click(1), silent),
            Err(Refusal::StaleSeq),
            "the refused event is not replayable"
        );
        // A computer that never reported admits nothing.
        let mut never = authorized(Scope::Control);
        assert_eq!(
            never.admit_frame(&config(), &host(), &frame(1), T0 + 2000),
            Err(Refusal::CaptureNotReady)
        );
    }

    #[test]
    fn a_locked_screen_shows_nothing_and_takes_nothing() {
        let mut session = live(Scope::Control);
        let locked = ConditionsReport {
            target_id: TARGET.to_string(),
            viewer_user_id: VIEWER.to_string(),
            conditions: Conditions {
                capture_ready: true,
                unlocked: false,
                screen_recording: Permission::Granted,
                accessibility: Permission::Granted,
            },
        };
        session
            .report(&config(), &host(), &locked, T0 + 3000)
            .expect("report");
        assert_eq!(
            session.admit_frame(&config(), &host(), &frame(2), T0 + 3000),
            Err(Refusal::Locked)
        );
        assert_eq!(
            session.admit_input(&config(), &viewer(), &click(1), T0 + 3000),
            Err(Refusal::Locked)
        );
    }

    #[test]
    fn only_the_bound_viewer_sends_input_and_only_the_computers_own_side_sends_frames() {
        let mut session = live(Scope::Control);
        // The same account on another sign-in is another party; so is a stranger and so is the computer's side.
        for impostor in [
            Identity::new(VIEWER, Some("fam_other".to_string())),
            Identity::new(VIEWER, None),
            Identity::new(STRANGER, Some("fam_viewer".to_string())),
            host(),
        ] {
            assert_eq!(
                session.admit_input(&config(), &impostor, &click(1), T0 + 2100),
                Err(Refusal::WrongParty),
                "{impostor:?}"
            );
        }
        for impostor in [
            viewer(),
            Identity::new(STRANGER, Some("fam_host".to_string())),
            Identity::new(OWNER, Some("fam_viewer".to_string())),
            Identity::new(OWNER, None),
        ] {
            assert_eq!(
                session.admit_frame(&config(), &impostor, &frame(2), T0 + 2100),
                Err(Refusal::WrongParty),
                "{impostor:?}"
            );
        }
        // A frame for another viewer, or another computer, is not this session's.
        let mut elsewhere = frame(2);
        elsewhere.viewer_user_id = STRANGER.to_string();
        assert_eq!(
            session.admit_frame(&config(), &host(), &elsewhere, T0 + 2100),
            Err(Refusal::WrongParty)
        );
        let mut other_computer = click(1);
        other_computer.target_id = "tgt_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ".to_string();
        assert_eq!(
            session.admit_input(&config(), &viewer(), &other_computer, T0 + 2100),
            Err(Refusal::WrongParty)
        );
        // The viewer cannot report the computer's conditions.
        let report = ConditionsReport {
            target_id: TARGET.to_string(),
            viewer_user_id: VIEWER.to_string(),
            conditions: Conditions {
                capture_ready: true,
                unlocked: true,
                screen_recording: Permission::Granted,
                accessibility: Permission::Granted,
            },
        };
        assert_eq!(
            session.report(&config(), &viewer(), &report, T0 + 2100),
            Err(Refusal::WrongParty)
        );
        // Frames and state belong to the viewer and the computer's side, and to nobody else.
        assert!(session.viewer_receives(&viewer()));
        assert!(!session.viewer_receives(&Identity::new(VIEWER, Some("fam_other".to_string()))));
        assert!(!session.viewer_receives(&host()));
        assert!(session.host_receives(&host()));
        assert!(!session.host_receives(&viewer()));
    }

    #[test]
    fn input_waits_for_a_first_frame_and_stays_on_the_screen_the_viewer_was_shown() {
        let mut session = authorized(Scope::Control);
        ready(&mut session, T0 + 2000);
        assert_eq!(
            session.admit_input(&config(), &viewer(), &click(1), T0 + 2000),
            Err(Refusal::NoFrame)
        );
        session
            .admit_frame(&config(), &host(), &frame(1), T0 + 2000)
            .expect("frame");
        let off = InputMessage {
            target_id: TARGET.to_string(),
            seq: 1,
            event: InputEvent::PointerDown {
                x: 1280,
                y: 10,
                button: Button::Left,
            },
        };
        assert_eq!(
            session.admit_input(&config(), &viewer(), &off, T0 + 2100),
            Err(Refusal::InvalidInput)
        );
        assert_eq!(
            session.admit_input(&config(), &viewer(), &click(1), T0 + 2100),
            Ok(())
        );
    }

    #[test]
    fn sequence_numbers_only_increase_and_the_rate_is_bounded() {
        let mut session = live(Scope::Control);
        assert_eq!(
            session.admit_input(&config(), &viewer(), &click(5), T0 + 2100),
            Ok(())
        );
        assert_eq!(
            session.admit_input(&config(), &viewer(), &click(5), T0 + 2100),
            Err(Refusal::StaleSeq)
        );
        assert_eq!(
            session.admit_input(&config(), &viewer(), &click(4), T0 + 2100),
            Err(Refusal::StaleSeq)
        );
        assert_eq!(
            session.admit_frame(&config(), &host(), &frame(1), T0 + 2200),
            Err(Refusal::StaleSeq)
        );
        let limit = u64::from(config().input_max_per_second);
        let mut refused = 0;
        for seq in 6..(6 + limit + 10) {
            if session.admit_input(&config(), &viewer(), &click(seq), T0 + 2300)
                == Err(Refusal::RateLimited)
            {
                refused += 1;
            }
        }
        assert!(
            refused >= 10,
            "past the limit in one second is refused ({refused})"
        );
        // The next second has room again.
        assert_eq!(
            session.admit_input(&config(), &viewer(), &click(1000), T0 + 3400),
            Ok(())
        );
        let frames = u64::from(config().frame_max_per_second);
        let mut limited = 0;
        for seq in 2..(2 + frames + 5) {
            if session.admit_frame(&config(), &host(), &frame(seq), T0 + 4000)
                == Err(Refusal::RateLimited)
            {
                limited += 1;
            }
        }
        assert!(limited >= 5);
    }

    #[test]
    fn a_grant_runs_out_and_a_session_with_no_viewer_activity_ends() {
        let mut session = live(Scope::Control);
        let end = session.ends_ms();
        assert!(end > T0);
        assert_eq!(
            session.admit_input(&config(), &viewer(), &click(1), end),
            Err(Refusal::Stopped(StopReason::SessionExpired))
        );
        assert_eq!(session.state(), State::Stopped);
        let mut idle = live(Scope::Control);
        let later = T0 + 2000 + config().session_idle_seconds * 1000;
        assert_eq!(
            idle.tick(&config(), later).map(|t| t.stop),
            Some(Some(StopReason::Idle))
        );
        // A viewer who stopped pinging is gone, whatever it was granted.
        let mut watching = live(Scope::View);
        assert_eq!(
            watching.tick(&config(), later).map(|t| t.stop),
            Some(Some(StopReason::Idle))
        );
        // A ping keeps a view session alive.
        let mut pinged = live(Scope::View);
        let mid = T0 + 2000 + config().session_idle_seconds * 500;
        assert_eq!(pinged.ping(&config(), &viewer(), TARGET, mid), Ok(()));
        assert_eq!(
            pinged.ping(&config(), &host(), TARGET, mid),
            Err(Refusal::WrongParty)
        );
        assert_eq!(pinged.tick(&config(), later), None);
        // Input keeps a control session alive.
        let mut active = live(Scope::Control);
        let mut steps = 0;
        for step in 1..6_i64 {
            let at = T0 + 2000 + step * (config().session_idle_seconds * 1000 - 5000);
            if at >= active.ends_ms() {
                break;
            }
            ready(&mut active, at);
            assert_eq!(
                active.admit_input(
                    &config(),
                    &viewer(),
                    &click(u64::try_from(step).unwrap_or(0)),
                    at
                ),
                Ok(()),
                "step {step}"
            );
            steps += 1;
        }
        assert!(
            steps >= 2,
            "activity at intervals shorter than the idle limit kept it alive past one idle period"
        );
    }

    #[test]
    fn either_side_may_stop_it_a_stranger_may_not_and_the_system_may_end_it() {
        let mut by_host = live(Scope::Control);
        assert_eq!(
            by_host
                .stop(Some(&host()), StopReason::ViewerStopped)
                .map(|t| t.stop),
            Ok(Some(StopReason::HostStopped))
        );
        let mut by_viewer = live(Scope::Control);
        assert_eq!(
            by_viewer
                .stop(Some(&viewer()), StopReason::HostStopped)
                .map(|t| t.stop),
            Ok(Some(StopReason::ViewerStopped))
        );
        let mut stranger = live(Scope::Control);
        assert_eq!(
            stranger.stop(
                Some(&Identity::new(STRANGER, None)),
                StopReason::HostStopped
            ),
            Err(Refusal::WrongParty)
        );
        assert_eq!(stranger.state(), State::Authorized);
        assert_eq!(
            stranger
                .stop(None, StopReason::ViewerSignedOut)
                .map(|t| t.stop),
            Ok(Some(StopReason::ViewerSignedOut))
        );
        // After a stop nothing is admitted, and no message revives it.
        assert_eq!(
            stranger.admit_input(&config(), &viewer(), &click(1), T0 + 2100),
            Err(Refusal::Stopped(StopReason::ViewerSignedOut))
        );
        assert_eq!(
            stranger.admit_frame(&config(), &host(), &frame(9), T0 + 2100),
            Err(Refusal::Stopped(StopReason::ViewerSignedOut))
        );
        assert!(!stranger.viewer_receives(&viewer()));
    }

    fn clip(direction: Direction, seq: u64, text: &str) -> Clipboard {
        Clipboard {
            target_id: TARGET.to_string(),
            viewer_user_id: VIEWER.to_string(),
            direction,
            seq,
            text: Some(text.to_string()),
            files: Vec::new(),
        }
    }

    #[test]
    fn the_clipboard_is_a_separate_channel_that_needs_control_a_ready_screen_and_the_right_sender()
    {
        let mut view_only = live(Scope::View);
        let at = T0 + 2100;
        assert_eq!(
            view_only.admit_clipboard(&config(), &viewer(), &clip(Direction::ToHost, 1, "hi"), at),
            Err(Refusal::ViewOnly)
        );
        let mut session = live(Scope::Control);
        // Toward the computer: only the viewer. Toward the viewer: only the computer's own side.
        assert_eq!(
            session.admit_clipboard(&config(), &host(), &clip(Direction::ToHost, 1, "hi"), at),
            Err(Refusal::WrongParty)
        );
        assert_eq!(
            session.admit_clipboard(
                &config(),
                &viewer(),
                &clip(Direction::ToViewer, 1, "hi"),
                at
            ),
            Err(Refusal::WrongParty)
        );
        let stranger = Identity::new(STRANGER, None);
        assert_eq!(
            session.admit_clipboard(&config(), &stranger, &clip(Direction::ToHost, 1, "hi"), at),
            Err(Refusal::WrongParty)
        );
        let mut other = clip(Direction::ToHost, 1, "hi");
        other.viewer_user_id = STRANGER.to_string();
        assert_eq!(
            session.admit_clipboard(&config(), &viewer(), &other, at),
            Err(Refusal::WrongParty)
        );
        assert_eq!(
            session.admit_clipboard(&config(), &viewer(), &clip(Direction::ToHost, 1, "hi"), at),
            Ok(())
        );
        assert_eq!(
            session.admit_clipboard(
                &config(),
                &host(),
                &clip(Direction::ToViewer, 1, "back"),
                at + 1000
            ),
            Ok(())
        );
        // Each direction counts on its own and only upward; a repeated number is stale.
        assert_eq!(
            session.admit_clipboard(
                &config(),
                &viewer(),
                &clip(Direction::ToHost, 1, "again"),
                at + 2000
            ),
            Err(Refusal::StaleSeq)
        );
        assert_eq!(
            session.admit_clipboard(
                &config(),
                &viewer(),
                &clip(Direction::ToHost, 2, "again"),
                at + 2000
            ),
            Ok(())
        );
        // An oversize transfer breaks the bound even when everything else is fine.
        let big = "x".repeat(config().clipboard_max_bytes + 1);
        assert_eq!(
            session.admit_clipboard(
                &config(),
                &viewer(),
                &clip(Direction::ToHost, 3, &big),
                at + 3000
            ),
            Err(Refusal::InvalidInput)
        );
        // A transfer is bounded in rate.
        let mut fast = live(Scope::Control);
        let limit = u64::from(config().clipboard_max_per_second);
        for seq in 1..=limit {
            assert_eq!(
                fast.admit_clipboard(&config(), &viewer(), &clip(Direction::ToHost, seq, "a"), at),
                Ok(())
            );
        }
        assert_eq!(
            fast.admit_clipboard(
                &config(),
                &viewer(),
                &clip(Direction::ToHost, limit + 1, "a"),
                at
            ),
            Err(Refusal::RateLimited)
        );
        // A locked screen takes and gives nothing; so does a computer that lost Accessibility (toward it) or fell silent.
        let mut locked = live(Scope::Control);
        let report = ConditionsReport {
            target_id: TARGET.to_string(),
            viewer_user_id: VIEWER.to_string(),
            conditions: Conditions {
                capture_ready: true,
                unlocked: false,
                screen_recording: Permission::Granted,
                accessibility: Permission::Granted,
            },
        };
        locked
            .report(&config(), &host(), &report, at)
            .expect("report");
        assert_eq!(
            locked.admit_clipboard(&config(), &viewer(), &clip(Direction::ToHost, 1, "a"), at),
            Err(Refusal::Locked)
        );
        assert_eq!(
            locked.admit_clipboard(&config(), &host(), &clip(Direction::ToViewer, 1, "a"), at),
            Err(Refusal::Locked)
        );
        let mut no_ax = live(Scope::Control);
        let report = ConditionsReport {
            target_id: TARGET.to_string(),
            viewer_user_id: VIEWER.to_string(),
            conditions: Conditions {
                capture_ready: true,
                unlocked: true,
                screen_recording: Permission::Granted,
                accessibility: Permission::Denied,
            },
        };
        no_ax
            .report(&config(), &host(), &report, at)
            .expect("report");
        assert_eq!(
            no_ax.admit_clipboard(&config(), &viewer(), &clip(Direction::ToHost, 1, "a"), at),
            Err(Refusal::PermissionMissing("accessibility"))
        );
        let mut silent = live(Scope::Control);
        let stale = at + config().host_report_max_age_seconds * 1000 + 1;
        assert_eq!(
            silent.admit_clipboard(
                &config(),
                &viewer(),
                &clip(Direction::ToHost, 1, "a"),
                stale
            ),
            Err(Refusal::CaptureNotReady)
        );
    }

    #[test]
    fn the_feature_switch_refuses_a_request() {
        let mut off = config();
        off.enabled = false;
        assert_eq!(
            Session::request(&off, computer(), viewer(), Scope::View, T0).err(),
            Some(Refusal::Disabled)
        );
    }

    #[test]
    fn refusals_have_stable_spellings() {
        assert_eq!(
            Refusal::PermissionMissing("accessibility").as_str(),
            "permission_missing:accessibility"
        );
        assert_eq!(Refusal::Stopped(StopReason::Idle).as_str(), "stopped:idle");
        assert_eq!(Refusal::WrongParty.as_str(), "wrong_party");
        assert_eq!(Refusal::Busy.as_str(), "busy");
        assert_eq!(StopReason::AgentResumed.as_str(), "agent_resumed");
        assert_eq!(State::Authorized.as_str(), "authorized");
    }
}
