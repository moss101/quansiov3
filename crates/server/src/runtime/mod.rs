//! Run/Turn/Step/Attempt state machines, agent turn loop and recovery composition
//! (RUN-001..RUN-011).
//!
//! CORE-006 owns the durable protocol-state and checkpoint metadata that make exact
//! resume possible without reading semantic memory (DOSSIER.md §8, DOMAIN.md §5.7–§5.8).

pub mod agents;
pub mod checkpoints;
pub mod orchestration;
pub mod planning;
pub mod protocol_state;
pub mod recovery;
pub mod state_machine;
pub mod turn_loop;
pub mod verification;
