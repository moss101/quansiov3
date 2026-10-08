//! The server's host for native computer tools (ADP-002, DOMAIN.md §8.6.1, composed from EXEC-010's dispatcher).
//!
//! A model-proposed `computer.*` call has been validated, authorized by the run's Capability Projection, decided by
//! policy and reserved on the Effect Ledger when it arrives here. [`ComputerToolHost`] walks it through
//! [`quansio_machine::computer_use::sequence::ComputerSequence`] so that the rules of §8.6.1 B (a user stop, a
//! takeover, a spent limit, a revoked permission) apply to the call, and answers the dispatcher with what the ledger
//! settles from the receipt. A permission that is missing is not a model retry: the host asks the run to be
//! suspended on the attention item (`HostOutcome::Suspend`) and the person resumes it.
//!
//! The platform bridge is a port. This build composes none that can reach a real machine, because the helper
//! executable that holds the OS permissions and the channel to it do not exist yet (ADP-002 notes); the default is
//! [`UnavailableComputerBridge`], which refuses every call before any input and is a recorded composition gap.

mod host;

pub use host::{
    ComputerRoutedHost, ComputerToolHost, StopRegistry, UnavailableComputerBridge,
    BRIDGE_UNAVAILABLE,
};
