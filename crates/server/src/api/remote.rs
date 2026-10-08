//! Remote control of a person's own computer, relayed over the live-frames WebSocket (ADP-009, DOMAIN.md §8.6.2, §14).
//!
//! Four commands (`RequestRemoteControl`, `GrantRemoteControl`, `DenyRemoteControl`, `StopRemoteControl`) move one viewer's session on one
//! native computer through the authorization state machine in `quansio_machine::remote`; the frames and the input travel on the
//! existing `/v1/stream` socket, on the target's channel, as `remote.*` live frames that **only the two parties of the session ever
//! receive** — the per-frame gate below is the authorization, because the same channel carries the target's other live frames to anyone
//! who watches it. Nothing is persisted but the lifecycle: a `target.remote_changed` event and an audit entry for every transition. A
//! restart ends every session, as it should: a consent does not survive the process that held it.
//!
//! The server is a relay and a second check, not the only one: the computer's own side re-validates every input against the same machine
//! before injecting it (`quansio_qworkerd::remote`), and a computer that stops reporting its capture, lock and permission state stops
//! being sent input.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use quansio_events::EventStore;
use quansio_machine::computer_use::store::ComputerControlStore;
use quansio_machine::computer_use::MachineHolder;
use quansio_machine::remote::protocol::{Scope, Upstream};
use quansio_machine::remote::session::{
    Computer, Decision, Identity, Refusal, Session, State, StopReason, Transition,
};
use quansio_machine::remote::RemoteConfig;
use serde_json::{json, Value};
use sqlx::Row;

use super::live_sessions::{actor_of, audit, commit, stage};
use super::rbac::Authorization;
use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::policy::rbac::ActionFamily;
use crate::runtime::state_machine::{LiveFramePort, RuntimeIdentity};

/// The event the lifecycle emits, in the `target` family (DOMAIN.md §9).
pub const REMOTE_EVENT: &str = "target.remote_changed";

type Key = (String, String, String);

/// A session as the commands and the live frames describe it: who, what, and where it stands. No pixels, no input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionView {
    /// The tenant.
    pub tenant_id: String,
    /// The workspace of the computer.
    pub workspace_id: String,
    /// The computer.
    pub target_id: String,
    /// The person who decides for it.
    pub host_user_id: String,
    /// The viewer.
    pub viewer_user_id: String,
    /// What was asked.
    pub requested: Scope,
    /// What was granted.
    pub granted: Option<Scope>,
    /// Where it is.
    pub state: State,
    /// Why it ended.
    pub reason: Option<StopReason>,
    /// When the request lapses.
    pub consent_expires_ms: i64,
    /// When the grant ends.
    pub ends_ms: i64,
}

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map_or_else(String::new, |at| {
        at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    })
}

impl SessionView {
    fn of(session: &Session) -> Self {
        let computer = session.computer();
        Self {
            tenant_id: computer.tenant_id.clone(),
            workspace_id: computer.workspace_id.clone(),
            target_id: computer.target_id.clone(),
            host_user_id: computer.owner_user_id.clone(),
            viewer_user_id: session.viewer().user_id.clone(),
            requested: session.requested(),
            granted: session.granted(),
            state: session.state(),
            reason: session.stop_reason(),
            consent_expires_ms: session.consent_expires_ms(),
            ends_ms: session.ends_ms(),
        }
    }

    /// The JSON the commands answer with and the lifecycle event carries.
    #[must_use]
    pub fn json(&self) -> Value {
        json!({
            "target_id": self.target_id,
            "workspace_id": self.workspace_id,
            "host_user_id": self.host_user_id,
            "viewer_user_id": self.viewer_user_id,
            "requested_scope": self.requested.as_str(),
            "granted_scope": self.granted.map(Scope::as_str),
            "state": self.state.as_str(),
            "reason": self.reason.map(StopReason::as_str),
            "consent_expires_at": (self.state == State::Requested).then(|| iso(self.consent_expires_ms)),
            "ends_at": (self.state == State::Authorized).then(|| iso(self.ends_ms)),
        })
    }
}

/// One live frame to publish on the target's channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Relay {
    /// `target:<tgt>`.
    pub channel: String,
    /// The frame (`remote.frame`, `remote.input`, `remote.clipboard` or `remote.state`).
    pub frame: Value,
}

