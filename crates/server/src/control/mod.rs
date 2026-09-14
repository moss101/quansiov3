//! Control module: workspaces, teammates, connector metadata and administrative
//! command owners (APP-002, OPS-001).
//!
//! CORE-001 adds the authoritative persistence schema owner; later control-plane
//! tasks extend this module without introducing a second store (DOSSIER.md §5).

pub mod conversation;
pub mod schema;
pub mod skills;
