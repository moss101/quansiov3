//! What the worker does with the control plane's browser orders (APP-007 over EXEC-009): it takes them only while it holds
//! its lease, rebuilds a decision receipt from the wire in exactly one place and refuses a malformed one, and tells the
//! control plane what its driver reports after each order. The executor here only records, because what is under test is
//! the daemon's handling and not Chrome (that is `browser_worker.rs` and the server's `worker_live.rs`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use quansio_machine::control::browser::{DecisionKind, DecisionReceipt, ReceiptWire};
use quansio_machine::gateway::wire::{self, BrowserOrder, FromWorker, LeaseWire, ToWorker};
use quansio_machine::gateway::ActionEnvelope;
use quansio_qworkerd::daemon::{serve, UnattachedEvidence};
use quansio_qworkerd::host::{HostFailure, OutputSink, ToolExecutor, ToolOutput, WorkerHost};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9YORD001";
const SESSION: &str = "bsn_01J8Z3K6F1N8VQ2X5W9YORD001";

#[derive(Default)]
struct Recorded {
    orders: Vec<String>,
    decisions: Vec<(DecisionKind, String)>,
}

struct Recording(Arc<Mutex<Recorded>>);

#[async_trait::async_trait]
impl ToolExecutor for Recording {
    async fn run(
        &self,
        envelope: &ActionEnvelope,
        _: &mut OutputSink,
    ) -> Result<ToolOutput, HostFailure> {
        Err(HostFailure::ToolFailed {
            tool: envelope.tool.clone(),
            detail: "not under test".to_string(),
        })
    }

    async fn browser_order(&self, order: &BrowserOrder) -> Result<(), String> {
        self.0.lock().unwrap().orders.push(format!("{order:?}"));
        Ok(())
    }

    async fn browser_decision(&self, receipt: &DecisionReceipt) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .decisions
            .push((receipt.kind(), receipt.subject().to_string()));
        Ok(())
    }

    async fn browser_guards(&self, only: Option<&str>) -> Vec<(String, Value)> {
        vec![(
            only.unwrap_or(SESSION).to_string(),
            json!({"tabs": [], "in_doubt": []}),
        )]
    }
}

fn receipt(kind: &str, decision: &str) -> ReceiptWire {
    ReceiptWire {
        session_id: SESSION.to_string(),
        tab_id: "T1".to_string(),
        kind: kind.to_string(),
        subject: "crt_1".to_string(),
        decision: decision.to_string(),
        decided_by: "usr_x".to_string(),
        decided_at: "2026-10-17T00:00:00Z".to_string(),
    }
}

fn control(holder: &str) -> ToWorker {
    ToWorker::Browser(BrowserOrder::Control {
        session_id: SESSION.to_string(),
        holder: holder.to_string(),
        at: "2026-10-17T00:00:00Z".to_string(),
    })
}

#[tokio::test]
async fn a_worker_takes_browser_orders_only_while_it_holds_a_lease_and_rebuilds_a_receipt_only_from_a_valid_wire_form(
) {
    let recorded = Arc::new(Mutex::new(Recorded::default()));
    let (server_end, worker_end) = tokio::io::duplex(1 << 16);
    let (worker_read, worker_write) = tokio::io::split(worker_end);
    let shared = recorded.clone();
    let worker = tokio::spawn(async move {
        let host = std::sync::Arc::new(WorkerHost::new(Recording(shared), UnattachedEvidence));
        let _ = serve(
            host,
            "tnt_x",
            TARGET,
            Duration::from_secs(3600),
            worker_read,
            worker_write,
        )
        .await;
    });
    let (server_read, mut server_write) = tokio::io::split(server_end);
    let mut lines = BufReader::new(server_read).lines();
    let hello: FromWorker = wire::decode(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert!(matches!(hello, FromWorker::Hello { .. }));

    let send = |frame: ToWorker| std::future::ready(wire::encode(&frame).unwrap());
    // No lease yet: nothing is applied and nothing is reported.
    for frame in [
        control("user"),
        ToWorker::Browser(BrowserOrder::Certificate {
            receipt: receipt("certificate", "allow"),
        }),
    ] {
        server_write
            .write_all(send(frame).await.as_bytes())
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        recorded.lock().unwrap().orders.is_empty() && recorded.lock().unwrap().decisions.is_empty(),
        "a worker without a lease took an order"
    );

    // A lease; now an order is applied and the driver's state is reported.
    let lease = ToWorker::Lease(LeaseWire {
        lease_id: "lse_x".into(),
        target_id: TARGET.into(),
        generation: 1,
        expires_at: "2999-01-01T00:00:00Z".into(),
    });
    server_write
        .write_all(send(lease).await.as_bytes())
        .await
        .unwrap();
    server_write
        .write_all(send(control("user")).await.as_bytes())
        .await
        .unwrap();
    let reported: FromWorker = wire::decode(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert!(
        matches!(reported, FromWorker::BrowserGuard { ref session_id, .. } if session_id == SESSION),
        "{reported:?}"
    );
    assert_eq!(recorded.lock().unwrap().orders.len(), 1);

    // A decision is rebuilt from a valid wire form...
    let valid = ToWorker::Browser(BrowserOrder::Certificate {
        receipt: receipt("certificate", "allow"),
    });
    server_write
        .write_all(send(valid).await.as_bytes())
        .await
        .unwrap();
    let _ = lines.next_line().await.unwrap().unwrap();
    assert_eq!(
        recorded.lock().unwrap().decisions,
        vec![(DecisionKind::Certificate, "crt_1".to_string())]
    );

    // ...and never from a malformed one: an unknown kind, an unknown decision, a blank identity.
    for bad in [
        receipt("root", "allow"),
        receipt("certificate", "maybe"),
        ReceiptWire {
            decided_by: String::new(),
            ..receipt("permission", "deny")
        },
    ] {
        server_write
            .write_all(
                send(ToWorker::Browser(BrowserOrder::Permission { receipt: bad }))
                    .await
                    .as_bytes(),
            )
            .await
            .unwrap();
        let _ = lines.next_line().await.unwrap().unwrap();
    }
    assert_eq!(
        recorded.lock().unwrap().decisions.len(),
        1,
        "a malformed receipt was applied"
    );

    // A frame the worker cannot read does not end the connection: it is skipped, and the next order is still applied.
    for garbage in [
        "not json at all\n",
        "{\"type\":\"browser\",\"order\":\"user_telepathy\"}\n",
        "{\"type\":\"lease\"}\n",
    ] {
        server_write.write_all(garbage.as_bytes()).await.unwrap();
    }
    let applied_before = recorded.lock().unwrap().orders.len();
    server_write
        .write_all(send(control("agent")).await.as_bytes())
        .await
        .unwrap();
    let reported: FromWorker = wire::decode(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert!(
        matches!(reported, FromWorker::BrowserGuard { .. }),
        "the worker stopped answering after a bad frame: {reported:?}"
    );
    assert_eq!(recorded.lock().unwrap().orders.len(), applied_before + 1);
    assert!(
        !worker.is_finished(),
        "the session ended over a frame it could not read"
    );

    drop(server_write);
    worker.abort();
}
