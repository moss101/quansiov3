//! The hub: accepts workers, holds their leases, and carries dispatches to them.
//!
//! The hub runs no timer. A lease is renewed when the worker's heartbeat arrives (DOMAIN.md §8.2), so a worker
//! that goes quiet is fenced when its lease lapses.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::runtime::state_machine::LiveFramePort;
use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_machine::control::browser::{
    BrowserSessionStore, DecisionKind, GuardReport, SessionStatus,
};
use quansio_machine::control::{
    Lease, LeaseRequest, MachineControl, MachineError, TargetFence, TargetStatus,
};
use quansio_machine::gateway::wire::{
    self, BrowserOrder, FromWorker, LeaseWire, OutcomeWire, ToWorker,
};
use quansio_machine::gateway::{ActionEnvelope, DispatchRequest, Heartbeat, MachineGateway};
use serde_json::json;
use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

/// How the hub holds leases.
#[derive(Debug, Clone)]
pub struct HubConfig {
    /// The controller identity the hub takes leases as.
    pub controller_id: String,
    /// How long a lease lasts before it must be renewed.
    pub lease_ttl_seconds: i64,
    /// How long a connecting worker has to say who it is.
    pub hello_timeout: Duration,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            controller_id: format!("ctl_worker_hub_{}", std::process::id()),
            lease_ttl_seconds: 60,
            hello_timeout: Duration::from_secs(5),
        }
    }
}

/// Why the hub could not do what was asked.
#[derive(Debug, thiserror::Error)]
pub enum HubError {
    /// The socket could not be bound.
    #[error("the worker socket could not be bound: {0}")]
    Bind(std::io::Error),
    /// No worker is attached for the workspace.
    #[error("no worker is attached for workspace {0}")]
    NoWorker(String),
    /// The lease is not live in the control plane, so nothing is sent.
    #[error("the lease is not live: {0}")]
    LeaseNotLive(String),
    /// The dispatch could not be put on the wire.
    #[error("the dispatch could not be sent: {0}")]
    Send(String),
    /// The dispatch was sent and the worker did not answer in time or went away: the outcome is unknown.
    #[error("the worker did not answer: {0}")]
    NoAnswer(String),
    /// The envelope was malformed.
    #[error("the envelope was malformed: {0}")]
    Envelope(String),
}

/// A worker as the hub sees it, for diagnostics and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSnapshot {
    /// Tenant.
    pub tenant_id: String,
    /// Workspace of the target.
    pub workspace_id: String,
    /// Target.
    pub target_id: String,
    /// Lease the hub holds on it.
    pub lease_id: String,
    /// Generation of that lease.
    pub generation: i64,
}

type Answer = (OutcomeWire, bool);

/// A worker is found by the tenant and target it serves.
type LinkKey = (String, String);

type LinkMap = Mutex<HashMap<LinkKey, Arc<Link>>>;

/// The dispatches sent and not yet answered, by `(tenant, run)`: the worker (`(tenant, target)`) and the token of each.
type InFlightTable = Mutex<HashMap<LinkKey, Vec<(LinkKey, String)>>>;

struct Link {
    tenant_id: String,
    workspace_id: String,
    target_id: String,
    connection: u64,
    outbound: mpsc::Sender<String>,
    pending: Mutex<HashMap<String, oneshot::Sender<Answer>>>,
    lease: Mutex<Lease>,
    /// Sessions already proven to be on this worker's target, so a frame costs no query.
    verified: Mutex<HashSet<String>>,
    /// Evidence a worker sent ahead of the result that names it, by dispatch token; at most [`MAX_HELD_EVIDENCE`], the
    /// oldest dropped first, so a worker that sends evidence nobody asked for cannot grow the server.
    evidence: Mutex<std::collections::VecDeque<(String, HeldEvidence)>>,
}

/// How much evidence waits for its result at most, per worker.
const MAX_HELD_EVIDENCE: usize = 8;

