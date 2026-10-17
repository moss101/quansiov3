//! Browser sessions on a worker: one Chromium and one [`BrowserDriver`] per session, started when the session's
//! first call arrives (EXEC-009 composition over ADP-001).
//!
//! The runtime decided whether a `browser.*` call may happen before it was dispatched. What this adds is only
//! where the call runs: it keeps the session's browser alive between calls (a ref is good for one action, but the
//! page it was taken from must still be there), gives the driver the call's identity so its idempotency journal
//! and in-doubt recovery work across a worker restart, and passes the tier the runtime approved so the driver can
//! refuse a click that would settle above it.
//!
//! A reply is always a document — `ok`, `error` or `in_doubt` — never a transport failure, so the server can settle
//! the effect as exactly what the driver said it was. An outcome the driver could not establish stays `in_doubt`
//! and is not run again.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use quansio_machine::gateway::ActionEnvelope;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};

use quansio_machine::control::browser::{DecisionKind, DecisionReceipt};
use quansio_machine::gateway::wire::BrowserOrder;

use crate::browser::tools::ToolReply;
use crate::browser::{
    BrowserCallContext, BrowserDriver, BrowserFault, DriverConfig, FaultCode, ScreencastFrame,
    ScreencastHandle,
};
use crate::chrome::{vet_navigation, ChromeProcess};

struct LiveSession {
    driver: BrowserDriver,
    /// Held so the browser lives exactly as long as its session.
    _chrome: ChromeProcess,
}

/// Runs `browser.*` calls on browsers this worker launches.
pub struct BrowserRuntime {
    chrome: PathBuf,
    state_dir: PathBuf,
    tenant_id: String,
    allow_private_network: bool,
    sessions: Mutex<HashMap<String, Arc<LiveSession>>>,
    /// The running screencasts, by session.
    pumps: Mutex<HashMap<String, ScreencastHandle>>,
    frames: mpsc::Sender<ScreencastFrame>,
    frames_out: Mutex<mpsc::Receiver<ScreencastFrame>>,
}

impl std::fmt::Debug for BrowserRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserRuntime")
            .field("chrome", &self.chrome)
            .field("state_dir", &self.state_dir)
            .finish_non_exhaustive()
    }
}

fn refusal(tool: &str, code: FaultCode, message: impl Into<String>) -> Value {
    ToolReply::refused(tool, &BrowserFault::new(code, message)).to_json()
}

impl BrowserRuntime {
    /// A runtime that launches `chrome` and keeps each session's journal and profile under `state_dir`.
    #[must_use]
    pub fn new(
        chrome: impl Into<PathBuf>,
        state_dir: impl Into<PathBuf>,
        tenant_id: impl Into<String>,
    ) -> Self {
        // A viewer who is behind wants the newest picture: the queue is short and a full one drops frames.
        let (frames, frames_out) = mpsc::channel(4);
        Self {
            chrome: chrome.into(),
            state_dir: state_dir.into(),
            tenant_id: tenant_id.into(),
            allow_private_network: false,
            sessions: Mutex::new(HashMap::new()),
            pumps: Mutex::new(HashMap::new()),
            frames,
            frames_out: Mutex::new(frames_out),
        }
    }

    /// The next frame of any session whose screencast is running.
    pub async fn next_frame(&self) -> Option<ScreencastFrame> {
        self.frames_out.lock().await.recv().await
    }

    /// Let the browser be sent to loopback, private and link-local addresses. Off unless the operator who started
    /// the worker says so: those addresses are the machine itself and its network.
    #[must_use]
    pub const fn allow_private_network(mut self, allow: bool) -> Self {
        self.allow_private_network = allow;
        self
    }

    /// The sessions with a live browser.
    pub async fn live_sessions(&self) -> Vec<String> {
        self.sessions.lock().await.keys().cloned().collect()
    }