fn state_frame(view: &SessionView, ready: Option<Value>) -> Relay {
    let mut frame = json!({
        "type": "remote.state",
        "target_id": view.target_id,
        "viewer_user_id": view.viewer_user_id,
        "state": view.state.as_str(),
        "granted_scope": view.granted.map(Scope::as_str),
        "reason": view.reason.map(StopReason::as_str),
    });
    if let Some(ready) = ready {
        frame["ready"] = ready;
    }
    Relay {
        channel: format!("target:{}", view.target_id),
        frame,
    }
}

/// Why an upstream message was not relayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dropped {
    /// The message breaks the wire form.
    Malformed(String),
    /// No such session.
    NoSession,
    /// The state machine refused it.
    Refused(Refusal),
}

impl Dropped {
    /// The reason spelling the sender is told.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Malformed(_) => "invalid_message".to_string(),
            Self::NoSession => "no_session".to_string(),
            Self::Refused(refusal) => refusal.as_str(),
        }
    }
}

/// Every remote session of the process, in memory only.
#[derive(Debug)]
pub struct RemoteHub {
    config: RemoteConfig,
    sessions: Mutex<HashMap<Key, Session>>,
}

impl Default for RemoteHub {
    fn default() -> Self {
        Self::new(RemoteConfig::builtin())
    }
}

