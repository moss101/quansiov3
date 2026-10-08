//! The streamable-HTTP transport and the egress gate it goes through.
//!
//! Every request is a `POST` of one JSON-RPC message to the server's endpoint. The answer is either one JSON
//! body or an event stream that carries the answer (and may carry notifications and requests the server
//! makes of the client, which are refused as on stdio). Before **each** request the destination is resolved
//! and handed to the [`EgressGate`] with every address it resolved to, the request is sent to those vetted
//! addresses only, and redirects are not followed — the rules of webhook delivery (DOMAIN.md §13.5), because
//! an MCP server is an outbound destination like any other and a name that starts resolving somewhere else is
//! refused rather than trusted.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use quansio_machine::egress::{
    Decision, Destination, EgressBroker, EgressGrant, EgressOrigin, EgressRequest, NetworkPolicy,
};
use serde_json::Value;

use super::transport::{
    classify, notification_message, refusal_message, request_message, Incoming, Transport,
    TransportError,
};

/// The protocol revision the client speaks after `initialize`.
pub(crate) const PROTOCOL_HEADER: &str = "mcp-protocol-version";
/// The session header of the streamable-HTTP transport.
const SESSION_HEADER: &str = "mcp-session-id";

/// Decides whether the worker may reach a destination.
///
/// The one question the egress broker answers (EXEC-008): given the URL and **every address it resolved
/// to**, may this target reach it? An answer of `Err(rule)` names the broker's rule and nothing is sent.
#[async_trait]
pub trait EgressGate: Send + Sync {
    /// Whether `url`, resolving to `resolved`, may be reached at `at` (canonical ISO-8601 UTC).
    async fn permit(&self, url: &str, resolved: &[IpAddr], at: &str) -> Result<(), String>;
}

/// The egress broker's own rules over a policy and the grants issued to this target.
///
/// qworkerd holds no database (`crates/qworkerd/tests/host.rs` scans for it), so the runtime gives the worker
/// the target's network policy and its live grants, and the decision is [`EgressBroker::decide`]: the same
/// function the browser, the connector broker and webhook delivery use. Nothing is reachable because it was
/// not forbidden: a destination needs a live grant, and every address it resolved to needs an allowed class.
#[derive(Debug, Clone)]
pub struct BrokerEgressGate {
    tenant_id: String,
    target_id: String,
    target_generation: i64,
    capability_id: Option<String>,
    policy: NetworkPolicy,
    grants: Vec<EgressGrant>,
}

impl BrokerEgressGate {
    /// A gate for one target under one policy and its grants.
    #[must_use]
    pub fn new(
        tenant_id: impl Into<String>,
        target_id: impl Into<String>,
        target_generation: i64,
        policy: NetworkPolicy,
        grants: Vec<EgressGrant>,
    ) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            target_id: target_id.into(),
            target_generation,
            capability_id: None,
            policy,
            grants,
        }
    }
}

#[async_trait]
impl EgressGate for BrokerEgressGate {
    async fn permit(&self, url: &str, resolved: &[IpAddr], at: &str) -> Result<(), String> {
        let request = EgressRequest {
            tenant_id: &self.tenant_id,
            target_id: &self.target_id,
            target_generation: self.target_generation,
            capability_id: self.capability_id.as_deref(),
            origin: EgressOrigin::Tool,
            url,
            resolved_addresses: resolved,
            at,
            headers: &[],
            credential_ref: None,
        };
        match EgressBroker::decide(&request, &self.policy, &self.grants) {
            Ok(Decision::Allow { .. }) => Ok(()),
            Ok(Decision::Deny(denial)) => Err(denial.rule().to_string()),
            Err(denial) => Err(denial.rule().to_string()),
        }
    }
}

/// The gate a worker composes for a target: the broker's rules over what the runtime issued.
#[must_use]
pub fn broker_gate(
    tenant_id: &str,
    target_id: &str,
    target_generation: i64,
    policy: NetworkPolicy,
    grants: Vec<EgressGrant>,
) -> Arc<dyn EgressGate> {
    Arc::new(BrokerEgressGate::new(
        tenant_id,
        target_id,
        target_generation,
        policy,
        grants,
    ))
}

/// The current instant as the canonical `YYYY-MM-DDTHH:MM:SSZ` the egress grants compare against.
pub(crate) fn canonical_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    canonical_instant(i64::try_from(seconds).unwrap_or(0))
}

/// Seconds since the epoch as `YYYY-MM-DDTHH:MM:SSZ` (the civil-from-days algorithm; no time crate).
pub(crate) fn canonical_instant(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        (rest % 3_600) / 60,
        rest % 60
    )
}

/// A streamable-HTTP server.
pub(crate) struct HttpTransport {
    url: String,
    egress: Arc<dyn EgressGate>,
    session: Mutex<Option<String>>,
    protocol: Mutex<Option<String>>,
    next_id: AtomicU64,
    closed: AtomicBool,
    list_changed: AtomicBool,
    max_message_bytes: usize,
}

impl HttpTransport {
    pub(crate) fn new(url: &str, egress: Arc<dyn EgressGate>, max_message_bytes: usize) -> Self {
        Self {
            url: url.to_string(),
            egress,
            session: Mutex::new(None),
            protocol: Mutex::new(None),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            list_changed: AtomicBool::new(false),
            max_message_bytes,
        }
    }

