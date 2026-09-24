//! OTLP span export (OTLP/HTTP, JSON encoding) to the operator's collector.
//!
//! Configured by `QUANSIO_OTLP_ENDPOINT` (the collector's base URL; spans go to `/v1/traces`).
//! Export never blocks the work it describes: finished spans go onto a bounded queue, a background
//! task batches them, and a full queue drops spans (counted) rather than slowing a turn. Attribute
//! values are redacted exactly as log lines are.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::context::TraceContext;
use super::redact::redact_text;

/// Spans held before the queue drops.
pub const QUEUE_CAPACITY: usize = 4_096;
/// Spans sent per request.
pub const BATCH_SIZE: usize = 256;
/// How long a partial batch waits.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(500);

/// A span ready for export.
#[derive(Debug, Clone)]
pub struct FinishedSpan {
    /// Its place in the trace.
    pub context: TraceContext,
    /// Its name.
    pub name: &'static str,
    /// Start, Unix nanoseconds.
    pub start_unix_nanos: u128,
    /// End, Unix nanoseconds.
    pub end_unix_nanos: u128,
    /// Whether it ended in error.
    pub error: bool,
    /// Its fields.
    pub attributes: Value,
}

/// The exporter handle the logger holds.
#[derive(Debug, Clone)]
pub struct OtlpExporter {
    sender: mpsc::Sender<FinishedSpan>,
    dropped: Arc<AtomicU64>,
}

impl OtlpExporter {
    /// Start exporting to `endpoint` (e.g. `http://127.0.0.1:4318`) as `service_name`.
    ///
    /// Must be called inside a Tokio runtime.
    #[must_use]
    pub fn start(endpoint: &str, service_name: &str) -> Self {
        let (sender, mut receiver) = mpsc::channel::<FinishedSpan>(QUEUE_CAPACITY);
        let url = format!("{}/v1/traces", endpoint.trim_end_matches('/'));
        let service = service_name.to_string();
        let client = reqwest::Client::new();
        tokio::spawn(async move {
            let mut batch = Vec::with_capacity(BATCH_SIZE);
            loop {
                let next = tokio::time::timeout(FLUSH_INTERVAL, receiver.recv()).await;
                match next {
                    Ok(Some(span)) => {
                        batch.push(span);
                        if batch.len() < BATCH_SIZE {
                            continue;
                        }
                    }
                    Ok(None) => {
                        if !batch.is_empty() {
                            send(&client, &url, &service, std::mem::take(&mut batch)).await;
                        }
                        return;
                    }
                    Err(_) => {}
                }
                if !batch.is_empty() {
                    send(&client, &url, &service, std::mem::take(&mut batch)).await;
                }
            }
        });
        Self {
            sender,
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Queue a finished span; a full queue drops it.
    pub fn export(&self, span: FinishedSpan) {
        if self.sender.try_send(span).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Spans dropped because the queue was full.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

async fn send(client: &reqwest::Client, url: &str, service: &str, spans: Vec<FinishedSpan>) {
    let body = request_body(service, &spans);
    // A collector outage is not the run's failure; the spans are also in the logs.
    let _ = client
        .post(url)
        .header("content-type", "application/json")
        .timeout(Duration::from_secs(5))
        .body(body.to_string())
        .send()
        .await;
}

/// The `ExportTraceServiceRequest` in OTLP's JSON encoding.
#[must_use]
pub fn request_body(service: &str, spans: &[FinishedSpan]) -> Value {
    json!({
        "resourceSpans": [{
            "resource": {"attributes": [attribute("service.name", &Value::String(service.to_string()))]},
            "scopeSpans": [{
                "scope": {"name": "quansio"},
                "spans": spans.iter().map(span_json).collect::<Vec<_>>(),
            }],
        }],
    })
}

fn span_json(span: &FinishedSpan) -> Value {
    let attributes: Vec<Value> = match &span.attributes {
        Value::Object(fields) => fields
            .iter()
            .filter(|(key, _)| key.as_str() != "name")
            .map(|(key, value)| attribute(key, value))
            .collect(),
        _ => Vec::new(),
    };
    let mut value = json!({
        "traceId": span.context.trace_id,
        "spanId": span.context.span_id,
        "name": span.name,
        "kind": 1,
        "startTimeUnixNano": span.start_unix_nanos.to_string(),
        "endTimeUnixNano": span.end_unix_nanos.to_string(),
        "attributes": attributes,
        "status": {"code": if span.error { 2 } else { 1 }},
    });
    if let Some(parent) = &span.context.parent_span_id {
        value["parentSpanId"] = Value::String(parent.clone());
    }
    value
}

fn attribute(key: &str, value: &Value) -> Value {
    let encoded = match value {
        Value::Bool(flag) => json!({"boolValue": flag}),
        Value::Number(number) if number.is_i64() || number.is_u64() => {
            json!({"intValue": number.to_string()})
        }
        Value::Number(number) => json!({"doubleValue": number.as_f64().unwrap_or_default()}),
        Value::String(text) => json!({"stringValue": redact_text(text)}),
        other => json!({"stringValue": redact_text(&other.to_string())}),
    };
    json!({"key": key, "value": encoded})
}
