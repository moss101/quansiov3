//! Worker daemon: tool host, terminal, checkpoints and managed browser (CDP).
//!
//! Canonical owner (DOSSIER.md §17): `crates/qworkerd`.
//!
//! `host` is qworkerd's side of the control channel (EXEC-002): it validates the typed envelope the
//! machine gateway issued against the lease it holds, runs each dispatch at most once, and reports what
//! it observed. `tools` is the tool host (EXEC-006): file, directory, patch, process and terminal
//! operations that every one take a path proven to be inside an authorized root and, when they mutate,
//! the capability/effect/idempotency context they settle. `browser` is the managed session (EXEC-009),
//! driven over CDP with the DOM, accessibility tree and metadata first and a screenshot only as the
//! fallback. It holds no database and evaluates no policy —
//! the runtime decided all of that before dispatching, and a worker that re-decided would be a second
//! authority.
#![forbid(unsafe_code)]

pub mod browser;
pub mod host;
pub mod tools;

/// Repository path of this crate's canonical owner, used by the workspace
/// conformance check to prove one owner maps to exactly one package.
pub const CANONICAL_OWNER: &str = "crates/qworkerd";
