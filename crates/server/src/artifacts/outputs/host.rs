//! The server-hosted output tools (ADP-006, DOMAIN.md §7.5 and §10.4): `deck.render`, `deck.export`,
//! `deck.deliver`, `diagram.render`, `document.export_pdf`, `video.edit`, `video.render`,
//! `video.inspect`, `audio.transcribe` and the `webdev.preview_*` tools.
//!
//! Each one verifies what it is handed before it commits a version with a quality record it wrote
//! itself. A worker-side operation (a browser layout, `ffmpeg`, a transcription route) is a port;
//! with no port composed the tool fails `ADAPTER_UNAVAILABLE`.

use std::sync::Arc;

use async_trait::async_trait;

use super::config::OutputsConfig;
use super::deck_host;
use super::ports::RenderPort;
use super::support::{Ctx, Env};
use crate::artifacts::ObjectStore;
use crate::runtime::turn_loop::{HostDispatch, HostFailure, HostOutcome, ToolHostPort};
use sqlx::PgPool;

/// The tools this host executes.
pub const TOOLS: &[&str] = &[
    "deck.render",
    "deck.export",
    "deck.deliver",
    "diagram.render",
    "document.export_pdf",
    "video.edit",
    "video.render",
    "video.inspect",
    "audio.transcribe",
    "webdev.preview_open",
    "webdev.preview_renew",
    "webdev.preview_close",
    "webdev.preview_suggestions",
];

/// Whether a tool is one of this host's.
#[must_use]
pub fn is_output_tool(name: &str) -> bool {
    TOOLS.contains(&name)
}

/// Executes the output tools.
#[derive(Clone)]
pub struct OutputToolHost {
    env: Env,
}

impl std::fmt::Debug for OutputToolHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputToolHost").finish_non_exhaustive()
    }
}

impl OutputToolHost {
    /// A host over the database and the object store, with the shipped configuration and no worker
    /// ports: every tool that needs one fails closed until it is composed.
    #[must_use]
    pub fn new(pool: PgPool, objects: Arc<dyn ObjectStore>) -> Self {
        Self {
            env: Env {
                pool,
                objects,
                config: Arc::new(OutputsConfig::shipped()),
                render: None,
            },
        }
    }

    /// Use another configuration.
    #[must_use]
    pub fn with_config(mut self, config: OutputsConfig) -> Self {
        self.env.config = Arc::new(config);
        self
    }

    /// Compose the worker that lays pages out in the render sandbox.
    #[must_use]
    pub fn with_render(mut self, port: Arc<dyn RenderPort>) -> Self {
        self.env.render = Some(port);
        self
    }
}

#[async_trait]
impl ToolHostPort for OutputToolHost {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        let ctx = Ctx::new(&self.env, &dispatch);
        match dispatch.tool.as_str() {
            "deck.render" => deck_host::render(&ctx).await,
            "deck.export" => deck_host::export(&ctx).await,
            "deck.deliver" => deck_host::deliver(&ctx).await,
            other if is_output_tool(other) => Err(HostFailure::Unavailable {
                owner: crate::runtime::turn_loop::dispatch::OUTPUT_HOST_OWNER,
                detail: format!("{other} is not wired in this build yet"),
            }),
            other => Err(HostFailure::Refused {
                detail: format!("{other} is not an output tool"),
            }),
        }
    }
}