impl RemoteHub {
    /// A hub over a configuration.
    #[must_use]
    pub fn new(config: RemoteConfig) -> Self {
        Self {
            config,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// The configuration.
    #[must_use]
    pub const fn config(&self) -> &RemoteConfig {
        &self.config
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Session>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A viewer asks.
    ///
    /// # Errors
    /// [`Refusal::Disabled`]; [`Refusal::NotAuthorized`] when this viewer already has a live session on the computer; [`Refusal::Busy`]
    /// when anyone else does (a computer has one session at a time, so a person never has two viewers to watch for).
    pub fn request(
        &self,
        computer: Computer,
        viewer: Identity,
        scope: Scope,
        now_ms: i64,
    ) -> Result<SessionView, Refusal> {
        let key = (
            computer.tenant_id.clone(),
            computer.target_id.clone(),
            viewer.user_id.clone(),
        );
        let mut sessions = self.lock();
        let mut busy = false;
        for ((tenant, target, viewer_user), existing) in sessions.iter_mut() {
            if tenant != &key.0 || target != &key.1 {
                continue;
            }
            existing.tick(&self.config, now_ms);
            if existing.state() != State::Stopped {
                if viewer_user == &key.2 {
                    return Err(Refusal::NotAuthorized);
                }
                busy = true;
            }
        }
        if busy {
            return Err(Refusal::Busy);
        }
        let session = Session::request(&self.config, computer, viewer, scope, now_ms)?;
        let view = SessionView::of(&session);
        sessions.insert(key, session);
        Ok(view)
    }

    /// The person at the computer answers.
    ///
    /// # Errors
    /// The machine's refusal, or [`Refusal::NotAuthorized`] when there is no such session.
    pub fn decide(
        &self,
        tenant_id: &str,
        target_id: &str,
        viewer_user_id: &str,
        by: &Identity,
        decision: Decision,
        now_ms: i64,
    ) -> Result<(SessionView, Transition), Refusal> {
        let mut sessions = self.lock();
        let key = (
            tenant_id.to_string(),
            target_id.to_string(),
            viewer_user_id.to_string(),
        );
        let session = sessions.get_mut(&key).ok_or(Refusal::NotAuthorized)?;
        let result = session.decide(&self.config, by, decision, now_ms);
        let view = SessionView::of(session);
        // A request that lapsed while it was being answered is stopped, and the caller records that.
        match result {
            Ok(transition) => Ok((view, transition)),
            Err(refusal) => Err(refusal),
        }
    }

    /// Someone stops a session; `viewer_user_id` is `None` for the person at the computer's Stop, which ends every session on it.
    ///
    /// # Errors
    /// [`Refusal::WrongParty`] for a caller who is neither side, [`Refusal::NotAuthorized`] when there is no such session.
    pub fn stop(
        &self,
        tenant_id: &str,
        target_id: &str,
        viewer_user_id: Option<&str>,
        by: &Identity,
    ) -> Result<Vec<(SessionView, Transition)>, Refusal> {
        let mut sessions = self.lock();
        let mut stopped = Vec::new();
        let mut wrong = false;
        for ((tenant, target, viewer), session) in sessions.iter_mut() {
            if tenant != tenant_id
                || target != target_id
                || viewer_user_id.is_some_and(|wanted| wanted != viewer)
            {
                continue;
            }
            if session.state() == State::Stopped {
                continue;
            }
            match session.stop(Some(by), StopReason::HostStopped) {
                Ok(transition) => stopped.push((SessionView::of(session), transition)),
                Err(Refusal::WrongParty) => wrong = true,
                Err(_) => {}
            }
        }
        if stopped.is_empty() {
            return Err(if wrong {
                Refusal::WrongParty
            } else {
                Refusal::NotAuthorized
            });
        }
        Ok(stopped)
    }

    /// The system ends every live session on a computer (the agent took input back, the computer went away).
    pub fn stop_target(
        &self,
        tenant_id: &str,
        target_id: &str,
        reason: StopReason,
    ) -> Vec<(SessionView, Transition)> {
        let mut sessions = self.lock();
        sessions
            .iter_mut()
            .filter(|((tenant, target, _), session)| {
                tenant == tenant_id && target == target_id && session.state() != State::Stopped
            })
            .filter_map(|(_, session)| {
                session
                    .stop(None, reason)
                    .ok()
                    .map(|transition| (SessionView::of(session), transition))
            })
            .collect()
    }

    /// The system ends every live session a viewer holds (their sign-in ended).
    pub fn stop_viewer(
        &self,
        tenant_id: &str,
        viewer: &Identity,
        reason: StopReason,
    ) -> Vec<(SessionView, Transition)> {
        let mut sessions = self.lock();
        sessions
            .iter_mut()
            .filter(|((tenant, _, user), session)| {
                tenant == tenant_id
                    && user == &viewer.user_id
                    && session.viewer() == viewer
                    && session.state() != State::Stopped
            })
            .filter_map(|(_, session)| {
                session
                    .stop(None, reason)
                    .ok()
                    .map(|transition| (SessionView::of(session), transition))
            })
            .collect()
    }

    /// Advance every session of `tenant_id` to `now_ms` and return the ones that ended because of it. Stopped sessions are forgotten a
    /// minute after they ended.
    pub fn sweep(&self, tenant_id: &str, now_ms: i64) -> Vec<(SessionView, Transition)> {
        let mut sessions = self.lock();
        let mut ended = Vec::new();
        for ((tenant, _, _), session) in sessions.iter_mut() {
            if tenant == tenant_id {
                if let Some(transition) = session.tick(&self.config, now_ms) {
                    ended.push((SessionView::of(session), transition));
                }
            }
        }
        sessions.retain(|(tenant, _, _), session| {
            tenant != tenant_id
                || session.state() != State::Stopped
                || now_ms - session.ends_ms().max(session.consent_expires_ms()) < 60_000
        });
        ended
    }

    /// The sessions `who` is a party to.
    pub fn sessions_of(&self, tenant_id: &str, who: &Identity) -> Vec<SessionView> {
        self.lock()
            .iter()
            .filter(|((tenant, _, _), session)| tenant == tenant_id && session.involves(who))
            .map(|(_, session)| SessionView::of(session))
            .collect()
    }

    /// Handle one upstream text message from `who`: parse it, ask the session, and return what to publish.
    ///
    /// # Errors
    /// [`Dropped`] when the message is malformed, names no session, or is refused.
    pub fn upstream(
        &self,
        tenant_id: &str,
        who: &Identity,
        text: &str,
        now_ms: i64,
    ) -> Result<Vec<Relay>, Dropped> {
        let message =
            Upstream::parse(text, &self.config).map_err(|error| Dropped::Malformed(error.0))?;
        let mut sessions = self.lock();
        let viewer_user = match &message {
            Upstream::Frame(frame) => frame.viewer_user_id.clone(),
            Upstream::Conditions(report) => report.viewer_user_id.clone(),
            Upstream::Input(_) | Upstream::Ping { .. } => who.user_id.clone(),
            Upstream::Clipboard(clip) => clip.viewer_user_id.clone(),
        };
        let key = (
            tenant_id.to_string(),
            message.target_id().to_string(),
            viewer_user,
        );
        let session = sessions.get_mut(&key).ok_or(Dropped::NoSession)?;
        let target_id = session.computer().target_id.clone();
        let channel = format!("target:{target_id}");
        let relays = match message {
            Upstream::Frame(frame) => {
                session
                    .admit_frame(&self.config, who, &frame, now_ms)
                    .map_err(Dropped::Refused)?;
                vec![Relay {
                    channel,
                    frame: json!({
                        "type": "remote.frame",
                        "target_id": frame.target_id,
                        "viewer_user_id": frame.viewer_user_id,
                        "seq": frame.seq,
                        "width": frame.width,
                        "height": frame.height,
                        "mime": frame.mime,
                        "data": frame.data,
                    }),
                }]
            }
            Upstream::Conditions(report) => {
                session
                    .report(&self.config, who, &report, now_ms)
                    .map_err(Dropped::Refused)?;
                let conditions = report.conditions;
                let view = SessionView::of(session);
                vec![state_frame(
                    &view,
                    Some(json!({
                        "capture_ready": conditions.capture_ready,
                        "unlocked": conditions.unlocked,
                        "screen_recording": conditions.screen_recording.as_str(),
                        "accessibility": conditions.accessibility.as_str(),
                    })),
                )]
            }
            Upstream::Ping { target_id } => {
                session
                    .ping(&self.config, who, &target_id, now_ms)
                    .map_err(Dropped::Refused)?;
                Vec::new()
            }
            Upstream::Clipboard(clip) => {
                session
                    .admit_clipboard(&self.config, who, &clip, now_ms)
                    .map_err(Dropped::Refused)?;
                vec![Relay {
                    channel,
                    frame: json!({
                        "type": "remote.clipboard",
                        "target_id": clip.target_id,
                        "viewer_user_id": clip.viewer_user_id,
                        "direction": clip.direction.as_str(),
                        "seq": clip.seq,
                        "text": clip.text,
                        "files": clip.files.iter().map(|file| json!({ "name": file.name, "data": file.data })).collect::<Vec<_>>(),
                    }),
                }]
            }
            Upstream::Input(input) => {
                session
                    .admit_input(&self.config, who, &input, now_ms)
                    .map_err(Dropped::Refused)?;
                vec![Relay {
                    channel,
                    frame: json!({
                        "type": "remote.input",
                        "target_id": input.target_id,
                        "viewer_user_id": session.viewer().user_id,
                        "seq": input.seq,
                        "event": serde_json::to_value(&input.event).unwrap_or(Value::Null),
                    }),
                }]
            }
        };
        drop(sessions);
        Ok(relays)
    }

    /// Whether `who` may be sent this `remote.*` live frame. Anything that names no session `who` is a party to is withheld.
    pub fn admits(&self, tenant_id: &str, who: &Identity, frame: &Value) -> bool {
        let Some(kind) = frame.get("type").and_then(Value::as_str) else {
            return false;
        };
        let (Some(target), Some(viewer)) = (
            frame.get("target_id").and_then(Value::as_str),
            frame.get("viewer_user_id").and_then(Value::as_str),
        ) else {
            return false;
        };
        let sessions = self.lock();
        let Some(session) = sessions.get(&(
            tenant_id.to_string(),
            target.to_string(),
            viewer.to_string(),
        )) else {
            return false;
        };
        match kind {
            "remote.frame" => session.viewer_receives(who),
            "remote.input" => session.host_receives(who),
            "remote.state" => session.involves(who),
            // A transfer goes to the side the direction names and to no one else.
            "remote.clipboard" => match frame.get("direction").and_then(Value::as_str) {
                Some("to_host") => session.host_receives(who),
                Some("to_viewer") => session.viewer_receives(who),
                _ => false,
            },
            _ => false,
        }
    }
}

/// Whether a live frame belongs to the remote relay (and so is admitted by [`RemoteHub::admits`] and nothing else).
#[must_use]
pub fn is_remote(frame: &Value) -> bool {
    frame
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.starts_with("remote."))
}

// ------------------------------------------------------------------ the commands

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn refuse(code: ApiErrorCode, message: impl Into<String>, correlation: &str) -> ApiError {
    ApiError::new(code, message.into(), correlation)
}

fn refusal_error(refusal: Refusal, correlation: &str) -> ApiError {
    let (code, message) = match refusal {
        Refusal::Disabled => (
            ApiErrorCode::PolicyDenied,
            "remote control is switched off for this deployment".to_string(),
        ),
        Refusal::WrongParty => (
            ApiErrorCode::ScopeForbidden,
            "only the person at the computer can decide, and only a party to a session can stop it"
                .to_string(),
        ),
        Refusal::Overgrant => (
            ApiErrorCode::ValidationSchema,
            "a grant may narrow a request, never widen it, and lasts at most the configured time"
                .to_string(),
        ),
        Refusal::Expired => (
            ApiErrorCode::ConflictState,
            "the request lapsed before it was answered".to_string(),
        ),
        other => (
            ApiErrorCode::ConflictState,
            format!("the session cannot do that now ({})", other.as_str()),
        ),
    };
    ApiError::new(code, message, correlation)
}

/// The caller's identity for the session: the account and the sign-in it came with.
async fn identity_of(
    state: &ApiState,
    scope: &TenantScope,
    user_id: &str,
    correlation: &str,
) -> Result<Identity, ApiError> {
    let family = match &scope.session_id {
        Some(session_id) => state
            .auth()
            .session_family(&scope.tenant_id, session_id)
            .await
            .map_err(|failure| refuse(ApiErrorCode::Internal, failure.to_string(), correlation))?,
        None => None,
    };
    Ok(Identity::new(user_id, family))
}

fn person<'a>(
    scope: &TenantScope,
    user_id: Option<&'a str>,
    what: &str,
    correlation: &str,
) -> Result<&'a str, ApiError> {
    if scope.principal.is_some() {
        return Err(refuse(
            ApiErrorCode::ScopeForbidden,
            format!("{what} is a person's: a service principal cannot"),
            correlation,
        ));
    }
    user_id.ok_or_else(|| {
        refuse(
            ApiErrorCode::AuthRequired,
            format!("{what} needs a signed-in person"),
            correlation,
        )
    })
}

