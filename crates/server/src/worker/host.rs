//! The tool host that forwards worker tools to the attached worker.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quansio_machine::gateway::wire::{BrowserOrder, OutcomeWire};
use serde_json::json;

use super::browser::{
    approved_tier, require_agent_control, settle, BrowserSessions, BROWSER_TOOLS,
};
use super::hub::{DispatchArgs, HubError, WorkerHub};
use crate::runtime::turn_loop::{HostDispatch, HostFailure, HostOutcome, ToolHostPort};

/// The tools a worker hosts (`host: qworkerd` in `config/tools.yaml`, minus the ones another host owns).
pub const WORKER_TOOLS: [&str; 5] = [
    "fs.read",
    "fs.write",
    "fs.patch",
    "terminal.exec",
    "process.spawn",
];

/// How long past a dispatch's own timeout the hub waits for the worker's answer.
const GRACE: Duration = Duration::from_secs(5);

/// Runs [`WORKER_TOOLS`] on the worker attached for the dispatch's workspace.
#[derive(Debug, Clone)]
pub struct WorkerToolHost {
    hub: WorkerHub,
    sessions: BrowserSessions,
}

impl WorkerToolHost {
    /// A host over a hub.
    #[must_use]
    pub fn new(hub: WorkerHub) -> Self {
        Self {
            hub,
            sessions: BrowserSessions::default(),
        }
    }

    /// The hub this host sends through.
    #[must_use]
    pub const fn hub(&self) -> &WorkerHub {
        &self.hub
    }

    /// Run a `browser.*` call on the session of the call's own run.
    async fn browser(&self, mut dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        if !BROWSER_TOOLS.contains(&dispatch.tool.as_str()) {
            return Err(HostFailure::Refused {
                detail: format!(
                    "BROWSER_TOOL_NOT_ROUTED: {} is not served by a worker's browser; nothing was sent",
                    dispatch.tool
                ),
            });
        }
        let Some(target_id) = self
            .hub
            .target_for_workspace(&dispatch.tenant_id, &dispatch.workspace_id)
        else {
            return Err(HostFailure::Refused {
                detail: format!(
                    "WORKER_UNAVAILABLE: no worker is attached for workspace {}; start qworkerd for it",
                    dispatch.workspace_id
                ),
            });
        };
        let session = self
            .sessions
            .for_run(
                self.hub.pool(),
                &dispatch.tenant_id,
                &target_id,
                &dispatch.run_id,
            )
            .await
            .map_err(|detail| HostFailure::Refused {
                detail: format!("BROWSER_SESSION_UNAVAILABLE: {detail}"),
            })?;
        require_agent_control(&session)?;
        // The control plane's word on who holds the session is stated to the worker before each agent call, so a takeover
        // or a handback it missed (it was away, or restarting) cannot leave its driver disagreeing with the database.
        self.hub
            .send_order(
                &dispatch.tenant_id,
                &target_id,
                BrowserOrder::Control {
                    session_id: session.id.clone(),
                    holder: "agent".to_string(),
                    at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                },
            )
            .await;
        // The call acts on its own run's session whatever id the model wrote.
        if let Some(arguments) = dispatch.args.as_object_mut() {
            arguments.insert("session_id".to_string(), json!(session.id));
        }
        let answer = self.send(&dispatch, approved_tier(&dispatch.tool)).await;
        // The call may have launched the session's browser or opened a tab: if anyone is watching, ask for the stream now
        // (the worker follows the newest tab and does nothing if it already shows it).
        if self.hub.has_viewers(&dispatch.tenant_id, &target_id) {
            self.hub
                .send_order(
                    &dispatch.tenant_id,
                    &target_id,
                    BrowserOrder::Screencast {
                        session_id: session.id.clone(),
                        enabled: true,
                        refresh: false,
                    },
                )
                .await;
        }
        match answer {
            Ok((OutcomeWire::Completed { output, .. }, _)) => Ok(settle(&output)),
            other => Self::map(other),
        }
    }

