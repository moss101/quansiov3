//! Typed errors for the RuntimeEvent store, outbox and cursors (DOMAIN.md §15).

use quansio_core::CoreError;
use thiserror::Error;

/// Errors returned by the event store.
#[derive(Debug, Error)]
pub enum EventError {
    /// PostgreSQL rejected a statement or the transaction could not complete.
    #[error("event store database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The event `type` names a family that is not in DOMAIN.md §9.2.
    #[error("unknown event family {family:?}; canonical families are tenant, workspace, member, thread, work, agent, run, turn, step, model, tool, effect, approval, question, target, lease, browser, terminal, checkpoint, artifact, evidence, knowledge, memory, skill, pack, routine, notification, connector, policy, audit, usage, capability, compaction, ops")]
    UnknownEventFamily {
        /// The rejected family prefix.
        family: String,
    },
    /// The event `type` is not `<family>.<event>` with a lowercase snake_case event.
    #[error("malformed event type {value:?}: expected <family>.<event>")]
    MalformedEventType {
        /// The rejected value.
        value: String,
    },
    /// The tenant id is not a canonical `tn_` identifier.
    #[error("invalid tenant id {0:?}")]
    InvalidTenantId(String),
    /// A mutation tried to commit without staging its RuntimeEvent.
    ///
    /// No committed state transition may lack its corresponding event, so the store
    /// refuses the commit and rolls the transaction back (DOMAIN.md §9.1).
    #[error("mutation staged no RuntimeEvent; refusing to commit state without its event")]
    NoEventStaged,
    /// The mutation rejected the transaction with its own typed error.
    ///
    /// The store rolls back and returns this wrapper; the rejecting owner carries its
    /// typed error out of band, so a rejection never reaches the event stream.
    #[error("mutation rejected by {owner}: {message}")]
    MutationRejected {
        /// Repository path of the owner that rejected the mutation.
        owner: &'static str,
        /// Human-readable rejection reason.
        message: String,
    },
    /// A cursor token could not be decoded or encoded.
    #[error("cursor error: {0}")]
    Cursor(#[from] CoreError),
    /// A cursor token names a different stream than the one being resumed.
    #[error("cursor stream mismatch: expected {expected:?}, got {found:?}")]
    CursorStreamMismatch {
        /// The stream the consumer subscribed to.
        expected: String,
        /// The stream encoded in the cursor.
        found: String,
    },
    /// A JSON value could not be encoded or decoded.
    #[error("event JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// The event transport failed; the outbox row stays unpublished and is retried.
    #[error("event transport failed: {0}")]
    Transport(#[from] TransportError),
    /// A row read back from `runtime_events` does not satisfy the envelope.
    #[error("malformed runtime_events row: {0}")]
    MalformedEventRow(String),
}

/// Failures reported by an [`crate::EventTransport`].
#[derive(Debug, Error)]
pub enum TransportError {
    /// No event transport is configured or reachable. Publishing fails closed: the
    /// outbox keeps the event unpublished rather than dropping it.
    #[error("event transport unavailable: {0}")]
    Unavailable(String),
    /// The transport rejected the message.
    #[error("event transport rejected the message: {0}")]
    Rejected(String),
}
