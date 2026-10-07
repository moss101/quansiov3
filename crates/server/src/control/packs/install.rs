//! Installing a pack by revision: fetch, verify, compile, activate (ADP-008, DOMAIN.md 11.6).
//!
//! One install path. A request becomes a report (`running`, phase `fetch`) before anything else happens; the phases
//! then move it along, and a failure at any of them finishes the report as failed and leaves the pack's active
//! revision exactly as it was: `retained_previous` when there is one, `failed` when there is none. Only
//! [`PackStore::activate`] changes what is active, in one transaction. Installing content whose revision is already
//! active is a no-op that reports `activated` (idempotent), and the same command id replays what it left.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use quansio_core::CorrelationId;
use quansio_events::Actor;
use quansio_tools::ToolRegistry;

use super::compile::{CompileRequest, PackCompilePort};
use super::content::{fetch, PackSource};
use super::report::{Diagnostic, InstallReport, Phase, Severity, State};
use super::store::{Activated, Activation, Begun, PackError, PackStore};

/// A seam between phases: called with the phase that just finished and the request id, before the next phase
/// begins. The deployable sets none. A test's hook waits here to stop an install exactly between two phases (a
/// crashed server), or to let another install complete first (a concurrent one).
pub type PhaseHook =
    Arc<dyn Fn(Phase, String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// One install request.
#[derive(Debug, Clone)]
pub struct InstallRequest {
    /// The command id; it names the report.
    pub request_id: String,
    /// The workspace the administrator acts from.
    pub workspace_id: String,
    /// Where the content comes from.
    pub source: PackSource,
    /// The revision the caller expects the content to have.
    pub expected_revision: Option<String>,
    /// The revision the caller believes is active; `Some(None)` is "nothing".
    pub expected_active: Option<Option<String>>,
    /// Who installs.
    pub actor: Actor,
    /// The command's correlation id.
    pub correlation_id: CorrelationId,
}

/// A finished install.
#[derive(Debug, Clone)]
pub struct Installed {
    /// The report the attempt left.
    pub report: InstallReport,
    /// The `pckv_` row that is active (absent on a replay).
    pub version_id: Option<String>,
    /// The revision was already active: nothing moved.
    pub unchanged: bool,
    /// Revisions that fell out of retention.
    pub evicted: Vec<String>,
    /// This is the answer an earlier attempt with the same command id left.
    pub replayed: bool,
}

/// Why an install did not finish as an install.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// The install failed at a phase; the report says where, why and what is still active.
    #[error("pack install {} failed at {}", .0.request_id, .0.phase.as_str())]
    Failed(Box<InstallReport>),
    /// The store refused or failed outside any phase.
    #[error(transparent)]
    Store(#[from] PackError),
}

/// The pack installer.
#[derive(Clone)]
pub struct PackInstaller {
    store: PackStore,
    compiler: Option<Arc<dyn PackCompilePort>>,
    registry: &'static ToolRegistry,
    builtin_root: PathBuf,
    hook: Option<PhaseHook>,
}

impl std::fmt::Debug for PackInstaller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackInstaller").finish_non_exhaustive()
    }
}

impl PackInstaller {
    /// An installer over `store`. Without a `compiler` nothing can be installed: every attempt fails at
    /// `compile`, because nothing is activated that was not compiled.
    #[must_use]
    pub fn new(store: PackStore, compiler: Option<Arc<dyn PackCompilePort>>) -> Self {
        let builtin_root = std::env::var_os("QUANSIO_PACKS_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&store.config().install.builtin_root));
        Self {
            store,
            compiler,
            registry: ToolRegistry::builtin(),
            builtin_root,
            hook: None,
        }
    }

    /// Read built-in packs from `root`.
    #[must_use]
    pub fn with_builtin_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.builtin_root = root.into();
        self
    }

    /// Install a seam between phases.
    #[must_use]
    pub fn with_phase_hook(mut self, hook: PhaseHook) -> Self {
        self.hook = Some(hook);
        self
    }

    async fn after(&self, phase: Phase, request_id: &str) {
        if let Some(hook) = &self.hook {
            hook(phase, request_id.to_string()).await;
        }
    }

    /// Plan against another registry (tests).
    #[must_use]
    pub fn with_registry(mut self, registry: &'static ToolRegistry) -> Self {
        self.registry = registry;
        self
    }

    /// The store this installer works through.
    #[must_use]
    pub fn store(&self) -> &PackStore {
        &self.store
    }

    async fn fail(
        &self,
        tenant_id: &str,
        request: &InstallRequest,
        phase: Phase,
        diagnostics: Vec<Diagnostic>,
    ) -> InstallError {
        match self
            .store
            .fail(
                tenant_id,
                &request.request_id,
                phase,
                &diagnostics,
                &request.actor,
                request.correlation_id,
            )
            .await
        {
            Ok(report) => InstallError::Failed(Box::new(report)),
            Err(error) => InstallError::Store(error),
        }
    }