fn target_param<'a>(params: &'a Value, correlation: &str) -> Result<&'a str, ApiError> {
    params
        .get("target_id")
        .and_then(Value::as_str)
        .filter(|id| id.starts_with("tgt_"))
        .ok_or_else(|| {
            refuse(
                ApiErrorCode::ValidationSchema,
                "target_id is a tgt_ id",
                correlation,
            )
        })
}

/// What a command needs to know about the computer.
struct Host {
    workspace_id: String,
    owner_user_id: Option<String>,
    native: bool,
}

async fn load_host(
    state: &ApiState,
    tenant_id: &str,
    target_id: &str,
    correlation: &str,
) -> Result<Host, ApiError> {
    let mut tx = state.tenant_tx(tenant_id, correlation).await?;
    let row = sqlx::query("SELECT workspace_id, substrate, owner_kind, owner_ref FROM execution_targets WHERE tenant_id = $1 AND id = $2")
        .bind(tenant_id)
        .bind(target_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| refuse(ApiErrorCode::Internal, error.to_string(), correlation))?;
    tx.commit()
        .await
        .map_err(|error| refuse(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let Some(row) = row else {
        return Err(refuse(
            ApiErrorCode::NotFound,
            format!("{target_id} is not a computer of this tenant"),
            correlation,
        ));
    };
    let substrate: String = row.get("substrate");
    let owner_kind: String = row.get("owner_kind");
    let owner_ref: Option<String> = row.get("owner_ref");
    Ok(Host {
        workspace_id: row.get("workspace_id"),
        owner_user_id: (owner_kind == "user").then_some(owner_ref).flatten(),
        native: substrate == "local_capsule_macos" || substrate == "windows_native",
    })
}

/// Record one transition: the lifecycle event and the audit entry, in one transaction, and the transient state frame to the parties.
async fn record(
    state: &ApiState,
    identity: &RuntimeIdentity,
    acting_user: Option<&str>,
    view: &SessionView,
    action: &str,
    reason: &str,
    correlation: &str,
) -> Result<(), ApiError> {
    let events = EventStore::new(state.pool().clone());
    let actor = actor_of(identity, acting_user);
    let tenant = view.tenant_id.clone();
    let payload = view.json();
    let aggregate = format!("{}:{}", view.target_id, view.viewer_user_id);
    let workspace = view.workspace_id.clone();
    let target = view.target_id.clone();
    let identity = identity.clone();
    let correlation_owned = correlation.to_string();
    let action = action.to_string();
    let reason = reason.to_string();
    commit(&events, &view.tenant_id.clone(), move |tx, batch| {
        Box::pin(async move {
            stage(
                tx,
                batch,
                &identity,
                &actor,
                "remote_session",
                &aggregate,
                REMOTE_EVENT,
                &workspace,
                payload,
            )
            .await?;
            audit(
                tx,
                &tenant,
                &workspace,
                &actor,
                &action,
                &format!("target:{target}"),
                &reason,
                &correlation_owned,
            )
            .await?;
            Ok(())
        })
    })
    .await?;
    let relay = state_frame(view, None);
    state
        .live()
        .publish(&view.tenant_id, &relay.channel, relay.frame);
    Ok(())
}

fn system_identity(tenant_id: &str) -> RuntimeIdentity {
    RuntimeIdentity::system(
        tenant_id,
        "api",
        quansio_core::CorrelationId::generate(&mut quansio_core::UlidGenerator::new()),
    )
}

/// Record the sessions the system ended (an expiry, a sign-out, the agent resuming).
pub async fn record_ended(state: &ApiState, ended: Vec<(SessionView, Transition)>) {
    for (view, transition) in ended {
        let identity = system_identity(&view.tenant_id);
        let reason = transition.stop.map_or("ended", StopReason::as_str);
        let _ = record(
            state,
            &identity,
            None,
            &view,
            "StopRemoteControl",
            reason,
            "remote",
        )
        .await;
    }
}

/// `RequestRemoteControl {target_id, scope}`: a viewer asks the person at a native computer.
pub async fn request_remote_control(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let user = person(scope, user_id, "remote control", correlation)?;
    let target_id = target_param(params, correlation)?;
    let wanted = params
        .get("scope")
        .and_then(Value::as_str)
        .and_then(Scope::parse)
        .ok_or_else(|| {
            refuse(
                ApiErrorCode::ValidationSchema,
                "scope is view or control",
                correlation,
            )
        })?;
    let host = load_host(state, &scope.tenant_id, target_id, correlation).await?;
    // Asking is authoring work in the computer's workspace; the person at the computer then decides.
    Authorization::check(
        state.pool(),
        scope,
        Some(&host.workspace_id),
        ActionFamily::AuthorWork,
        correlation,
    )
    .await?;
    if !host.native {
        return Err(refuse(
            ApiErrorCode::ConflictState,
            "remote control is for a native computer",
            correlation,
        ));
    }
    let Some(owner) = host.owner_user_id else {
        return Err(refuse(
            ApiErrorCode::ConflictState,
            "this computer has no owner who can consent",
            correlation,
        ));
    };
    let viewer = identity_of(state, scope, user, correlation).await?;
    let view = state
        .remote()
        .request(
            Computer {
                tenant_id: scope.tenant_id.clone(),
                workspace_id: host.workspace_id,
                target_id: target_id.to_string(),
                owner_user_id: owner,
            },
            viewer,
            wanted,
            now_ms(),
        )
        .map_err(|refusal| refusal_error(refusal, correlation))?;
    if let Err(error) = record(
        state,
        identity,
        Some(user),
        &view,
        "RequestRemoteControl",
        &format!("{} requested", wanted.as_str()),
        correlation,
    )
    .await
    {
        // A request that could not be recorded is not left open.
        let _ = state
            .remote()
            .stop_target(&scope.tenant_id, target_id, StopReason::HostLost);
        return Err(error);
    }
    Ok(json!({ "session": view.json() }))
}

/// `GrantRemoteControl {target_id, viewer_user_id, scope?, ttl_seconds?}`: the person at the computer says yes.
pub async fn grant_remote_control(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let user = person(scope, user_id, "deciding on remote control", correlation)?;
    let target_id = target_param(params, correlation)?;
    let viewer = viewer_param(params, correlation)?;
    let by = identity_of(state, scope, user, correlation).await?;
    let hub = state.remote();
    // A request that lapsed is recorded as lapsed before anyone answers it.
    record_ended(state, hub.sweep(&scope.tenant_id, now_ms())).await;
    let existing = hub
        .sessions_of(&scope.tenant_id, &by)
        .into_iter()
        .find(|view| view.target_id == target_id && view.viewer_user_id == viewer);
    let Some(existing) = existing else {
        return Err(refuse(
            ApiErrorCode::NotFound,
            "no such request",
            correlation,
        ));
    };
    let granted = match params.get("scope") {
        None | Some(Value::Null) => existing.requested,
        Some(value) => value.as_str().and_then(Scope::parse).ok_or_else(|| {
            refuse(
                ApiErrorCode::ValidationSchema,
                "scope is view or control",
                correlation,
            )
        })?,
    };
    let seconds = match params.get("ttl_seconds") {
        None | Some(Value::Null) => hub.config().grant_max_seconds.min(900),
        Some(value) => value.as_i64().ok_or_else(|| {
            refuse(
                ApiErrorCode::ValidationSchema,
                "ttl_seconds is a whole number",
                correlation,
            )
        })?,
    };
    // The agent must not be driving a computer a person is about to drive: a control grant needs the person to hold native input.
    if granted == Scope::Control {
        let mut tx = state.tenant_tx(&scope.tenant_id, correlation).await?;
        let control = ComputerControlStore::load(&mut tx, &scope.tenant_id, target_id).await;
        tx.commit()
            .await
            .map_err(|error| refuse(ApiErrorCode::Internal, error.to_string(), correlation))?;
        if matches!(control, Ok(ref control) if matches!(control.holder, MachineHolder::Agent)) {
            return Err(refuse(
                ApiErrorCode::ConflictState,
                "the agent is driving this computer; take over native input first (RequestTakeover) before granting control",
                correlation,
            ));
        }
    }
    let (view, transition) = hub
        .decide(
            &scope.tenant_id,
            target_id,
            viewer,
            &by,
            Decision::Grant {
                scope: granted,
                seconds,
            },
            now_ms(),
        )
        .map_err(|refusal| refusal_error(refusal, correlation))?;
    let reason = format!("{} granted for {seconds}s", granted.as_str());
    if let Err(error) = record(
        state,
        identity,
        Some(user),
        &view,
        "GrantRemoteControl",
        &reason,
        correlation,
    )
    .await
    {
        let ended = hub.stop_target(&scope.tenant_id, target_id, StopReason::HostLost);
        record_ended(state, ended).await;
        return Err(error);
    }
    let _ = transition;
    Ok(json!({ "session": view.json() }))
}

fn viewer_param<'a>(params: &'a Value, correlation: &str) -> Result<&'a str, ApiError> {
    params
        .get("viewer_user_id")
        .and_then(Value::as_str)
        .filter(|id| id.starts_with("usr_"))
        .ok_or_else(|| {
            refuse(
                ApiErrorCode::ValidationSchema,
                "viewer_user_id is a usr_ id",
                correlation,
            )
        })
}

