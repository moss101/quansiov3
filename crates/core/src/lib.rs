//! Shared identity, replay-safety and error primitives for the Quansio runtime.
//!
//! Canonical owner (DOSSIER.md §17): `crates/core`. Every canonical identifier,
//! generation counter, fence token, cursor and idempotency key in the platform comes
//! from here, so replay safety is defined once (DOMAIN.md §1).
//!
//! This crate is pure: it performs no I/O, holds no state and depends on no database,
//! which is what makes the replay-safety rules testable in isolation.
#![forbid(unsafe_code)]

/// Repository path of this crate's canonical owner.
pub const CANONICAL_OWNER: &str = "crates/core";

pub mod cursor;
pub mod error;
pub mod generation;
pub mod id;
pub mod idempotency;
pub mod ulid;

pub use cursor::Cursor;
pub use error::CoreError;
pub use generation::{FenceToken, Generation, Revision, Sequence};
pub use id::{CanonicalId, CausationId, CommandId, CorrelationId, EventId, Prefix, TypedId};
pub use idempotency::{classify_duplicate, Digest, DuplicateOutcome, IdempotencyKey};
pub use ulid::{Clock, EntropySource, OsEntropy, SystemClock, Ulid, UlidGenerator};
