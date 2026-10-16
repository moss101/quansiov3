//! The tool host that forwards worker tools to the attached worker.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quansio_machine::gateway::wire::OutcomeWire;
use serde_json::json;

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
}

impl WorkerToolHost {
    /// A host over a hub.
    #[must_use]
    pub const fn new(hub: WorkerHub) -> Self {
        Self { hub }
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
        if !WORKER_TOOLS.contains(&dispatch.tool.as_str()) {
            return Err(HostFailure::Refused {
                detail: format!("{} is not a worker tool", dispatch.tool),
            });
        }
        let args_json = dispatch.args.to_string();
        // The envelope's deadline has one-second resolution; round up so a short call is not born expired.
        let deadline_at = instant_after(dispatch.timeout_ms.saturating_add(1000));
        let answer = self
            .hub
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
                },
            )
            .await;
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
        if WORKER_TOOLS.contains(&dispatch.tool.as_str()) {
            self.worker.execute(dispatch).await
        } else {
            self.rest.execute(dispatch).await
        }
    }
}
