//! The transcription port (ADP-006, DOMAIN.md §10.4 E).
//!
//! Speech-to-text is a model call, and model calls belong to the intelligence plane's gateway: it
//! resolves the route from `config/models.yaml`, runs the data-policy guard before a byte is built,
//! and holds the provider credential. The authority never sees the credential and never calls a
//! provider. It hands the audio to this port and gets back one of three things: a job that is still
//! open, a finished transcript document (which the authority re-validates and commits itself), or a
//! failure. With no port composed, `audio.transcribe` fails `ADAPTER_UNAVAILABLE`.

use async_trait::async_trait;

use super::ports::PortError;
use super::transcript::Source;

/// One request to transcribe, or to learn where an earlier one is.
#[derive(Debug, Clone)]
pub struct TranscribeRequest {
    /// The caller's idempotency id: the same id is the same job.
    pub request_id: String,
    /// The audio file's bytes.
    pub audio: Vec<u8>,
    /// Its media type.
    pub media_type: String,
    /// The language the caller named, if any.
    pub language: Option<String>,
    /// The audio Artifact version these bytes are.
    pub source: Source,
    /// How long the call may wait for the job to finish.
    pub wait_ms: u64,
}

/// Where a transcription job is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscribeStatus {
    /// The job is open: ask again with the same `request_id`.
    Pending {
        /// The job's identity.
        job_id: String,
        /// How long to wait before asking again.
        poll_after_ms: u64,
    },
    /// The job finished.
    Done {
        /// The job's identity.
        job_id: String,
        /// The transcript document the gateway built; the authority validates it again.
        transcript: Vec<u8>,
        /// The catalog id of the route that produced it.
        model: String,
        /// The provider that served the route.
        provider: String,
    },
    /// The job failed; the request id is free again.
    Failed {
        /// The error code the gateway or the transcript rules named.
        code: String,
        /// What went wrong, without payloads.
        message: String,
        /// Whether asking again may help.
        retryable: bool,
    },
}

/// The gateway's transcription route, as the authority sees it.
#[async_trait]
pub trait TranscriptionPort: Send + Sync {
    /// Submit the job if this request id is new, then report where it is, waiting up to `wait_ms`.
    ///
    /// # Errors
    /// [`PortError::Unavailable`] when no route can be reached, [`PortError::Failed`] when the
    /// transport to the gateway failed.
    async fn request(&self, request: TranscribeRequest) -> Result<TranscribeStatus, PortError>;
}
