//! The worker's session loop: lease in, dispatch in, result out (EXEC-002).
//!
//! The daemon dials the server and speaks [`quansio_machine::gateway::wire`]. It holds no database and
//! decides nothing: a [`ToWorker::Lease`] says which lease the worker holds, a [`ToWorker::Dispatch`] is
//! validated against that lease by [`WorkerHost::receive`] *before* anything runs, and what the host
//! recorded is what goes back. The host outlives the connection, so a dispatch the server re-sends after a
//! reconnect is answered from the record rather than run twice.
//!
//! Dispatches run side by side (up to the host's concurrency), each in its own task, so a long command does not hold
//! the channel: heartbeats, browser orders and a cancel keep flowing while it runs. "At most once" is the host's rule, not
//! the channel's: a token runs once however it is delivered ([`WorkerHost::receive`]).

use quansio_machine::control::browser::DecisionReceipt;
use quansio_machine::gateway::wire::{self, BrowserOrder, FromWorker, OutcomeWire, ToWorker};
use quansio_machine::gateway::LeaseClaim;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

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
        HostOutcome::Interrupted { .. } => OutcomeWire::Interrupted {
            detail:
                "the action was stopped by a cancel after it started; part of it may have happened"
                    .to_string(),
        },
    }
}

/// Serve one connection until the server closes it.
///
/// While a lease is held the worker reports what it observes every `heartbeat_every`; the server renews the lease
/// on a heartbeat it accepts, so a worker that stops reporting is fenced when the lease lapses.
///
/// The lease the worker holds starts as "none" and is replaced by each [`ToWorker::Lease`] (the server sends it on
/// connect). Each dispatch is validated against the lease held when it arrived and runs in its own task; its answer goes
/// out on this connection if it is still open. A dispatch still running when the connection ends runs to its end and is
/// recorded, so a re-delivery is answered from the record rather than run again.
///
/// # Errors
/// Returns [`SessionError`] when the stream fails or the server sends a frame that is not one.
pub async fn serve<E, S, R, W>(
    host: Arc<WorkerHost<E, S>>,
    tenant_id: &str,
    target_id: &str,
    heartbeat_every: Duration,
    reader: R,
    mut writer: W,
) -> Result<(), SessionError>
where
    E: ToolExecutor + 'static,
    S: EvidenceSink + 'static,
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
    // What the dispatch tasks have to say, written by this loop alone so lines never interleave.
    let (outbound, mut answers) = mpsc::unbounded_channel::<String>();
    let mut lines = BufReader::new(reader).lines();
    let mut next_beat = tokio::time::Instant::now() + heartbeat_every;
    loop {
        let line = tokio::select! {
            line = lines.next_line() => match line? {
                Some(line) => line,
                None => return Ok(()),
            },
            Some(answer) = answers.recv() => {
                writer.write_all(answer.as_bytes()).await?;
                continue;
            }
            frame = host.executor().next_frame() => {
                // Frames flow only while the server asked for them, and only to a worker that still holds its lease.
                if let (Some(frame), true) = (frame, lease.held) {
                    writer
                        .write_all(
                            wire::encode(&FromWorker::BrowserFrame {
                                session_id: frame.session_id,
                                tab_id: frame.tab_id,
                                media_type: frame.media_type.to_string(),
                                data: frame.data,
                                url: frame.url,
                                device_width: frame.device_width,
                                device_height: frame.device_height,
                                sequence: frame.sequence,
                            })?
                            .as_bytes(),
                        )
                        .await?;
                }
                continue;
            }
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
        // A frame this worker cannot read — damaged, or newer than the worker — is skipped and reported, not fatal: ending the
        // connection over one frame would disconnect every worker the day the server learns a new order. A dispatch that is
        // skipped is never answered, which the server settles as unknown and does not retry.
        let frame = match wire::decode::<ToWorker>(&line) {
            Ok(frame) => frame,
            Err(why) => {
                eprintln!("qworkerd: skipped a frame it cannot read: {why}");
                continue;
            }
        };
        match frame {
            ToWorker::Lease(held) => lease = held.into(),
            ToWorker::Dispatch(wire_envelope) => {
                tokio::spawn(dispatch(
                    Arc::clone(&host),
                    *wire_envelope,
                    lease.clone(),
                    outbound.clone(),
                ));
            }
            // A cancel only ever stops work, so a fenced worker honours it too.
            ToWorker::Cancel { dispatch_token } => {
                host.cancel(&dispatch_token);
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
                for line in guard_lines(&host, Some(&session)).await? {
                    writer.write_all(line.as_bytes()).await?;
                }
            }
        }
    }
}

/// Run one dispatch and queue its answer (and, after a browser action, the guard reports) for the connection.
async fn dispatch<E, S>(
    host: Arc<WorkerHost<E, S>>,
    wire_envelope: wire::EnvelopeWire,
    lease: LeaseClaim,
    outbound: mpsc::UnboundedSender<String>,
) where
    E: ToolExecutor + 'static,
    S: EvidenceSink + 'static,
{
    let dispatch_token = wire_envelope.dispatch_token.clone();
    let effect_id = wire_envelope.effect_id.clone();
    let is_browser = wire_envelope.tool.starts_with("browser.");
    let result = match wire_envelope.into_envelope() {
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
    // A closed connection drops the answer; the outcome is recorded for a re-delivery.
    match wire::encode(&result) {
        Ok(line) => {
            let _ = outbound.send(line);
        }
        Err(why) => eprintln!("qworkerd: a result could not be encoded: {why}"),
    }
    // A browser action may have changed what its driver guards (a dialog, a certificate error, a call in doubt): the
    // control plane is told while the action is fresh.
    if is_browser {
        if let Ok(lines) = guard_lines(&host, None).await {
            for line in lines {
                let _ = outbound.send(line);
            }
        }
    }
}

/// The guard report of one session, or of every live one, as lines for the control plane.
async fn guard_lines<E, S>(
    host: &WorkerHost<E, S>,
    only: Option<&str>,
) -> Result<Vec<String>, SessionError>
where
    E: ToolExecutor,
    S: EvidenceSink,
{
    let mut lines = Vec::new();
    for (session_id, report) in host.executor().browser_guards(only).await {
        lines.push(wire::encode(&FromWorker::BrowserGuard {
            session_id,
            report,
        })?);
    }
    Ok(lines)
}