/// `DenyRemoteControl {target_id, viewer_user_id}`: the person at the computer says no.
pub async fn deny_remote_control(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let user = person(scope, user_id, "deciding on remote control", correlation)?;
    let target_id = target_param(params, correlation)?;
    let viewer = viewer_param(params, correlation)?;
    let by = identity_of(state, scope, user, correlation).await?;
    record_ended(state, state.remote().sweep(&scope.tenant_id, now_ms())).await;
    let (view, _) = state
        .remote()
        .decide(
            &scope.tenant_id,
            target_id,
            viewer,
            &by,
            Decision::Deny,
            now_ms(),
        )
        .map_err(|refusal| refusal_error(refusal, correlation))?;
    record(
        state,
        identity,
        Some(user),
        &view,
        "DenyRemoteControl",
        "denied",
        correlation,
    )
    .await?;
    Ok(json!({ "session": view.json() }))
}

/// `StopRemoteControl {target_id, viewer_user_id?}`: the person at the computer stops every session on it (or one viewer's), or a viewer
/// stops their own.
pub async fn stop_remote_control(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let user = person(scope, user_id, "stopping remote control", correlation)?;
    let target_id = target_param(params, correlation)?;
    let viewer = match params.get("viewer_user_id") {
        None | Some(Value::Null) => None,
        Some(_) => Some(viewer_param(params, correlation)?),
    };
    let by = identity_of(state, scope, user, correlation).await?;
    let stopped = state
        .remote()
        .stop(&scope.tenant_id, target_id, viewer, &by)
        .map_err(|refusal| refusal_error(refusal, correlation))?;
    let mut sessions = Vec::new();
    for (view, transition) in stopped {
        let reason = transition.stop.map_or("stopped", StopReason::as_str);
        record(
            state,
            identity,
            Some(user),
            &view,
            "StopRemoteControl",
            reason,
            correlation,
        )
        .await?;
        sessions.push(view.json());
    }
    Ok(json!({ "sessions": sessions }))
}

