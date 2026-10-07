//! Business Capability Pack installs (ADP-008, CAP-005; DOMAIN.md 11.6).
//!
//! A pack is installed **by revision**: the sha256 of its canonical content (manifest and files). The
//! [`PackInstaller`] takes a pack through `fetch`, `verify`, `compile` (the intelligence plane's
//! `CompilePack`) and `activate`; only activation changes what is active, in one transaction in the
//! [`PackStore`], which retains the revision it replaced (bounded by `config/pack-install.yaml`) so
//! [`PackStore::rollback`] can activate it again. Every attempt leaves a redacted, bounded, expiring
//! [`InstallReport`]. This is the one install path: it reuses the CAP-004/CAP-005 compiler and quarantine rules in the
//! plane, the `capability_packs` tables of migration 0001 and the lazy retention sweep of the other control owners.

pub mod compile;
pub mod config;
pub mod content;
pub mod install;
pub mod redact;
pub mod report;
pub mod store;

pub use compile::{
    parse_compilation, Compilation, CompileRequest, CompileUnavailable, GatewayPackCompiler,
    PackCompilePort,
};
pub use config::PackConfig;
pub use content::{fetch, report_name, PackContent, PackSource, GOVERNANCE};
pub use install::{InstallError, InstallRequest, Installed, PackInstaller, PhaseHook};
pub use report::{Diagnostic, InstallReport, Outcome, Phase, Severity, State};
pub use store::{
    Activated, Activation, Begun, PackError, PackStore, PackSummary, RevisionInfo, RolledBack,
    PACKS_OWNER,
};