    /// Resolve the destination, ask the egress gate, and return the addresses the request may go to.
    async fn vetted(&self) -> Result<(Destination, Vec<SocketAddr>), TransportError> {
        let destination = Destination::parse(&self.url)
            .map_err(|denial| TransportError::Denied(denial.rule().to_string()))?;
        let resolved: Vec<SocketAddr> =
            tokio::net::lookup_host((destination.host().to_string(), destination.port()))
                .await
                .map_err(|_| TransportError::Unreachable("the host did not resolve".to_string()))?
                .collect();
        let addresses: Vec<IpAddr> = resolved.iter().map(SocketAddr::ip).collect();
        self.egress
            .permit(&self.url, &addresses, &canonical_now())
            .await
            .map_err(TransportError::Denied)?;
        Ok((destination, resolved))
    }

    async fn post(
        &self,
        body: &Value,
        timeout: Duration,
    ) -> Result<reqwest::Response, TransportError> {
        let bytes = serde_json::to_vec(body)
            .map_err(|error| TransportError::Protocol(error.to_string()))?;
        if bytes.len() > self.max_message_bytes {
            return Err(TransportError::TooLarge { sent: false });
        }
        let (destination, pinned) = self.vetted().await?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(timeout)
            .resolve_to_addrs(destination.host(), &pinned)
            .build()
            .map_err(|error| TransportError::Unreachable(error.to_string()))?;
        let mut request = client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(bytes);
        if let Some(version) = self
            .protocol
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            request = request.header(PROTOCOL_HEADER, version);
        }
        if let Some(session) = self
            .session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            request = request.header(SESSION_HEADER, session);
        }
        request.send().await.map_err(|error| {
            if error.is_timeout() {
                TransportError::Timeout { sent: true }
            } else if error.is_connect() {
                TransportError::Unreachable("the connection was refused".to_string())
            } else {
                TransportError::Closed { sent: true }
            }
        })
    }

    /// Read a body up to the bound, a chunk at a time, so a server cannot make the worker buffer without limit.
    async fn bounded_body(
        &self,
        mut response: reqwest::Response,
    ) -> Result<Vec<u8>, TransportError> {
        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            if error.is_timeout() {
                TransportError::Timeout { sent: true }
            } else {
                TransportError::Closed { sent: true }
            }
        })? {
            if body.len() + chunk.len() > self.max_message_bytes {
                return Err(TransportError::TooLarge { sent: true });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// Answer the requests a server made inside a stream: each is refused, best effort.
    async fn refuse(&self, ids: &[Value], timeout: Duration) {
        for id in ids {
            let _ = self.post(&refusal_message(id), timeout).await;
        }
    }
}

/// The `data:` payloads of the events in `text`, one JSON document per event.
pub(crate) fn sse_events(text: &str) -> Vec<String> {
    let mut events = Vec::new();
    let mut data: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if !data.is_empty() {
                events.push(data.join("\n"));
                data.clear();
            }
        } else if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if !data.is_empty() {
        events.push(data.join("\n"));
    }
    events
}

#[async_trait]
impl Transport for HttpTransport {
    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, TransportError> {
        if self.is_closed() {
            return Err(TransportError::Closed { sent: false });
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let response = self
            .post(&request_message(id, method, &params), timeout)
            .await?;
        let status = response.status();
        if let Some(session) = response
            .headers()
            .get(SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
        {
            *self.session.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(session.to_string());
        }
        if status.as_u16() == 404
            && self
                .session
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some()
        {
            // The session is gone: the next conversation starts again from `initialize`.
            self.closed.store(true, Ordering::SeqCst);
            return Err(TransportError::Closed { sent: true });
        }
        if !status.is_success() {
            return Err(TransportError::Protocol(format!(
                "http_{}",
                status.as_u16()
            )));
        }
        let is_stream = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        let body = self.bounded_body(response).await?;
        let text = String::from_utf8(body)
            .map_err(|_| TransportError::Protocol("the response is not UTF-8".to_string()))?;
        let documents: Vec<String> = if is_stream {
            sse_events(&text)
        } else {
            vec![text]
        };
        let mut answer: Option<Result<Value, TransportError>> = None;
        let mut refused: Vec<Value> = Vec::new();
        for document in documents {
            let Ok(message) = serde_json::from_str::<Value>(&document) else {
                continue;
            };
            match classify(&message, 512) {
                Incoming::Response {
                    id: answered,
                    outcome,
                } if answered == id => {
                    answer = Some(outcome);
                }
                Incoming::ServerRequest { id } => refused.push(id),
                Incoming::Notification { list_changed: true } => {
                    self.list_changed.store(true, Ordering::SeqCst);
                }
                _ => {}
            }
        }
        self.refuse(&refused, timeout).await;
        answer.unwrap_or_else(|| {
            Err(TransportError::Protocol(
                "the response does not answer the request".to_string(),
            ))
        })
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), TransportError> {
        let response = self
            .post(
                &notification_message(method, &params),
                Duration::from_secs(10),
            )
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(TransportError::Protocol(format!(
                "http_{}",
                response.status().as_u16()
            )))
        }
    }

    fn take_list_changed(&self) -> bool {
        self.list_changed.swap(false, Ordering::SeqCst)
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn set_protocol(&self, version: &str) {
        *self.protocol.lock().unwrap_or_else(PoisonError::into_inner) = Some(version.to_string());
    }

    async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instants_are_canonical() {
        assert_eq!(canonical_instant(0), "1970-01-01T00:00:00Z");
        assert_eq!(canonical_instant(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(canonical_instant(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(canonical_instant(4_102_444_799), "2099-12-31T23:59:59Z");
    }

    #[test]
    fn an_event_stream_is_split_into_its_data_documents() {
        let text =
            "event: message\ndata: {\"a\":1}\n\n: comment\ndata: one\ndata: two\n\ndata: tail";
        assert_eq!(
            sse_events(text),
            vec![
                "{\"a\":1}".to_string(),
                "one\ntwo".to_string(),
                "tail".to_string()
            ]
        );
        assert!(sse_events("event: ping\n\n").is_empty());
    }
}