    /// Install `request`'s pack.
    ///
    /// # Errors
    /// [`InstallError::Failed`] with the finished report when a phase failed (the previous revision is still
    /// active); [`InstallError::Store`] for an unexpected store failure or a request still running.
    pub async fn install(
        &self,
        tenant_id: &str,
        request: InstallRequest,
    ) -> Result<Installed, InstallError> {
        let config = self.store.config().clone();
        let begun = self
            .store
            .begin(tenant_id, &request.request_id, &request.source.name_hint())
            .await?;
        match begun {
            Begun::Existing(report) => {
                return match report.state {
                    State::Succeeded => Ok(Installed {
                        report,
                        version_id: None,
                        unchanged: false,
                        evicted: Vec::new(),
                        replayed: true,
                    }),
                    State::Failed => Err(InstallError::Failed(Box::new(report))),
                    State::Running => Err(InstallError::Store(PackError::InFlight {
                        request_id: request.request_id,
                    })),
                };
            }
            Begun::Started(_) => {}
        }

        // fetch
        let content = match fetch(&request.source, &config.install, &self.builtin_root) {
            Ok(content) => content,
            Err(diagnostics) => {
                return Err(self
                    .fail(tenant_id, &request, Phase::Fetch, diagnostics)
                    .await)
            }
        };
        self.after(Phase::Fetch, &request.request_id).await;

        // verify (the runtime's half: the revision is computed here, by the authority)
        let revision = match content.revision() {
            Ok(revision) => revision,
            Err(error) => {
                let finding =
                    Diagnostic::error(Phase::Verify, "pack.schema", error.detail).at(error.pointer);
                return Err(self
                    .fail(tenant_id, &request, Phase::Verify, vec![finding])
                    .await);
            }
        };
        let name = content.name().map(super::content::report_name);
        self.store
            .set_phase(
                tenant_id,
                &request.request_id,
                Phase::Verify,
                name.as_deref(),
                Some(&revision),
            )
            .await?;
        if let Some(expected) = &request.expected_revision {
            if expected != &revision {
                let finding = Diagnostic::error(
                    Phase::Verify,
                    "pack.revision",
                    "the revision named for this install is not the revision of the content",
                );
                return Err(self
                    .fail(tenant_id, &request, Phase::Verify, vec![finding])
                    .await);
            }
        }
        self.after(Phase::Verify, &request.request_id).await;

        // compile (the plane verifies the rest and compiles)
        self.store
            .set_phase(tenant_id, &request.request_id, Phase::Compile, None, None)
            .await?;
        let Some(compiler) = &self.compiler else {
            let finding = Diagnostic::error(
                Phase::Compile,
                "pack.compile.unavailable",
                "no intelligence plane is composed on this deployment, so nothing can be compiled or installed",
            );
            return Err(self
                .fail(tenant_id, &request, Phase::Compile, vec![finding])
                .await);
        };
        let tools = self.registry.visibility();
        let correlation = request.correlation_id.to_string();
        let compilation = compiler
            .compile(CompileRequest {
                tenant_id,
                workspace_id: &request.workspace_id,
                request_id: &request.request_id,
                correlation_id: &correlation,
                content: &content,
                expected_revision: request.expected_revision.as_deref(),
                tools: &tools,
                limits: &config.install,
            })
            .await;
        let compilation = match compilation {
            Ok(compilation) => compilation,
            Err(unavailable) => {
                let finding =
                    Diagnostic::error(Phase::Compile, "pack.compile.unavailable", unavailable.0);
                return Err(self
                    .fail(tenant_id, &request, Phase::Compile, vec![finding])
                    .await);
            }
        };
        if !compilation.ok {
            let phase = compilation.failed_phase.unwrap_or(Phase::Compile);
            return Err(self
                .fail(tenant_id, &request, phase, compilation.diagnostics)
                .await);
        }
        if compilation.revision != revision {
            let finding = Diagnostic::error(
                Phase::Compile,
                "pack.revision.disagree",
                "the plane and the runtime computed different revisions for this content",
            );
            return Err(self
                .fail(tenant_id, &request, Phase::Compile, vec![finding])
                .await);
        }
        self.after(Phase::Compile, &request.request_id).await;

        // activate
        self.store
            .set_phase(tenant_id, &request.request_id, Phase::Activate, None, None)
            .await?;
        let warnings: Vec<Diagnostic> = compilation
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Warning)
            .cloned()
            .collect();
        let expected = request
            .expected_active
            .as_ref()
            .map(|expected| expected.as_deref());
        let activated = self
            .store
            .activate(
                tenant_id,
                Activation {
                    request_id: &request.request_id,
                    content: &content,
                    revision: &revision,
                    warnings: &warnings,
                    resolution: &compilation.resolution,
                    expected_active: expected,
                    actor: &request.actor,
                    correlation_id: request.correlation_id,
                },
            )
            .await;
        match activated {
            Ok(Activated {
                version_id,
                unchanged,
                evicted,
                report,
                ..
            }) => Ok(Installed {
                report,
                version_id: Some(version_id),
                unchanged,
                evicted,
                replayed: false,
            }),
            Err(PackError::Conflict {
                expected, current, ..
            }) => {
                let finding = Diagnostic::error(
                    Phase::Activate,
                    "pack.activate.conflict",
                    format!(
                        "the active revision is {} but this install expected {}",
                        current.as_deref().unwrap_or("none"),
                        expected.as_deref().unwrap_or("none")
                    ),
                );
                Err(self
                    .fail(tenant_id, &request, Phase::Activate, vec![finding])
                    .await)
            }
            Err(PackError::Database(error)) => {
                let finding = Diagnostic::error(
                    Phase::Activate,
                    "pack.activate.failed",
                    format!("activation failed and was rolled back: {error}"),
                );
                Err(self
                    .fail(tenant_id, &request, Phase::Activate, vec![finding])
                    .await)
            }
            Err(other) => Err(InstallError::Store(other)),
        }
    }
}
