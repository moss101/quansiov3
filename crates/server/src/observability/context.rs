//! W3C trace context (`traceparent`) and the span the current task is in.
//!
//! A trace starts where a request enters Quansio — the desktop or web client sends a
//! `traceparent` with each command, and the server starts one when none came — and follows the
//! work across every process boundary: the server holds the current span in a task-local for the
//! whole request future (API → runtime turn → model source → intelligence client), and every call
//! to the intelligence plane carries a child `traceparent` in its gRPC metadata, where the plane
//! continues it. A trace id is correlation, never authority: accepting a client's id grants
//! nothing.

use std::future::Future;

use ring::rand::{SecureRandom, SystemRandom};

tokio::task_local! {
    static CURRENT: TraceContext;
}

/// One span's place in a trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    /// 32 lowercase hex characters, shared by every span of the trace.
    pub trace_id: String,
    /// 16 lowercase hex characters naming this span.
    pub span_id: String,
    /// The span this one continues, when it is not the trace's root here.
    pub parent_span_id: Option<String>,
}

impl TraceContext {
    /// A new trace whose root span is this one.
    #[must_use]
    pub fn root() -> Self {
        Self {
            trace_id: random_hex(16),
            span_id: random_hex(8),
            parent_span_id: None,
        }
    }

    /// Continue an incoming `traceparent` (`00-<trace>-<parent>-<flags>`); `None` when it is not
    /// a valid W3C header, so the caller starts a new trace instead of trusting a malformed one.
    #[must_use]
    pub fn continue_from(traceparent: &str) -> Option<Self> {
        let parts: Vec<&str> = traceparent.trim().split('-').collect();
        let [version, trace_id, parent, flags] = parts.as_slice() else {
            return None;
        };
        let valid = |value: &str, len: usize| {
            value.len() == len
                && value
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                && value.bytes().any(|b| b != b'0')
        };
        if *version != "00" || !valid(trace_id, 32) || !valid(parent, 16) || flags.len() != 2 {
            return None;
        }
        Some(Self {
            trace_id: (*trace_id).to_string(),
            span_id: random_hex(8),
            parent_span_id: Some((*parent).to_string()),
        })
    }

    /// A child span of this one, in the same trace.
    #[must_use]
    pub fn child(&self) -> Self {
        Self {
            trace_id: self.trace_id.clone(),
            span_id: random_hex(8),
            parent_span_id: Some(self.span_id.clone()),
        }
    }

    /// This span as a W3C `traceparent` header value (sampled).
    #[must_use]
    pub fn traceparent(&self) -> String {
        format!("00-{}-{}-01", self.trace_id, self.span_id)
    }

    /// The span the current task is in, when a trace was entered.
    #[must_use]
    pub fn current() -> Option<Self> {
        CURRENT.try_with(Clone::clone).ok()
    }

    /// Run `future` inside this span.
    pub async fn scope<F: Future>(self, future: F) -> F::Output {
        CURRENT.scope(self, future).await
    }
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    // SystemRandom only fails when the OS has no entropy source; a fixed id would be worse
    // than a visible failure, so fall back to the clock only as a last resort.
    if SystemRandom::new().fill(&mut buffer).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for (index, byte) in buffer.iter_mut().enumerate() {
            *byte = (nanos >> ((index % 16) * 8)) as u8;
        }
    }
    if buffer.iter().all(|byte| *byte == 0) {
        buffer[0] = 1;
    }
    hex::encode(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_traceparent_round_trips_and_continues_as_a_child() {
        let root = TraceContext::root();
        assert_eq!(root.trace_id.len(), 32);
        assert_eq!(root.span_id.len(), 16);
        let continued = TraceContext::continue_from(&root.traceparent()).expect("valid");
        assert_eq!(continued.trace_id, root.trace_id);
        assert_eq!(
            continued.parent_span_id.as_deref(),
            Some(root.span_id.as_str())
        );
        assert_ne!(continued.span_id, root.span_id);
        let child = continued.child();
        assert_eq!(
            child.parent_span_id.as_deref(),
            Some(continued.span_id.as_str())
        );
    }

    #[test]
    fn a_malformed_traceparent_is_not_continued() {
        for bad in [
            "",
            "garbage",
            "01-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            "00-0AF7651916CD43DD8448EB211C80319C-b7ad6b7169203331-01",
            "00-00000000000000000000000000000000-b7ad6b7169203331-01",
            "00-0af7651916cd43dd8448eb211c80319c-0000000000000000-01",
            "00-0af7651916cd43dd-b7ad6b7169203331-01",
        ] {
            assert!(TraceContext::continue_from(bad).is_none(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn the_current_span_is_the_scope_the_task_runs_in() {
        assert!(TraceContext::current().is_none());
        let root = TraceContext::root();
        let seen = root.clone().scope(async { TraceContext::current() }).await;
        assert_eq!(seen, Some(root));
    }
}
