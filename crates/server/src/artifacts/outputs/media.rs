//! The supervised `ffmpeg` jobs of an execution target, as a port (ADP-006, DOMAIN.md §10.4 D).
//!
//! A render or an inspection runs as a process on a target, under a supervisor that owns its identity,
//! its cancellation and its restart (`quansio_qworkerd::media`). The server-hosted `video.render` and
//! `video.inspect` Tools hand it a typed plan and read back bytes and a probe; with no port composed, or
//! no `ffmpeg` on the target, they fail `ADAPTER_UNAVAILABLE` and nothing is produced.

use async_trait::async_trait;

use super::ports::PortError;

/// A file staged for a media job.
#[derive(Debug, Clone)]
pub struct MediaFile {
    /// A plain file name inside the job.
    pub name: String,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

/// A render job: the typed plan of a timeline.
#[derive(Debug, Clone)]
pub struct RenderJobSpec {
    /// The plan's identity: the digest of everything that decides what the job does.
    pub job_id: String,
    /// The media the plan cuts from.
    pub inputs: Vec<MediaFile>,
    /// The filter graph and the subtitles, as small text files.
    pub files: Vec<MediaFile>,
    /// The argument vector, after the binary.
    pub argv: Vec<String>,
    /// The file the job writes.
    pub output: String,
    /// How long it may run.
    pub timeout_ms: u64,
    /// The largest output.
    pub max_output_bytes: u64,
}

/// What `ffprobe` said of a rendered file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The container's duration.
    pub duration_ms: u64,
    /// The first video stream's width.
    pub width: u32,
    /// The first video stream's height.
    pub height: u32,
    /// How many video streams.
    pub video_streams: u32,
    /// How many audio streams.
    pub audio_streams: u32,
    /// How many subtitle streams.
    pub subtitle_streams: u32,
    /// The container's format.
    pub format: String,
}

/// A finished render.
#[derive(Debug, Clone)]
pub struct RenderedMedia {
    /// The file's bytes.
    pub bytes: Vec<u8>,
    /// What was found in it.
    pub probe: Probe,
    /// The `ffmpeg` that made it.
    pub tool: String,
}

/// An inspection job.
#[derive(Debug, Clone)]
pub struct InspectJobSpec {
    /// The job's identity.
    pub job_id: String,
    /// The video.
    pub input: MediaFile,
    /// The times to look at.
    pub times_ms: Vec<u64>,
    /// The width of each thumbnail.
    pub thumbnail_width: u32,
    /// Contact sheet columns.
    pub columns: u32,
    /// How long it may run.
    pub timeout_ms: u64,
}

/// One frame of an inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameInfo {
    /// When it was taken.
    pub time_ms: u64,
    /// The SHA-256 of the frame's PNG.
    pub digest: String,
    /// Whether the picture is one flat colour.
    pub uniform: bool,
    /// Its width.
    pub width: u32,
    /// Its height.
    pub height: u32,
}

/// A finished inspection.
#[derive(Debug, Clone)]
pub struct InspectedMedia {
    /// The contact sheet, a PNG.
    pub sheet: Vec<u8>,
    /// Its width.
    pub sheet_width: u32,
    /// Its height.
    pub sheet_height: u32,
    /// The frames, in the order asked.
    pub frames: Vec<FrameInfo>,
    /// The `ffmpeg` that made them.
    pub tool: String,
}

/// The supervised `ffmpeg` jobs of an execution target.
#[async_trait]
pub trait MediaJobPort: Send + Sync {
    /// The `ffmpeg` this port runs, by its version line.
    ///
    /// # Errors
    /// [`PortError::Unavailable`] naming the variable that would make it available.
    async fn toolchain(&self) -> Result<String, PortError>;

    /// Run a render job, or attach to the running or finished job with the same identity.
    ///
    /// # Errors
    /// [`PortError`].
    async fn render(&self, job: RenderJobSpec) -> Result<RenderedMedia, PortError>;

    /// Run an inspection job, or attach to the one with the same identity.
    ///
    /// # Errors
    /// [`PortError`].
    async fn inspect(&self, job: InspectJobSpec) -> Result<InspectedMedia, PortError>;

    /// Stop a job: its process group is killed and a partial output discarded.
    async fn cancel(&self, job_id: &str);
}
