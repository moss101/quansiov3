//! CompletionContract verification (RUN-008, DOMAIN.md §4.4).
//!
//! The model proposes; the trusted runtime owns reality. A completion claim is therefore only
//! ever a *claim*: this module turns the WorkNode's CompletionContract into evidence a model
//! cannot fabricate, and the engine reaches `SUCCEEDED` only when the deterministic checks pass
//! (and the optional independent semantic verifier agrees).
//!
//! The order is fixed and fail-closed:
//!
//! ```text
//! load the run → its WorkNode → its CompletionContract        (a claim with no contract cannot certify)
//! → deterministic checks, in declaration order                 (artifacts, effects, predicates, tests, citations)
//! → human signoff, when the contract requires it                (a model may never sign off for a human)
//! → optional independent semantic verification                  (disagreement rejects, never passes)
//! ```
//!
//! A check kind whose owner has not landed yet is **not** skipped: it fails the contract and
//! names the owning task, so an unimplemented check can never become a silent pass.

pub mod checks;
pub mod contract;
pub mod service;

use thiserror::Error;

pub use checks::{effects_settled_verdict, run_check, CheckResult, CheckScope};
pub use contract::{CompletionContract, DeterministicCheck, SemanticVerification};
pub use service::{
    ContractVerifier, SemanticVerdict, SemanticVerificationRequest, SemanticVerifierPort,
    UnavailableSemanticVerifier,
};

/// Repository path of this module's canonical owner.
pub const VERIFICATION_OWNER_PATH: &str = "crates/server/src/runtime/verification";

/// The task that owns running a contract's `test_command` through the execution boundary.
pub const COMMAND_CHECK_OWNER: &str = "EXEC-006";
/// The task that owns computing citation coverage.
pub const CITATION_CHECK_OWNER: &str = "CAP-002";
/// The task that owns the typed predicate registry `assertion` names.
pub const PREDICATE_OWNER: &str = "RUN-008";
/// The task that owns the gateway-backed independent semantic verifier.
pub const SEMANTIC_VERIFIER_OWNER: &str = "INT-002";

/// A verification refusal.
#[derive(Debug, Error)]
pub enum VerificationError {
    /// The stored contract is not a valid CompletionContract.
    #[error("the CompletionContract is malformed: {detail}")]
    MalformedContract {
        /// What the contract must say.
        detail: String,
    },
    /// The contract names a check kind this runtime does not implement.
    #[error("the CompletionContract names an unknown check kind {kind:?}")]
    UnknownCheck {
        /// The unrecognised kind.
        kind: String,
    },
    /// The run or its WorkNode does not exist for this tenant.
    #[error("{entity} {id} was not found for this tenant")]
    NotFound {
        /// Entity kind.
        entity: &'static str,
        /// Entity identity.
        id: String,
    },
    /// The predicate registry has no evaluator for the named predicate.
    #[error("no evaluator is installed for typed predicate {expr:?}")]
    UnknownPredicate {
        /// Predicate id from the contract.
        expr: String,
    },
    /// A database failure.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

impl VerificationError {
    /// The DOMAIN.md §15 error code this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MalformedContract { .. }
            | Self::UnknownCheck { .. }
            | Self::UnknownPredicate { .. } => "VALIDATION_SCHEMA",
            Self::NotFound { .. } => "NOT_FOUND",
            Self::Database(_) => "INTERNAL",
        }
    }
}
