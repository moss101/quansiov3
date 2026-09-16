//! Control module: workspaces, teammates, connector metadata and administrative
//! command owners (APP-002, OPS-001).
//!
//! CORE-001 adds the authoritative persistence schema owner; later control-plane
//! tasks extend this module without introducing a second store (DOSSIER.md §5).

pub mod collaboration;
pub mod connectors;
pub mod conversation;
pub mod data_lifecycle;
pub mod identity;
pub mod ops;
pub mod recovery_point;
pub mod routines;
pub mod schema;
pub mod skills;
pub mod teammates;
