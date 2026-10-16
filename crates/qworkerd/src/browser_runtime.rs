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
use tokio::sync::Mutex;

use crate::browser::tools::ToolReply;
use crate::browser::{BrowserCallContext, BrowserDriver, BrowserFault, DriverConfig, FaultCode};
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
        Self {
            chrome: chrome.into(),
            state_dir: state_dir.into(),
            tenant_id: tenant_id.into(),
            allow_private_network: false,
            sessions: Mutex::new(HashMap::new()),
        }
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