/// Evidence bytes a worker sent with a dispatch, still base64.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldEvidence {
    /// Media type the worker claimed; the host checks the bytes against it.
    pub media_type: String,
    /// The bytes, base64.
    pub data_base64: String,
}

/// Accepts workers and carries dispatches to them.
#[derive(Clone)]
pub struct WorkerHub {
    pool: PgPool,
    config: Arc<HubConfig>,
    links: Arc<LinkMap>,
    connections: Arc<std::sync::atomic::AtomicU64>,
    /// Where a worker's frames are published for whoever is watching; set once the API state exists.
    live: Arc<Mutex<Option<Arc<dyn LiveFramePort>>>>,
    /// How many live sockets are watching each target's channel, by `(tenant, target)`.
    viewers: Arc<Mutex<HashMap<LinkKey, usize>>>,
    /// The dispatches sent and not yet answered, by `(tenant, run)`: the worker and the token of each.
    in_flight: Arc<InFlightTable>,
    /// The socket the hub listens on, once it does.
    socket: Arc<Mutex<Option<std::path::PathBuf>>>,
}

impl std::fmt::Debug for WorkerHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerHub").finish_non_exhaustive()
    }
}

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

impl WorkerHub {
    /// A hub over the control-plane pool.
    #[must_use]
    pub fn new(pool: PgPool, config: HubConfig) -> Self {
        Self {
            pool,
            config: Arc::new(config),
            links: Arc::new(Mutex::new(HashMap::new())),
            connections: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            live: Arc::new(Mutex::new(None)),
            viewers: Arc::new(Mutex::new(HashMap::new())),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            socket: Arc::new(Mutex::new(None)),
        }
    }