    async fn session(
        &self,
        session_id: &str,
        envelope: &ActionEnvelope,
    ) -> Result<Arc<LiveSession>, String> {
        let mut sessions = self.sessions.lock().await;
        if let Some(live) = sessions.get(session_id) {
            return Ok(Arc::clone(live));
        }
        // A session id becomes a directory name: only the ids the domain mints are accepted.
        if !session_id.starts_with("bsn_")
            || !session_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(format!("{session_id:?} is not a browser session id"));
        }
        let root = self.state_dir.join("browser").join(session_id);
        let chrome = ChromeProcess::launch(&self.chrome, &root.join("profile")).await?;
        let mut session =
            crate::browser::BrowserSession::open(session_id, &self.tenant_id, &envelope.target_id);
        session.run_id = Some(envelope.run_id.clone());
        let driver = BrowserDriver::open(
            DriverConfig::new(chrome.port, root.join("session")),
            session,
            None,
        )
        .map_err(|fault| fault.to_string())?;
        let live = Arc::new(LiveSession {
            driver,
            _chrome: chrome,
        });
        sessions.insert(session_id.to_string(), Arc::clone(&live));
        Ok(live)
    }

    async fn live(&self, session_id: &str) -> Option<Arc<LiveSession>> {
        self.sessions.lock().await.get(session_id).cloned()
    }

    /// Apply an order about a session to its driver.
    ///
    /// `Control` is idempotent and is a no-op for a session whose browser has not been launched (the browser starts agent-held).
    /// The orders that act on a page need the browser to exist.
    ///
    /// # Errors
    /// Returns why the order was not applied.
    pub async fn order(&self, order: &BrowserOrder) -> Result<(), String> {
        let Some(live) = self.live(order.session_id()).await else {
            return match order {
                BrowserOrder::Control { .. } => Ok(()),
                _ => Err(format!(
                    "no browser is running for session {}",
                    order.session_id()
                )),
            };
        };
        match order {
            BrowserOrder::Control { holder, at, .. } => match holder.as_str() {
                "user" if !live.driver.is_user_held() => live
                    .driver
                    .request_takeover(at)
                    .map_err(|fault| fault.to_string()),
                "agent" if live.driver.is_user_held() => {
                    live.driver.handback(at).map_err(|fault| fault.to_string())
                }
                "user" | "agent" => Ok(()),
                other => Err(format!("{other:?} is not a holder")),
            },
            BrowserOrder::Screencast {
                session_id,
                enabled,
            } => {
                let mut pumps = self.pumps.lock().await;
                if *enabled {
                    if pumps.contains_key(session_id) {
                        return Ok(());
                    }
                    // The tab a person is looking at is the one most recently opened.
                    let tab = live
                        .driver
                        .tab_ids()
                        .pop()
                        .ok_or_else(|| "the session has no tab to show".to_string())?;
                    let handle = live
                        .driver
                        .start_screencast(&tab, self.frames.clone())
                        .await
                        .map_err(|fault| fault.to_string())?;
                    pumps.insert(session_id.clone(), handle);
                } else if let Some(handle) = pumps.remove(session_id) {
                    handle.stop().await;
                }
                Ok(())
            }
            BrowserOrder::UserClick { tab_id, x, y, .. } => live
                .driver
                .user_click(tab_id, *x, *y)
                .await
                .map_err(|fault| fault.to_string()),
            BrowserOrder::UserText { tab_id, text, .. } => live
                .driver
                .user_text(tab_id, text)
                .await
                .map_err(|fault| fault.to_string()),
            BrowserOrder::Certificate { .. } | BrowserOrder::Permission { .. } => {
                Err("a decision is applied from its receipt, not as an order".to_string())
            }
        }
    }

    /// Apply a user's decision from the receipt the control plane minted for them.
    ///
    /// # Errors
    /// Returns why the decision was not applied.
    pub async fn decide(&self, receipt: &DecisionReceipt) -> Result<(), String> {
        let live = self
            .live(receipt.session_id())
            .await
            .ok_or_else(|| format!("no browser is running for session {}", receipt.session_id()))?;
        let applied = match receipt.kind() {
            DecisionKind::Certificate => {
                live.driver
                    .resolve_certificate(receipt.tab_id(), receipt)
                    .await
            }
            DecisionKind::Permission => {
                live.driver
                    .apply_permission_decision(receipt.tab_id(), receipt)
                    .await
            }
        };
        applied.map(|_| ()).map_err(|fault| fault.to_string())
    }

    /// The guard reports of the live sessions, or of one.
    pub async fn guards(&self, only: Option<&str>) -> Vec<(String, Value)> {
        let sessions = self.sessions.lock().await;
        sessions
            .iter()
            .filter(|(id, _)| only.is_none_or(|wanted| wanted == id.as_str()))
            .map(|(id, live)| (id.clone(), live.driver.guard_report()))
            .collect()
    }

    /// Run one `browser.*` envelope and return the driver's reply document.
    pub async fn call(&self, envelope: &ActionEnvelope) -> Value {
        let tool = envelope.tool.as_str();
        let args: Value = match serde_json::from_str(&envelope.args_json) {
            Ok(args) => args,
            Err(error) => {
                return refusal(
                    tool,
                    FaultCode::InvalidRequest,
                    format!("the arguments are not JSON: {error}"),
                )
            }
        };
        let Some(session_id) = args.get("session_id").and_then(Value::as_str) else {
            return refusal(tool, FaultCode::InvalidRequest, "session_id is required");
        };
        if tool == "browser.navigate" && !self.allow_private_network {
            if let Some(url) = args.get("url").and_then(Value::as_str) {
                if let Err(why) = vet_navigation(url).await {
                    return refusal(
                        tool,
                        FaultCode::NavigationDenied,
                        format!("navigation refused: {why}"),
                    );
                }
            }
        }
        let live = match self.session(session_id, envelope).await {
            Ok(live) => live,
            Err(why) => {
                return refusal(
                    tool,
                    FaultCode::TargetNotFound,
                    format!("no browser is available: {why}"),
                )
            }
        };
        let context = BrowserCallContext {
            run_id: envelope.run_id.clone(),
            // The effect is the identity that is stable across a re-delivery, which is what the journal keys on.
            tool_call_id: envelope.effect_id.clone(),
            action_id: envelope.dispatch_token.clone(),
            approved_tier: envelope.approved_tier,
        };
        let reply = live.driver.dispatch(tool, &args, &context).await;
        let mut document = reply.to_json();
        // Pixels do not cross the control channel yet; the reply says how many were captured and left behind.
        document["evidence_bytes"] = json!(reply.evidence.as_ref().map_or(0, Vec::len));
        document
    }
}