/// The agent took native input back: no person may hold it, so every live session on the computer ends.
pub async fn agent_resumed(state: &ApiState, tenant_id: &str, target_id: &str) {
    let ended = state
        .remote()
        .stop_target(tenant_id, target_id, StopReason::AgentResumed);
    record_ended(state, ended).await;
}

/// What a `/v1/stream` socket brings to the remote relay: the tenant and the identity it was opened with. A service principal and an
/// unsigned socket have none, so they are admitted no `remote.*` frame and may send none.
#[derive(Debug, Clone)]
pub struct SocketRelay {
    tenant_id: String,
    identity: Option<Identity>,
}

impl SocketRelay {
    /// The relay half of a socket opened by `scope`.
    pub async fn new(state: &ApiState, scope: &TenantScope) -> Self {
        let identity = match (scope.user_id.as_deref(), scope.principal.is_none()) {
            (Some(user), true) => identity_of(state, scope, user, "stream").await.ok(),
            _ => None,
        };
        Self {
            tenant_id: scope.tenant_id.clone(),
            identity,
        }
    }

    /// Whether this socket may be sent a `remote.*` live frame.
    #[must_use]
    pub fn admits(&self, state: &ApiState, frame: &Value) -> bool {
        self.identity
            .as_ref()
            .is_some_and(|who| state.remote().admits(&self.tenant_id, who, frame))
    }