    /// The socket the hub listens on, when it does: where a worker started on this machine dials in.
    #[must_use]
    pub fn socket_path(&self) -> Option<std::path::PathBuf> {
        self.socket
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Stop every dispatch of a run that a worker is still running (a person cancelled or paused the run).
    ///
    /// Each worker is sent `cancel {dispatch_token}`: a dispatch still waiting for its turn ends `cancelled` (nothing ran)
    /// and one already running is stopped and ends `interrupted`, which settles as unknown — the caller waiting for it
    /// gets that answer. Returns how many cancels were sent.
    pub async fn cancel_run(&self, tenant_id: &str, run_id: &str) -> usize {
        let running = self
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(tenant_id.to_string(), run_id.to_string()))
            .cloned()
            .unwrap_or_default();
        let mut sent = 0;
        for ((tenant, target), dispatch_token) in running {
            let link = self
                .links
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&(tenant, target))
                .cloned();
            let Some(link) = link else { continue };
            let Ok(line) = wire::encode(&ToWorker::Cancel { dispatch_token }) else {
                continue;
            };
            if link.outbound.send(line).await.is_ok() {
                sent += 1;
            }
        }
        sent
    }

    /// The tokens of a run's dispatches that are sent and not yet answered.
    #[must_use]
    pub fn running(&self, tenant_id: &str, run_id: &str) -> Vec<String> {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(tenant_id.to_string(), run_id.to_string()))
            .map(|running| running.iter().map(|(_, token)| token.clone()).collect())
            .unwrap_or_default()
    }

    /// The control-plane pool the hub leases through.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The target of the worker attached for a workspace, when one is.
    #[must_use]
    pub fn target_for_workspace(&self, tenant_id: &str, workspace_id: &str) -> Option<String> {
        self.link_for_workspace(tenant_id, workspace_id)
            .map(|link| link.target_id.clone())
    }

    /// The workers attached now.
    #[must_use]
    pub fn links(&self) -> Vec<LinkSnapshot> {
        let links = self.links.lock().unwrap_or_else(PoisonError::into_inner);
        links
            .values()
            .map(|link| {
                let lease = link.lease.lock().unwrap_or_else(PoisonError::into_inner);
                LinkSnapshot {
                    tenant_id: link.tenant_id.clone(),
                    workspace_id: link.workspace_id.clone(),
                    target_id: link.target_id.clone(),
                    lease_id: lease.id.clone(),
                    generation: lease.holder_generation,
                }
            })
            .collect()
    }

    /// Listen on a Unix socket that only the owner can use, and accept workers until the task is dropped.
    ///
    /// A stale socket file from a previous process is replaced; a live one is not touched by a second hub
    /// because binding over it fails.
    ///
    /// # Errors
    /// Returns [`HubError::Bind`] when the socket cannot be created or restricted.
    pub fn listen(&self, path: &Path) -> Result<tokio::task::JoinHandle<()>, HubError> {
        use std::os::unix::fs::PermissionsExt;
        if path.exists() {
            match std::os::unix::net::UnixStream::connect(path) {
                Ok(_) => {
                    return Err(HubError::Bind(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        "another hub is listening on it",
                    )))
                }
                Err(_) => std::fs::remove_file(path).map_err(HubError::Bind)?,
            }
        }
        let listener = UnixListener::bind(path).map_err(HubError::Bind)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(HubError::Bind)?;
        *self.socket.lock().unwrap_or_else(PoisonError::into_inner) = Some(path.to_path_buf());
        let accepting = tokio::spawn(self.clone().accept_loop(listener));
        Ok(accepting)
    }

    /// Accept connections for as long as the listener lives. This is a connection acceptor, not a timer.
    async fn accept_loop(self, listener: UnixListener) {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let hub = self.clone();
                    tokio::spawn(async move { hub.connection(stream).await });
                }
                Err(error) => {
                    eprintln!("worker hub: accept failed: {error}");
                    tokio::task::yield_now().await;
                }
            }
        }
    }

    async fn take_lease(&self, tenant: &str, target_id: &str) -> Result<(Lease, String), String> {
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        let target = MachineControl::load(&mut tx, tenant, target_id)
            .await
            .map_err(|e| e.to_string())?;
        if !target.substrate.is_outbound_only() {
            return Err(format!(
                "target {target_id} is a {} target; only a customer_private_worker dials in",
                target.substrate.as_str()
            ));
        }
        if target.status != TargetStatus::Ready {
            return Err(format!(
                "target {target_id} is {}, not READY",
                target.status.as_str()
            ));
        }
        let lease_id = CanonicalId::generate(Prefix::Lease, &mut UlidGenerator::new()).to_string();
        let lease = MachineControl::acquire_lease(
            &mut tx,
            tenant,
            &LeaseRequest {
                lease_id: &lease_id,
                target_id,
                controller_id: &self.config.controller_id,
                fence: &TargetFence {
                    generation: target.generation,
                },
                ttl_seconds: self.config.lease_ttl_seconds,
                now: &now(),
            },
        )
        .await
        .map_err(|error| match error {
            MachineError::LeaseHeld { holder, until, .. } => {
                format!("target {target_id} is leased by {holder} until {until}")
            }
            other => other.to_string(),
        })?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok((lease, target.workspace_id))
    }

    async fn connection(&self, stream: UnixStream) {
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        let hello = match tokio::time::timeout(self.config.hello_timeout, lines.next_line()).await {
            Ok(Ok(Some(line))) => wire::decode::<FromWorker>(&line),
            _ => return,
        };
        let Ok(FromWorker::Hello {
            tenant_id,
            target_id,
        }) = hello
        else {
            return;
        };
        let (lease, workspace_id) = match self.take_lease(&tenant_id, &target_id).await {
            Ok(taken) => taken,
            Err(reason) => {
                eprintln!("worker hub: refused worker for {target_id}: {reason}");
                return;
            }
        };
        let connection = self
            .connections
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (outbound, mut queue) = mpsc::channel::<String>(64);
        let link = Arc::new(Link {
            tenant_id: tenant_id.clone(),
            workspace_id,
            target_id: target_id.clone(),
            connection,
            outbound,
            pending: Mutex::new(HashMap::new()),
            lease: Mutex::new(lease.clone()),
            verified: Mutex::new(HashSet::new()),
            evidence: Mutex::new(std::collections::VecDeque::new()),
        });
        let key = (tenant_id.clone(), target_id.clone());
        self.links
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.clone(), Arc::clone(&link));
        if Self::send_lease(&link, &lease).await.is_err() {
            self.drop_link(&key, &link, false).await;
            return;
        }
        eprintln!("worker hub: worker attached for {target_id}");

        let writer_task = tokio::spawn(async move {
            while let Some(line) = queue.recv().await {
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        // A decision made while this worker was away reached no one. What the people decided is on the sessions, so it is
        // told to the worker as it arrives; a decision it already applied is refused by its driver, which is harmless.
        self.replay_decisions(&link).await;
        while let Ok(Some(line)) = lines.next_line().await {
            match wire::decode::<FromWorker>(&line) {
                Ok(FromWorker::Result {
                    dispatch_token,
                    replayed,
                    outcome,
                    ..
                }) => {
                    let waiter = link
                        .pending
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&dispatch_token);
                    if let Some(waiter) = waiter {
                        let _ = waiter.send((outcome, replayed));
                    }
                }
                Ok(FromWorker::Heartbeat {
                    target_id,
                    generation,
                    observed_state,
                    at,
                }) => {
                    self.heartbeat(
                        &link,
                        &Heartbeat {
                            target_id,
                            generation,
                            observed_state,
                            at,
                        },
                    )
                    .await;
                }
                Ok(FromWorker::Evidence {
                    dispatch_token,
                    media_type,
                    data_base64,
                }) => {
                    // Kept only for a dispatch this hub is waiting on: evidence for anything else is dropped.
                    let waiting = link
                        .pending
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .contains_key(&dispatch_token);
                    if waiting {
                        let mut held = link.evidence.lock().unwrap_or_else(PoisonError::into_inner);
                        held.push_back((
                            dispatch_token,
                            HeldEvidence {
                                media_type,
                                data_base64,
                            },
                        ));
                        while held.len() > MAX_HELD_EVIDENCE {
                            held.pop_front();
                        }
                    }
                }
                Ok(FromWorker::BrowserGuard { session_id, report }) => {
                    self.record_guard(&link, &session_id, &report).await;
                }
                Ok(FromWorker::BrowserFrame {
                    session_id,
                    tab_id,
                    media_type,
                    data,
                    url,
                    device_width,
                    device_height,
                    sequence,
                }) => {
                    let frame = Frame {
                        tab_id,
                        media_type,
                        data,
                        url,
                        device_width,
                        device_height,
                        sequence,
                    };
                    self.publish_frame(&link, &session_id, frame).await;
                }
                _ => {}
            }
        }
        writer_task.abort();
        self.drop_link(&key, &link, true).await;
    }

    /// Tell a worker that has just attached every certificate and permission decision recorded for its target's open sessions,
    /// oldest first, so the last word on a subject is the one it ends up with.
    async fn replay_decisions(&self, link: &Arc<Link>) {
        let sessions = async {
            let mut tx = self.pool.begin().await.ok()?;
            crate::control::schema::set_tenant_context(&mut tx, &link.tenant_id)
                .await
                .ok()?;
            BrowserSessionStore::list_for_target(&mut tx, &link.tenant_id, &link.target_id)
                .await
                .ok()
        }
        .await
        .unwrap_or_default();
        for session in sessions
            .into_iter()
            .filter(|session| session.status != SessionStatus::Closed)
        {
            for decision in &session.guard.decisions {
                let receipt = decision.to_receipt_wire(&session.id);
                let order = match decision.kind {
                    DecisionKind::Certificate => BrowserOrder::Certificate { receipt },
                    DecisionKind::Permission => BrowserOrder::Permission { receipt },
                };
                if let Ok(line) = wire::encode(&ToWorker::Browser(order)) {
                    let _ = link.outbound.send(line).await;
                }
            }
        }
    }

    async fn send_lease(link: &Link, lease: &Lease) -> Result<(), ()> {
        let frame = ToWorker::Lease(LeaseWire {
            lease_id: lease.id.clone(),
            target_id: lease.target_id.clone(),
            generation: lease.holder_generation,
            expires_at: lease.expires_at.clone(),
        });
        let line = wire::encode(&frame).map_err(|_| ())?;
        link.outbound.send(line).await.map_err(|_| ())
    }

    /// A live socket has started watching a target's channel. Frames flow from the target's browsers while at least one
    /// socket is; the returned guard ends the watch when it is dropped, however the socket ended.
    #[must_use]
    pub fn viewer_joined(&self, tenant_id: &str, target_id: &str) -> Viewer {
        let key = (tenant_id.to_string(), target_id.to_string());
        let first = {
            let mut viewers = self.viewers.lock().unwrap_or_else(PoisonError::into_inner);
            let count = viewers.entry(key.clone()).or_insert(0);
            *count += 1;
            *count == 1
        };
        // The first viewer starts the stream; every viewer, first or not, is sent a current picture.
        let _ = first;
        tokio::spawn(self.clone().stream_target(key.clone(), true));
        Viewer {
            hub: self.clone(),
            key,
        }
    }

    /// Whether anyone is watching the target's channel now.
    #[must_use]
    pub fn has_viewers(&self, tenant_id: &str, target_id: &str) -> bool {
        self.viewers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(tenant_id.to_string(), target_id.to_string()))
            .is_some_and(|count| *count > 0)
    }

    /// Ask the worker to start or stop the stream of every open browser session on a target.
    async fn stream_target(self, key: LinkKey, enabled: bool) {
        let sessions = async {
            let mut tx = self.pool.begin().await.ok()?;
            crate::control::schema::set_tenant_context(&mut tx, &key.0)
                .await
                .ok()?;
            BrowserSessionStore::list_for_target(&mut tx, &key.0, &key.1)
                .await
                .ok()
        }
        .await
        .unwrap_or_default();
        for session in sessions
            .into_iter()
            .filter(|session| session.status != SessionStatus::Closed)
        {
            self.send_order(
                &key.0,
                &key.1,
                BrowserOrder::Screencast {
                    session_id: session.id,
                    enabled,
                    refresh: enabled,
                },
            )
            .await;
        }
    }

    /// Where this hub publishes the frames its workers send. The API state attaches its live hub here.
    pub fn attach_live(&self, live: Arc<dyn LiveFramePort>) {
        *self.live.lock().unwrap_or_else(PoisonError::into_inner) = Some(live);
    }

    /// Publish one frame of a session's tab, if the session is on this worker's target.
    ///
    /// Frames are transient (DOMAIN.md §9.3): published to the target's channel for whoever is watching, never stored.
    /// A worker cannot show a frame as another target's session.
    async fn publish_frame(&self, link: &Arc<Link>, session_id: &str, frame: Frame) {
        let Some(live) = self
            .live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return;
        };
        let known = link
            .verified
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(session_id);
        if !known {
            let on_target = async {
                let mut tx = self.pool.begin().await.ok()?;
                crate::control::schema::set_tenant_context(&mut tx, &link.tenant_id)
                    .await
                    .ok()?;
                let session = BrowserSessionStore::load(&mut tx, &link.tenant_id, session_id)
                    .await
                    .ok()?;
                Some(session.target_id == link.target_id)
            }
            .await;
            if on_target != Some(true) {
                return;
            }
            link.verified
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(session_id.to_string());
        }
        live.publish(
            &link.tenant_id,
            &format!("target:{}", link.target_id),
            json!({
                "kind": "browser.frame",
                "session_id": session_id,
                "tab_id": frame.tab_id,
                "media_type": frame.media_type,
                "data": frame.data,
                "url": frame.url,
                "width": frame.device_width,
                "height": frame.device_height,
                "cursor": null,
                "sequence": frame.sequence,
            }),
        );
    }

    /// Store what a worker's driver reported about one of its sessions.
    ///
    /// Only for a session on the target this worker holds the lease of: a worker cannot write another target's
    /// session. The report is parsed strictly and bounded by the control plane (DOMAIN.md §8.4.1 E); one that does not
    /// parse is dropped, and the previous report stands.
    async fn record_guard(&self, link: &Arc<Link>, session_id: &str, report: &serde_json::Value) {
        let Ok(report) = GuardReport::from_json(report) else {
            return;
        };
        let stored = async {
            let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
            crate::control::schema::set_tenant_context(&mut tx, &link.tenant_id)
                .await
                .map_err(|e| e.to_string())?;
            let session = BrowserSessionStore::load(&mut tx, &link.tenant_id, session_id)
                .await
                .map_err(|e| e.to_string())?;
            if session.target_id != link.target_id {
                return Err(format!(
                    "session {session_id} is not on target {}",
                    link.target_id
                ));
            }
            BrowserSessionStore::record_guard_report(
                &mut tx,
                &link.tenant_id,
                session_id,
                &report,
                &now(),
            )
            .await
            .map_err(|e| e.to_string())?;
            tx.commit().await.map_err(|e| e.to_string())
        }
        .await;
        if let Err(why) = stored {
            eprintln!("worker hub: a guard report for {session_id} was not stored: {why}");
        }
    }

    /// Send an order about a browser session to the worker that runs it.
    ///
    /// The session's target is read from the control plane, so a caller names a session, never a worker. `Relay::NoWorker`
    /// is not a failure of the command that caused the order: what the user decided is recorded either way, and the order
    /// is repeated by the server before the next agent dispatch where it matters (control).
    pub async fn relay(&self, tenant_id: &str, order: BrowserOrder) -> Relay {
        let target_id = {
            let Ok(mut tx) = self.pool.begin().await else {
                return Relay::NoWorker;
            };
            if crate::control::schema::set_tenant_context(&mut tx, tenant_id)
                .await
                .is_err()
            {
                return Relay::NoWorker;
            }
            match BrowserSessionStore::load(&mut tx, tenant_id, order.session_id()).await {
                Ok(session) => session.target_id,
                Err(_) => return Relay::NoWorker,
            }
        };
        self.send_order(tenant_id, &target_id, order).await
    }

    /// Send an order to the worker attached for `target_id`.
    pub async fn send_order(&self, tenant_id: &str, target_id: &str, order: BrowserOrder) -> Relay {
        let link = self
            .links
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(tenant_id.to_string(), target_id.to_string()))
            .cloned();
        let Some(link) = link else {
            return Relay::NoWorker;
        };
        let Ok(line) = wire::encode(&ToWorker::Browser(order)) else {
            return Relay::NoWorker;
        };
        if link.outbound.send(line).await.is_ok() {
            Relay::Sent
        } else {
            Relay::NoWorker
        }
    }

    /// A worker's heartbeat: accept it only for the lease generation the hub holds, record what the worker
    /// observes, and renew the lease. A refused heartbeat changes nothing, so a fenced worker cannot keep a
    /// lease alive; its own copy lapses and it refuses work.
    async fn heartbeat(&self, link: &Arc<Link>, beat: &Heartbeat) {
        let held = link
            .lease
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if MachineGateway::accept_heartbeat(beat, &link.target_id, held.holder_generation).is_err()
        {
            return;
        }
        let renewed = async {
            let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
            MachineControl::observe(
                &mut tx,
                &link.tenant_id,
                &link.target_id,
                &beat.observed_state,
                &beat.at,
            )
            .await
            .map_err(|e| e.to_string())?;
            let lease = MachineControl::renew_lease(
                &mut tx,
                &link.tenant_id,
                &held.id,
                held.holder_generation,
                self.config.lease_ttl_seconds,
                &now(),
            )
            .await
            .map_err(|e| e.to_string())?;
            tx.commit().await.map_err(|e| e.to_string())?;
            Ok::<_, String>(lease)
        }
        .await;
        if let Ok(lease) = renewed {
            *link.lease.lock().unwrap_or_else(PoisonError::into_inner) = lease.clone();
            let _ = Self::send_lease(link, &lease).await;
        }
    }

    async fn drop_link(&self, key: &LinkKey, link: &Arc<Link>, release: bool) {
        {
            let mut links = self.links.lock().unwrap_or_else(PoisonError::into_inner);
            // Only remove the entry this connection owns; a newer connection may have replaced it.
            if links
                .get(key)
                .is_some_and(|current| current.connection == link.connection)
            {
                links.remove(key);
            }
        }
        // Dropping the senders wakes every waiter with "no answer": their outcome is unknown.
        link.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        if release {
            let id = link
                .lease
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .id
                .clone();
            if let Ok(mut tx) = self.pool.begin().await {
                if MachineControl::release_lease(&mut tx, &link.tenant_id, &id)
                    .await
                    .is_ok()
                {
                    let _ = tx.commit().await;
                }
            }
        }
    }

    fn link_for_workspace(&self, tenant_id: &str, workspace_id: &str) -> Option<Arc<Link>> {
        self.links
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .find(|link| link.tenant_id == tenant_id && link.workspace_id == workspace_id)
            .cloned()
    }

    /// Send one dispatch to the worker attached for the workspace and wait for its answer.
    ///
    /// The lease is validated in the control plane first. If the dispatch is sent and no answer comes, the
    /// error is [`HubError::NoAnswer`] and the caller must treat the effect as unknown; it is never re-sent.
    ///
    /// # Errors
    /// See [`HubError`].
    pub async fn dispatch(
        &self,
        tenant_id: &str,
        workspace_id: &str,
        request: &DispatchArgs<'_>,
    ) -> Result<(OutcomeWire, bool, Option<HeldEvidence>), HubError> {
        let link = self
            .link_for_workspace(tenant_id, workspace_id)
            .ok_or_else(|| HubError::NoWorker(workspace_id.to_string()))?;
        let lease = link
            .lease
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| HubError::LeaseNotLive(e.to_string()))?;
            MachineControl::validate_lease(
                &mut tx,
                tenant_id,
                &lease.id,
                lease.holder_generation,
                &now(),
            )
            .await
            .map_err(|e| HubError::LeaseNotLive(e.to_string()))?;
        }
        let envelope: ActionEnvelope = ActionEnvelope::dispatch(&DispatchRequest {
            effect_id: request.effect_id,
            dispatch_token: request.dispatch_token,
            target_id: &link.target_id,
            lease_id: &lease.id,
            generation: lease.holder_generation,
            run_id: request.run_id,
            step_id: request.step_id,
            tool: request.tool,
            tool_version: request.tool_version,
            args_json: request.args_json,
            max_output_bytes: request.max_output_bytes,
            deadline_at: request.deadline_at,
        })
        .map_err(|rejection| HubError::Envelope(rejection.to_string()))?;
        let envelope = match request.approved_tier {
            Some(tier) => envelope.with_approved_tier(tier),
            None => envelope,
        };
        // Belt and braces: the gateway's own rules, applied here as the worker will apply them.
        MachineGateway::validate(
            &envelope,
            &quansio_machine::gateway::LeaseClaim {
                lease_id: lease.id.clone(),
                target_id: link.target_id.clone(),
                generation: lease.holder_generation,
                held: true,
                expires_at: lease.expires_at.clone(),
            },
            &now(),
        )
        .map_err(|rejection| HubError::Envelope(rejection.to_string()))?;

        let (answer, waiting) = oneshot::channel();
        link.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(request.dispatch_token.to_string(), answer);
        // Known to `cancel_run` from before it is sent until this call returns, however it returns.
        let _in_flight = InFlight::enter(
            &self.in_flight,
            (tenant_id.to_string(), request.run_id.to_string()),
            (
                (link.tenant_id.clone(), link.target_id.clone()),
                request.dispatch_token.to_string(),
            ),
        );
        let line = wire::encode(&ToWorker::Dispatch(Box::new((&envelope).into())))
            .map_err(|e| HubError::Send(e.to_string()))?;
        if link.outbound.send(line).await.is_err() {
            link.pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(request.dispatch_token);
            return Err(HubError::Send("the worker's channel is closed".to_string()));
        }
        match tokio::time::timeout(request.wait, waiting).await {
            Ok(Ok((outcome, replayed))) => {
                let evidence = {
                    let mut held = link.evidence.lock().unwrap_or_else(PoisonError::into_inner);
                    held.iter()
                        .position(|(token, _)| token == request.dispatch_token)
                        .and_then(|at| held.remove(at))
                        .map(|(_, evidence)| evidence)
                };
                Ok((outcome, replayed, evidence))
            }
            Ok(Err(_)) => Err(HubError::NoAnswer("the worker disconnected".to_string())),
            Err(_) => {
                link.pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(request.dispatch_token);
                Err(HubError::NoAnswer(
                    "the worker did not answer in time".to_string(),
                ))
            }
        }
    }
}

