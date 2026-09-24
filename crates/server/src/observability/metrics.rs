//! Process metrics in the Prometheus text exposition format (0.0.4).
//!
//! Counters and latency histograms keyed by metric name and label set. Label values are redacted
//! and escaped before they are stored, so a value that came from a request cannot smuggle a
//! secret or break the exposition. Names are the code's, never a caller's.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock, PoisonError};

use super::redact::redact_text;

/// Latency buckets, seconds.
pub const BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

type Labels = Vec<(String, String)>;

#[derive(Default)]
struct Histogram {
    counts: [u64; BUCKETS.len()],
    sum: f64,
    count: u64,
}

#[derive(Default)]
struct Registry {
    help: BTreeMap<&'static str, &'static str>,
    counters: BTreeMap<(&'static str, Labels), u64>,
    histograms: BTreeMap<(&'static str, Labels), Histogram>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

fn labels(pairs: &[(&str, &str)]) -> Labels {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), escape(&redact_text(value))))
        .collect()
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Add one to a counter.
pub fn increment(name: &'static str, help: &'static str, pairs: &[(&str, &str)]) {
    let mut registry = registry().lock().unwrap_or_else(PoisonError::into_inner);
    registry.help.insert(name, help);
    *registry.counters.entry((name, labels(pairs))).or_default() += 1;
}

/// Record one observation (seconds) in a histogram.
pub fn observe(name: &'static str, help: &'static str, pairs: &[(&str, &str)], seconds: f64) {
    let mut registry = registry().lock().unwrap_or_else(PoisonError::into_inner);
    registry.help.insert(name, help);
    let histogram = registry
        .histograms
        .entry((name, labels(pairs)))
        .or_default();
    for (index, bound) in BUCKETS.iter().enumerate() {
        if seconds <= *bound {
            histogram.counts[index] += 1;
        }
    }
    histogram.sum += seconds;
    histogram.count += 1;
}

fn render_labels(labels: &Labels, extra: Option<(&str, &str)>) -> String {
    let mut parts: Vec<String> = labels
        .iter()
        .map(|(key, value)| format!("{key}=\"{value}\""))
        .collect();
    if let Some((key, value)) = extra {
        parts.push(format!("{key}=\"{value}\""));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{{{}}}", parts.join(","))
    }
}

/// Every metric, as Prometheus text.
#[must_use]
pub fn prometheus_text() -> String {
    let registry = registry().lock().unwrap_or_else(PoisonError::into_inner);
    let mut out = String::new();
    let mut described = std::collections::BTreeSet::new();
    for ((name, labels), value) in &registry.counters {
        if described.insert(*name) {
            let _ = writeln!(
                out,
                "# HELP {name} {}",
                registry.help.get(name).unwrap_or(&"")
            );
            let _ = writeln!(out, "# TYPE {name} counter");
        }
        let _ = writeln!(out, "{name}{} {value}", render_labels(labels, None));
    }
    for ((name, labels), histogram) in &registry.histograms {
        if described.insert(*name) {
            let _ = writeln!(
                out,
                "# HELP {name} {}",
                registry.help.get(name).unwrap_or(&"")
            );
            let _ = writeln!(out, "# TYPE {name} histogram");
        }
        for (index, bound) in BUCKETS.iter().enumerate() {
            let _ = writeln!(
                out,
                "{name}_bucket{} {}",
                render_labels(labels, Some(("le", &bound.to_string()))),
                histogram.counts[index]
            );
        }
        let _ = writeln!(
            out,
            "{name}_bucket{} {}",
            render_labels(labels, Some(("le", "+Inf"))),
            histogram.count
        );
        let _ = writeln!(
            out,
            "{name}_sum{} {}",
            render_labels(labels, None),
            histogram.sum
        );
        let _ = writeln!(
            out,
            "{name}_count{} {}",
            render_labels(labels, None),
            histogram.count
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_and_histograms_render_as_prometheus_text() {
        increment("quansio_test_things_total", "Things.", &[("kind", "a\"b")]);
        increment("quansio_test_things_total", "Things.", &[("kind", "a\"b")]);
        observe(
            "quansio_test_latency_seconds",
            "Latency.",
            &[("route", "/x")],
            0.2,
        );
        let text = prometheus_text();
        assert!(
            text.contains("# TYPE quansio_test_things_total counter"),
            "{text}"
        );
        assert!(
            text.contains("quansio_test_things_total{kind=\"a\\\"b\"} 2"),
            "{text}"
        );
        assert!(
            text.contains("quansio_test_latency_seconds_bucket{route=\"/x\",le=\"0.25\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("quansio_test_latency_seconds_bucket{route=\"/x\",le=\"0.1\"} 0"),
            "{text}"
        );
        assert!(
            text.contains("quansio_test_latency_seconds_count{route=\"/x\"} 1"),
            "{text}"
        );
    }

    #[test]
    fn a_label_value_cannot_carry_a_secret() {
        increment(
            "quansio_test_secret_total",
            "Secrets.",
            &[("who", "Bearer abc123")],
        );
        assert!(!prometheus_text().contains("abc123"));
    }
}
