//! Execution substrate adapters.
//!
//! These modules translate the canonical [`crate::control::ExecutionTarget`] state into a narrow
//! platform operation. They own no lifecycle state or effect record of their own.

pub mod macos_capsule;