    /// Handle one text message the socket sent: a remote message is relayed, and a refusal is the answer to send back.
    pub fn upstream(&self, state: &ApiState, text: &str) -> Option<Value> {
        let Some(who) = &self.identity else {
            return Some(json!({ "remote": { "ok": false, "reason": "wrong_party" } }));
        };
        match state
            .remote()
            .upstream(&self.tenant_id, who, text, now_ms())
        {
            Ok(relays) => {
                for relay in relays {
                    state
                        .live()
                        .publish(&self.tenant_id, &relay.channel, relay.frame);
                }
                None
            }
            Err(dropped) => Some(dropped_frame(&dropped)),
        }
    }

    /// The credential this socket was opened with no longer stands: the viewer's sessions end, and the host's sessions end with the sign-in.
    pub async fn lapsed(&self, state: &ApiState) {
        if let Some(who) = &self.identity {
            let ended =
                state
                    .remote()
                    .stop_viewer(&self.tenant_id, who, StopReason::ViewerSignedOut);
            record_ended(state, ended).await;
        }
    }

    /// Advance the clock for this tenant and record what ended.
    pub async fn sweep(&self, state: &ApiState) {
        let ended = state.remote().sweep(&self.tenant_id, now_ms());
        if !ended.is_empty() {
            record_ended(state, ended).await;
        }
    }
}

/// The error frame a refused upstream message is answered with.
#[must_use]
pub fn dropped_frame(dropped: &Dropped) -> Value {
    json!({ "remote": { "ok": false, "reason": dropped.reason() } })
}

/// The wall clock the relay uses.
#[must_use]
pub fn clock_ms() -> i64 {
    now_ms()
}
