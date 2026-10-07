//! Output generators (ADP-006, DOMAIN.md §10.4): decks, printed documents, diagrams, video,
//! transcripts and the web-dev preview.
//!
//! Canonical owner: `crates/server` artifacts module. The intelligence plane builds the bytes
//! (`python/intelligence/artifacts`); this module is where the authority **verifies** them and
//! commits a version with a quality record it wrote itself. Worker-side operations (a browser
//! layout, `ffmpeg`) are ports ([`ports`]); the deployable composes none it does not have, and a
//! Tool whose port is missing fails `ADAPTER_UNAVAILABLE`.

pub mod config;
pub mod deck;
mod deck_host;
mod docs_host;
pub mod document;
mod host;
pub mod html;
pub mod media;
pub mod ooxml;
pub mod pdf;
pub mod ports;
pub mod quality;
pub mod shapes;
pub mod support;
pub mod svg;
pub mod tool_owned;
pub mod transcript;
pub mod video;
mod video_host;
pub mod xml;

pub use host::{is_output_tool, OutputToolHost, TOOLS};
