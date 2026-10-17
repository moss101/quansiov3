//! The worker's session loop: lease in, dispatch in, result out (EXEC-002).
//!
//! The daemon dials the server and speaks [`quansio_machine::gateway::wire`]. It holds no database and
//! decides nothing: a [`ToWorker::Lease`] says which lease the worker holds, a [`ToWorker::Dispatch`] is
//! validated against that lease by [`WorkerHost::receive`] *before* anything runs, and what the host
//! recorded is what goes back. The host outlives the connection, so a dispatch the server re-sends after a
//! reconnect is answered from the record rather than run twice.
//!
//! Dispatches are served one at a time: a long command holds the channel until it ends. That is the
//! conservative reading of "at most once"; concurrent dispatch is a later change, not an assumption here.

use quansio_machine::control::browser::DecisionReceipt;
use quansio_machine::gateway::wire::{self, BrowserOrder, FromWorker, OutcomeWire, ToWorker};
use quansio_machine::gateway::LeaseClaim;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::host::{EvidenceSink, HostFailure, HostOutcome, ToolExecutor, WorkerHost};

/// An evidence sink for a worker with nowhere to put evidence: any upload fails closed.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnattachedEvidence;

#[async_trait::async_trait]
impl EvidenceSink for UnattachedEvidence {
    async fn upload(&self, _: &str, _: &[u8], _: &str) -> Result<String, HostFailure> {
        Err(HostFailure::EvidenceFailed(
            "no evidence store is attached to this worker".to_string(),
        ))
    }
}

/// Why a session ended.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The stream failed.
    #[error("the control channel failed: {0}")]
    Io(#[from] std::io::Error),
    /// A frame could not be written.
    #[error("a frame could not be encoded: {0}")]
    Encode(#[from] serde_json::Error),
    /// The server sent something that is not a frame.
    #[error("the server sent a malformed frame: {0}")]
    Malformed(String),
}

/// The canonical instant the gateway compares against (`YYYY-MM-DDTHH:MM:SSZ`).
#[must_use]
pub fn now_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn outcome_wire(outcome: HostOutcome) -> OutcomeWire {
    match outcome {
        HostOutcome::Completed {
            output,
            truncated,
            evidence_ref,
            ..
        } => OutcomeWire::Completed {
            output: String::from_utf8_lossy(&output).into_owned(),
            truncated,
            evidence_ref,
        },
        HostOutcome::Failed { failure, .. } => OutcomeWire::Failed {
            detail: failure.to_string(),
        },
        HostOutcome::Cancelled { .. } => OutcomeWire::Cancelled,
    }
}

/// Serve one connection until the server closes it.
///
/// While a lease is held the worker reports what it observes every `heartbeat_every`; the server renews the lease
/// on a heartbeat it accepts, so a worker that stops reporting is fenced when the lease lapses.
///
/// `lease` is the lease the worker currently holds; it starts as "none" and is replaced by each
/// [`ToWorker::Lease`], and survives a reconnect only if the caller keeps it (the server re-sends it on
/// connect, so the caller need not).
///
/// # Errors
/// Returns [`SessionError`] when the stream fails or the server sends a frame that is not one.
pub async fn serve<E, S, R, W>(
    host: &mut WorkerHost<E, S>,
    tenant_id: &str,
    target_id: &str,
    heartbeat_every: Duration,
    reader: R,
    mut writer: W,
) -> Result<(), SessionError>
where
    E: ToolExecutor,
    S: EvidenceSink,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    writer
        .write_all(
            wire::encode(&FromWorker::Hello {
                tenant_id: tenant_id.to_string(),
                target_id: target_id.to_string(),
            })?
            .as_bytes(),
        )
        .await?;

    let mut lease = LeaseClaim {
        lease_id: String::new(),
        target_id: target_id.to_string(),
        generation: 1,
        held: false,
        expires_at: String::new(),
    };
    let mut lines = BufReader::new(reader).lines();
    let mut next_beat = tokio::time::Instant::now() + heartbeat_every;
    loop {
        let line = tokio::select! {
            line = lines.next_line() => match line? {
                Some(line) => line,
                None => return Ok(()),
            },
            () = tokio::time::sleep_until(next_beat) => {
                next_beat += heartbeat_every;
                if lease.held {
                    let beat = WorkerHost::<E, S>::heartbeat(&lease, "READY", &now_utc());
                    writer
                        .write_all(
                            wire::encode(&FromWorker::Heartbeat {
                                target_id: beat.target_id,
                                generation: beat.generation,
                                observed_state: beat.observed_state,
                                at: beat.at,
                            })?
                            .as_bytes(),
                        )
                        .await?;
                }
                continue;
            }
        };
        match wire::decode::<ToWorker>(&line).map_err(SessionError::Malformed)? {
            ToWorker::Lease(held) => lease = held.into(),
            ToWorker::Dispatch(wire_envelope) => {
                let dispatch_token = wire_envelope.dispatch_token.clone();
                let effect_id = wire_envelope.effect_id.clone();
                let is_browser = wire_envelope.tool.starts_with("browser.");
                let result = match (*wire_envelope).into_envelope() {
                    Err(kind) => FromWorker::Result {
                        dispatch_token,
                        effect_id,
                        replayed: false,
                        outcome: OutcomeWire::Rejected {
                            reason: format!("unknown envelope kind: {kind}"),
                        },
                    },
                    Ok(envelope) => match host.receive(&envelope, &lease, &now_utc()).await {
                        Ok(recorded) => FromWorker::Result {
                            dispatch_token,
                            effect_id,
                            replayed: recorded.replayed,
                            outcome: outcome_wire(recorded.outcome),
                        },
                        Err(rejection) => FromWorker::Result {
                            dispatch_token,
                            effect_id,
                            replayed: false,
                            outcome: OutcomeWire::Rejected {
                                reason: rejection.to_string(),
                            },
                        },
                    },
                };
                writer.write_all(wire::encode(&result)?.as_bytes()).await?;
                // A browser action may have changed what its driver guards (a dialog, a certificate error, a call in
                // doubt): the control plane is told while the action is fresh.
                if is_browser {
                    report_guards(host, None, &mut writer).await?;
                }
            }
            ToWorker::Browser(order) => {
                // A worker that does not hold a lease takes no orders: it has been fenced.
                if !lease.held {
                    continue;
                }
                let session = order.session_id().to_string();
                let applied = match &order {
                    // The one place a receipt is rebuilt from the wire: the control channel's reader.
                    BrowserOrder::Certificate { receipt }
                    | BrowserOrder::Permission { receipt } => {
                        match DecisionReceipt::from_control_channel(receipt) {
                            Ok(receipt) => host.executor().browser_decision(&receipt).await,
                            Err(why) => Err(why),
                        }
                    }
                    other => host.executor().browser_order(other).await,
                };
                if let Err(why) = applied {
                    eprintln!("qworkerd: a browser order for {session} was not applied: {why}");
                }
                report_guards(host, Some(&session), &mut writer).await?;
            }
        }
    }
}

/// Send the control plane the guard report of one session, or of every live one.
async fn report_guards<E, S, W>(
    host: &WorkerHost<E, S>,
    only: Option<&str>,
    writer: &mut W,
) -> Result<(), SessionError>
where
    E: ToolExecutor,
    S: EvidenceSink,
    W: AsyncWrite + Unpin,
{
    for (session_id, report) in host.executor().browser_guards(only).await {
        writer
            .write_all(wire::encode(&FromWorker::BrowserGuard { session_id, report })?.as_bytes())
            .await?;
    }
    Ok(())
}