/// A live socket's watch on a target's channel; dropping it ends the watch.
pub struct Viewer {
    hub: WorkerHub,
    key: LinkKey,
}

impl std::fmt::Debug for Viewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewer")
            .field("target", &self.key.1)
            .finish()
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let last = {
            let mut viewers = self
                .hub
                .viewers
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match viewers.get_mut(&self.key) {
                Some(count) if *count > 1 => {
                    *count -= 1;
                    false
                }
                Some(_) => {
                    viewers.remove(&self.key);
                    true
                }
                None => false,
            }
        };
        if last {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(self.hub.clone().stream_target(self.key.clone(), false));
            }
        }
    }
}

/// A dispatch's entry in the hub's in-flight table, removed when the dispatch call ends.
struct InFlight {
    table: Arc<InFlightTable>,
    run: LinkKey,
    token: String,
}

impl InFlight {
    fn enter(table: &Arc<InFlightTable>, run: LinkKey, entry: (LinkKey, String)) -> Self {
        let token = entry.1.clone();
        table
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(run.clone())
            .or_default()
            .push(entry);
        Self {
            table: Arc::clone(table),
            run,
            token,
        }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(running) = table.get_mut(&self.run) {
            running.retain(|(_, token)| token != &self.token);
            if running.is_empty() {
                table.remove(&self.run);
            }
        }
    }
}

/// One picture of a tab as the worker sent it.
struct Frame {
    tab_id: String,
    media_type: String,
    data: String,
    url: String,
    device_width: u32,
    device_height: u32,
    sequence: u64,
}

/// Whether an order reached a worker's channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relay {
    /// It was put on the worker's channel.
    Sent,
    /// No worker is attached for the session's target.
    NoWorker,
}

/// What a dispatch carries.
#[derive(Debug, Clone)]
pub struct DispatchArgs<'a> {
    /// Effect settled.
    pub effect_id: &'a str,
    /// Fence token.
    pub dispatch_token: &'a str,
    /// Run.
    pub run_id: &'a str,
    /// Step.
    pub step_id: &'a str,
    /// Tool.
    pub tool: &'a str,
    /// Declaration version.
    pub tool_version: u32,
    /// Canonical JSON arguments.
    pub args_json: &'a str,
    /// Output bound.
    pub max_output_bytes: usize,
    /// Instant the action stops being acceptable.
    pub deadline_at: &'a str,
    /// How long to wait for the worker's answer.
    pub wait: Duration,
    /// The highest consequence tier the runtime approved for the call, for a tool whose consequence is known only
    /// once the worker has looked at the page.
    pub approved_tier: Option<u8>,
}
