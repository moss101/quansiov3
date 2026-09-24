//! Structured JSON logs and spans, redacted before they are written.
//!
//! One line per event: `ts`, `level`, `service`, `event`, the trace ids of the span the event
//! happened in, and the event's fields. The whole line passes [`redact_text`] before it reaches the
//! sink, so a secret canary or a credential-shaped value cannot be written whatever field it rode
//! in on. Message and tool content are never logged by the callers; this is the backstop.
//!
//! Lines also go to a bounded in-memory ring, tagged with their tenant, which is what a tenant's
//! diagnostic bundle is read from (no store), and finished spans go to the OTLP exporter when one
//! is configured.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use super::context::TraceContext;
use super::otlp::{FinishedSpan, OtlpExporter};
use super::redact::redact_text;

/// Lines kept in memory for diagnostic bundles.
pub const DEFAULT_RING_CAPACITY: usize = 2_000;

/// Where lines are written.
#[derive(Debug, Clone)]
pub enum LogSink {
    /// Standard error, one JSON object per line.
    Stderr,
    /// An in-memory buffer (tests, embedding).
    Memory(Arc<Mutex<Vec<String>>>),
}

/// How the process logs.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// The plane this process is (`runtime`).
    pub service: &'static str,
    /// Where lines go.
    pub sink: LogSink,
    /// Ring size for diagnostic bundles.
    pub ring_capacity: usize,
    /// Span export, when an OTLP endpoint is configured.
    pub otlp: Option<OtlpExporter>,
}

struct Logger {
    config: LogConfig,
    ring: Mutex<VecDeque<(Option<String>, String)>>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Install the process logger; `false` when one is already installed (the first one stays).
pub fn install(config: LogConfig) -> bool {
    LOGGER
        .set(Logger {
            ring: Mutex::new(VecDeque::with_capacity(config.ring_capacity.min(4_096))),
            config,
        })
        .is_ok()
}

/// Log levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Routine.
    Info,
    /// Unexpected but handled.
    Warn,
    /// A failure.
    Error,
}

impl Level {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// An operator notice (startup configuration, a background pass failing): a JSON event when the
/// logger is installed, a plain `quansio-server:` line on stderr before it is.
pub fn notice(level: Level, message: &str) {
    if LOGGER.get().is_some() {
        event(level, "server.notice", json!({ "detail": message }));
    } else {
        eprintln!("quansio-server: {}", redact_text(message));
    }
}

/// Emit one event in the current span (if any).
pub fn event(level: Level, name: &str, fields: Value) {
    emit(level, name, TraceContext::current().as_ref(), fields);
}

fn emit(level: Level, name: &str, context: Option<&TraceContext>, fields: Value) {
    let Some(logger) = LOGGER.get() else {
        return;
    };
    let mut line = Map::new();
    line.insert("ts".to_string(), Value::String(rfc3339_now()));
    line.insert(
        "level".to_string(),
        Value::String(level.as_str().to_string()),
    );
    line.insert(
        "service".to_string(),
        Value::String(logger.config.service.to_string()),
    );
    line.insert("event".to_string(), Value::String(name.to_string()));
    if let Some(context) = context {
        line.insert(
            "trace_id".to_string(),
            Value::String(context.trace_id.clone()),
        );
        line.insert(
            "span_id".to_string(),
            Value::String(context.span_id.clone()),
        );
        if let Some(parent) = &context.parent_span_id {
            line.insert("parent_span_id".to_string(), Value::String(parent.clone()));
        }
    }
    if let Value::Object(fields) = fields {
        for (key, value) in fields {
            line.entry(key).or_insert(value);
        }
    }
    let tenant = line
        .get("tenant_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let text = redact_text(&Value::Object(line).to_string());
    match &logger.config.sink {
        LogSink::Stderr => {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "{text}");
        }
        LogSink::Memory(buffer) => buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text.clone()),
    }
    let mut ring = logger.ring.lock().unwrap_or_else(PoisonError::into_inner);
    if ring.len() >= logger.config.ring_capacity.max(1) {
        ring.pop_front();
    }
    ring.push_back((tenant, text));
}

/// The ring's lines for `tenant_id`, oldest first.
#[must_use]
pub fn recent_for_tenant(tenant_id: &str) -> Vec<String> {
    let Some(logger) = LOGGER.get() else {
        return Vec::new();
    };
    logger
        .ring
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|(tenant, _)| tenant.as_deref() == Some(tenant_id))
        .map(|(_, line)| line.clone())
        .collect()
}

/// A timed span: a child of the current span (or a new trace), logged when it ends.
#[derive(Debug)]
pub struct Span {
    context: TraceContext,
    name: &'static str,
    fields: Value,
    started: Instant,
    started_unix_nanos: u128,
}

impl Span {
    /// Open a span under the current one.
    #[must_use]
    pub fn start(name: &'static str, fields: Value) -> Self {
        let context =
            TraceContext::current().map_or_else(TraceContext::root, |current| current.child());
        Self::with_context(context, name, fields)
    }

    /// Open a span with an explicit context (a request entering the process).
    #[must_use]
    pub fn with_context(context: TraceContext, name: &'static str, fields: Value) -> Self {
        Self {
            context,
            name,
            fields,
            started: Instant::now(),
            started_unix_nanos: unix_nanos(),
        }
    }

    /// This span's trace context (what a child or an outgoing call continues).
    #[must_use]
    pub const fn context(&self) -> &TraceContext {
        &self.context
    }

    /// End the span: one `span` event with its duration, outcome and fields.
    pub fn end(self, outcome: &str, extra: Value) {
        let duration = self.started.elapsed();
        let mut fields = match self.fields {
            Value::Object(fields) => fields,
            _ => Map::new(),
        };
        if let Value::Object(extra) = extra {
            fields.extend(extra);
        }
        fields.insert("name".to_string(), Value::String(self.name.to_string()));
        fields.insert("outcome".to_string(), Value::String(outcome.to_string()));
        fields.insert(
            "duration_ms".to_string(),
            json!(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)),
        );
        let level = if outcome == "error" {
            Level::Warn
        } else {
            Level::Info
        };
        let fields = Value::Object(fields);
        emit(level, "span", Some(&self.context), fields.clone());
        if let Some(exporter) = LOGGER.get().and_then(|logger| logger.config.otlp.as_ref()) {
            exporter.export(FinishedSpan {
                context: self.context,
                name: self.name,
                start_unix_nanos: self.started_unix_nanos,
                end_unix_nanos: self.started_unix_nanos + duration.as_nanos(),
                error: outcome == "error",
                attributes: fields,
            });
        }
    }
}

fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn rfc3339_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}
