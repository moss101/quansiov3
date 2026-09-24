//! The deployable: one process running the composed server (APP-001).
//!
//! Logging is installed first (OPS-003): every line on stderr is one redacted JSON object, and
//! spans also go to the operator's collector when `QUANSIO_OTLP_ENDPOINT` is set.

use quansio_server::composition::{Composition, ServerConfig};
use quansio_server::observability::{self, Level, LogConfig, LogSink, OtlpExporter};

#[tokio::main]
async fn main() {
    let otlp = std::env::var("QUANSIO_OTLP_ENDPOINT")
        .ok()
        .filter(|endpoint| !endpoint.is_empty())
        .map(|endpoint| OtlpExporter::start(&endpoint, "quansio-runtime"));
    let exporting = otlp.is_some();
    observability::install(LogConfig {
        service: "runtime",
        sink: LogSink::Stderr,
        ring_capacity: observability::DEFAULT_RING_CAPACITY,
        otlp,
    });
    if exporting {
        observability::notice(
            Level::Info,
            "QUANSIO_OTLP_ENDPOINT is set: spans are exported over OTLP/HTTP",
        );
    }
    let config = match ServerConfig::from_env() {
        Ok(config) => config,
        Err(message) => {
            observability::notice(Level::Error, &message);
            std::process::exit(2);
        }
    };
    let bind = config.bind.clone();
    match Composition::new(config).await {
        Ok(composition) => {
            observability::notice(Level::Info, &format!("listening on {bind}"));
            if let Err(message) = composition.serve().await {
                observability::notice(Level::Error, &message);
                std::process::exit(1);
            }
        }
        Err(message) => {
            observability::notice(Level::Error, &message);
            std::process::exit(1);
        }
    }
}