    async fn send(
        &self,
        dispatch: &HostDispatch,
        approved_tier: Option<u8>,
    ) -> Result<(OutcomeWire, bool), HubError> {
        let args_json = dispatch.args.to_string();
        // The envelope's deadline has one-second resolution; round up so a short call is not born expired.
        let deadline_at = instant_after(dispatch.timeout_ms.saturating_add(1000));
        self.hub
            .dispatch(
                &dispatch.tenant_id,
                &dispatch.workspace_id,
                &DispatchArgs {
                    effect_id: &dispatch.effect_id,
                    dispatch_token: &dispatch.dispatch_token,
                    run_id: &dispatch.run_id,
                    step_id: &dispatch.step_id,
                    tool: &dispatch.tool,
                    tool_version: dispatch.version,
                    args_json: &args_json,
                    max_output_bytes: dispatch.max_output_bytes,
                    deadline_at: &deadline_at,
                    wait: Duration::from_millis(dispatch.timeout_ms) + GRACE,
                    approved_tier,
                },
            )
            .await
    }

    fn map(answer: Result<(OutcomeWire, bool), HubError>) -> Result<HostOutcome, HostFailure> {
        match answer {
            Ok((OutcomeWire::Completed { output, .. }, _)) => Ok(HostOutcome::Success {
                output: serde_json::from_str(&output).unwrap_or_else(|_| json!({ "text": output })),
                evidence_ids: Vec::new(),
                remote_ref: None,
            }),
            Ok((OutcomeWire::Failed { detail }, _)) => Ok(HostOutcome::Failure {
                retryable: false,
                error: json!({ "code": "TOOL_FAILED", "detail": detail }),
                evidence_ids: Vec::new(),
            }),
            Ok((OutcomeWire::Cancelled, _)) => Ok(HostOutcome::Failure {
                retryable: false,
                error: json!({ "code": "CANCELLED", "detail": "the worker cancelled the action before it ran" }),
                evidence_ids: Vec::new(),
            }),
            // The worker ran nothing: the lease it holds did not admit the envelope.
            Ok((OutcomeWire::Rejected { reason }, _)) => Err(HostFailure::Refused {
                detail: format!("WORKER_REJECTED: {reason}"),
            }),
            Err(HubError::NoWorker(workspace)) => Err(HostFailure::Refused {
                detail: format!(
                    "WORKER_UNAVAILABLE: no worker is attached for workspace {workspace}; start qworkerd for it"
                ),
            }),
            Err(error @ (HubError::LeaseNotLive(_) | HubError::Envelope(_) | HubError::Send(_))) => {
                Err(HostFailure::Refused {
                    detail: format!("WORKER_NOT_DISPATCHED: {error}"),
                })
            }
            // Sent, and no answer: the effect may have happened. The runtime reconciles; nothing re-sends.
            Err(error @ HubError::NoAnswer(_)) => Ok(HostOutcome::Unknown {
                detail: error.to_string(),
            }),
            Err(error @ HubError::Bind(_)) => Err(HostFailure::Refused {
                detail: error.to_string(),
            }),
        }
    }
}

fn instant_after(millis: u64) -> String {
    let at = chrono::Utc::now()
        + chrono::Duration::milliseconds(i64::try_from(millis).unwrap_or(i64::MAX));
    at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[async_trait]
impl ToolHostPort for WorkerToolHost {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        if dispatch.tool.starts_with("browser.") {
            return self.browser(dispatch).await;
        }
        if !WORKER_TOOLS.contains(&dispatch.tool.as_str()) {
            return Err(HostFailure::Refused {
                detail: format!("{} is not a worker tool", dispatch.tool),
            });
        }
        let answer = self.send(&dispatch, None).await;
        Self::map(answer)
    }
}

/// Sends worker tools to the worker host and every other call to the host it wraps.
#[derive(Clone)]
pub struct WorkerRoutedHost {
    worker: Arc<WorkerToolHost>,
    rest: Arc<dyn ToolHostPort>,
}

impl std::fmt::Debug for WorkerRoutedHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerRoutedHost").finish_non_exhaustive()
    }
}

impl WorkerRoutedHost {
    /// Route [`WORKER_TOOLS`] to `worker`, the rest to `rest`.
    #[must_use]
    pub fn new(worker: Arc<WorkerToolHost>, rest: Arc<dyn ToolHostPort>) -> Self {
        Self { worker, rest }
    }
}

#[async_trait]
impl ToolHostPort for WorkerRoutedHost {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        // Every `browser.*` call goes to the worker host, which serves the tools a worker's browser can and refuses
        // the rest: none of them falls through to the host this one wraps.
        if WORKER_TOOLS.contains(&dispatch.tool.as_str()) || dispatch.tool.starts_with("browser.") {
            self.worker.execute(dispatch).await
        } else {
            self.rest.execute(dispatch).await
        }
    }
}
