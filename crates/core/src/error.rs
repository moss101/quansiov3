//! Typed errors for the identity and replay-safety primitives (DOMAIN.md §15).

use thiserror::Error;

/// Errors produced by canonical identity, generation and idempotency validation.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    /// A string is not a valid 26-character Crockford base32 ULID.
    #[error("invalid ULID: {0}")]
    InvalidUlid(String),
    /// A canonical id does not carry the expected prefix.
    #[error("invalid canonical id {value:?}: expected prefix {expected:?}")]
    InvalidPrefix {
        /// The rejected value.
        value: String,
        /// The prefix that was expected.
        expected: String,
    },
    /// The id carries a prefix that is not in the canonical table (DOMAIN.md §1.1).
    #[error("unknown canonical prefix: {0}")]
    UnknownPrefix(String),
    /// A generation or revision is older than the current value.
    #[error("stale generation: received {received}, current {current}")]
    StaleGeneration {
        /// Generation carried by the rejected message.
        received: u64,
        /// Current authoritative generation.
        current: u64,
    },
    /// A fence token does not match the lease the runtime currently holds.
    #[error("fence mismatch: token {token} does not match lease {lease_id}")]
    FenceMismatch {
        /// Token presented by the worker.
        token: String,
        /// Lease the runtime currently holds.
        lease_id: String,
    },
    /// A fence token could not be parsed.
    #[error("invalid fence token: {0}")]
    InvalidFenceToken(String),
    /// A cursor could not be decoded.
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),
    /// A duplicate command carried different parameters than the original.
    #[error("idempotency mismatch for command {command_id}: parameters differ")]
    IdempotencyMismatch {
        /// The command whose parameters differ.
        command_id: String,
    },
}
