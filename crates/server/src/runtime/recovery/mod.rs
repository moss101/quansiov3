//! Recovery after process, worker and machine failure (RUN-009, DOMAIN.md §5.2, §5.7, §7.2).
//!
//! Recovery rebuilds from **durable** state only: ProtocolState, RuntimeEvents, Steps/Attempts,
//! Checkpoints, Evidence and the Effect Ledger. It never consults semantic memory — knowledge,
//! embeddings, memory candidates and compaction are context, not authority — and it never repeats
//! an uncertain external effect: an effect whose outcome is unknown is reconciled first.
//!
//! The three rules this module enforces:
//!
//! 1. **Resume to the same safe logical point.** A killed runtime re-derives the run's next safe
//!    action from durable state; [`plan::plan_from`] holds the precedence (honour a cancellation →
//!    reconcile an unsettled effect → stay parked on a wait → resume).
//! 2. **Fence stale workers.** A generation behind the run's current one cannot mutate it: the
//!    store refuses the transition and [`plan::fence_decision`] names the attempt as stale.
//! 3. **Reconcile before resuming.** An `OUTCOME_UNKNOWN` effect is settled by evidence
//!    (`ReconciliationEvidence::Determined` when the class has a deterministic check, `Manual`
//!    otherwise) and never by dispatching again.
//!
//! [`RECOVERY_READ_TABLES`] is the complete set of tables recovery may read; `recovery.rs` in
//! `crates/server/tests` scans this module's sources and fails if any other table — a memory
//! table above all — is referenced.

pub mod plan;
pub mod service;

use thiserror::Error;

pub use plan::{fence_decision, plan_from, DurableState, FenceDecision, SafeAction};
pub use service::{Recoverer, RecoveryReport, RecoveryResolution};

/// Repository path of this module's canonical owner.
pub const RECOVERY_OWNER: &str = "crates/server/src/runtime/recovery";

/// Every table recovery may read. Durable recovery state only (DOMAIN.md §5.7).
///
/// A table outside this list is not recovery authority: `memory_entries`, `memory_candidates`,
/// embeddings and any other semantic-memory table are context, and a runtime that rebuilt itself
/// from them could resurrect work that never durably happened.
pub const RECOVERY_READ_TABLES: &[&str] = &[
    "runs",
    "turns",
    "steps",
    "attempts",
    "protocol_states",
    "runtime_events",
    "checkpoints",
    "effect_records",
    "evidence",
    "work_nodes",
];

/// Tables recovery must never read (DOMAIN.md §13; "recovery is not memory").
pub const RECOVERY_FORBIDDEN_TABLES: &[&str] = &[
    "memory_entries",
    "memory_candidates",
    "memory_edges",
    "embeddings",
    "embedding_vectors",
    "knowledge_entries",
];

/// A recovery refusal.
#[derive(Debug, Error)]
pub enum RecoveryError {
    /// A generation-stale actor tried to act on the current run.
    #[error("generation {observed} is stale: the run is at generation {current}")]
    StaleGeneration {
        /// Generation the actor carried.
        observed: u64,
        /// Generation the run holds.
        current: u64,
    },
    /// The run does not exist for this tenant.
    #[error("run {id} was not found for this tenant")]
    NotFound {
        /// Run identity.
        id: String,
    },
    /// A runtime transition or store failure.
    #[error(transparent)]
    Runtime(#[from] crate::runtime::state_machine::RuntimeError),
    /// An effect-ledger failure.
    #[error(transparent)]
    Effects(#[from] crate::effects::EffectError),
    /// A database failure.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

impl RecoveryError {
    /// The DOMAIN.md §15 error code this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::StaleGeneration { .. } => "FENCED_STALE_GENERATION",
            Self::NotFound { .. } => "NOT_FOUND",
            Self::Runtime(error) => error.code(),
            Self::Effects(_) => "INTERNAL",
            Self::Database(_) => "INTERNAL",
        }
    }
}

/// Whether a table name may be read during recovery. Used by the module's structural test.
#[must_use]
pub fn is_recovery_read_table(table: &str) -> bool {
    RECOVERY_READ_TABLES.contains(&table)
}
