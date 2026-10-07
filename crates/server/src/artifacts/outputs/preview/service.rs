//! Preview scopes and the loopback server that serves them (DOMAIN.md §10.4 F).
//!
//! A scope is a short-lived capability to read one directory of a workspace root over HTTP. It
//! lives only in memory (nothing about it survives a restart), carries a random token, and is
//! re-validated against the [`PreviewAuthority`] on **every** request, so a link kept past the
//! moment it was meant for stops working the moment the person, the run, the workspace or the root
//! it was issued for changes.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use ring::rand::{SecureRandom, SystemRandom};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use sqlx::PgPool;

use super::authority::{AuthorityError, Binding, Lapse, PreviewAuthority, Resolved};
use super::clock::Clock;
use super::suggest::{self, Kind, Log, Stored, Suggestion};
use crate::artifacts::outputs::config::{OutputsConfig, Preview};
use crate::artifacts::outputs::quality::{Check, QualityRecord, Trust};
use crate::artifacts::outputs::support::{
    commit_output, find_output, read_version_bytes, Ctx, Env, OutputSpec,
};
use crate::artifacts::{ArtifactKind, ObjectStore};
use crate::runtime::turn_loop::{HostDispatch, HostOutcome};

/// How many lapsed scopes are remembered, so a lapsed link keeps answering `PREVIEW_SCOPE_LAPSED`
/// (and a token that was never issued keeps answering `NOT_FOUND`).
const REMEMBERED_LAPSES: usize = 4096;

/// Why a scope could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// The directory is not a directory a scope can be opened on.
    Invalid(String),
    /// The binding no longer holds.
    Lapsed(Lapse),
    /// There is no root to serve from.
    Refused(String),
    /// The authority could not be asked.
    Unavailable(String),
    /// The scope limit is reached.
    TooMany(usize),
}

/// Why a call about an existing scope failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeError {
    /// No such scope was ever issued here (or it was forgotten long ago).
    Unknown,
    /// The scope belongs to someone else.
    Forbidden,
    /// The scope no longer serves.
    Lapsed {
        /// Why.
        reason: Lapse,
        /// The scope.
        scope_id: String,
    },
    /// The authority could not be asked; nothing was decided and the scope is unchanged.
    Unavailable(String),
}

/// Why a suggestion was not stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageError {
    /// The scope already holds as many as it may.
    TooMany(usize),
    /// The suggestion is out of bounds.
    Invalid(String),
    /// The store refused.
    Store(String),
}

/// A scope that was opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// The scope's identity: what the tools take.
    pub scope_id: String,
    /// The capability token: part of the URL, never of a tool argument.
    pub token: String,
    /// When it lapses unless renewed (ms since the epoch).
    pub expires_ms: u64,
    /// How long it lives from now.
    pub ttl_ms: u64,
}

/// A scope that was renewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renewed {
    /// When it now lapses (ms since the epoch).
    pub expires_ms: u64,
    /// How long it may still be renewed for in all.
    pub lifetime_remaining_ms: u64,
}

#[derive(Debug, Clone)]
struct Scope {
    scope_id: String,
    binding: Binding,
    dispatch: HostDispatch,
    directory: String,
    generation: String,
    created_ms: u64,
    expires_ms: u64,
    lock: Arc<tokio::sync::Mutex<()>>,
}

/// One live scope as the person's preview panel lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// `pvs_` and the first 16 hex digits of the token's digest.
    pub scope_id: String,
    /// The directory, relative to the workspace root.
    pub directory: String,
    /// When the scope lapses unless renewed, in epoch milliseconds.
    pub expires_ms: u64,
    /// Time left, in milliseconds.
    pub ttl_ms: u64,
}

#[derive(Debug, Clone)]
struct Tomb {
    scope_id: String,
    reason: Lapse,
    binding: Binding,
}

/// A scope that passed every check for the request at hand.
#[derive(Debug, Clone)]
pub struct Active {
    /// The scope.
    pub scope_id: String,
    /// What it is bound to.
    pub binding: Binding,
    /// The call it was opened by (the run, the step, the member).
    pub dispatch: HostDispatch,
    /// The directory it serves, relative to the workspace root.
    pub directory: String,
    /// What the authority resolved just now.
    pub resolved: Resolved,
    /// Serializes writes to the scope's suggestions.
    pub lock: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Default)]
struct Registry {
    live: HashMap<String, Scope>,
    lapsed: HashMap<String, Tomb>,
    order: VecDeque<String>,
    by_id: HashMap<String, String>,
}

impl Registry {
    fn remember(&mut self, token: String, tomb: Tomb) {
        self.by_id.insert(tomb.scope_id.clone(), token.clone());
        self.order.push_back(token.clone());
        self.lapsed.insert(token, tomb);
        while self.order.len() > REMEMBERED_LAPSES {
            if let Some(oldest) = self.order.pop_front() {
                if let Some(old) = self.lapsed.remove(&oldest) {
                    self.by_id.remove(&old.scope_id);
                }
            }
        }
    }
}

/// What the HTTP handlers and the tools share.
pub struct Inner {
    pub(super) env: Env,
    pub(super) limits: Preview,
    authority: Arc<dyn PreviewAuthority>,
    pub(super) clock: Arc<dyn Clock>,
    registry: Mutex<Registry>,
    pub(super) port: u16,
}

fn principal_of(dispatch: &HostDispatch) -> String {
    dispatch
        .acting_user_id
        .clone()
        .unwrap_or_else(|| format!("run:{}", dispatch.run_id))
}

fn binding_principal(binding: &Binding) -> String {
    binding
        .acting_user_id
        .clone()
        .unwrap_or_else(|| format!("run:{}", binding.run_id))
}

fn binding_of(dispatch: &HostDispatch) -> Binding {
    Binding {
        tenant_id: dispatch.tenant_id.clone(),
        workspace_id: dispatch.workspace_id.clone(),
        run_id: dispatch.run_id.clone(),
        acting_user_id: dispatch.acting_user_id.clone(),
    }
}

/// The directory a scope is opened on, as a clean relative path (`.` for the root).
///
/// # Errors
/// A sentence naming what is wrong.
pub fn clean_directory(directory: &str) -> Result<String, String> {
    if directory.is_empty() || directory.len() > 512 {
        return Err("directory is 1 to 512 characters".to_string());
    }
    if directory.starts_with('/') || directory.contains('\\') || directory.contains('\0') {
        return Err("directory is relative to the workspace root".to_string());
    }
    let mut kept: Vec<&str> = Vec::new();
    for segment in directory.split('/') {
        match segment {
            "" | "." => {}
            ".." => return Err("directory does not leave the workspace root".to_string()),
            other if other.starts_with('.') => {
                return Err("directory does not name a hidden folder".to_string())
            }
            other => kept.push(other),
        }
    }
    Ok(if kept.is_empty() {
        ".".to_string()
    } else {
        kept.join("/")
    })
}

impl Inner {
    fn registry(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().expect("the preview registry")
    }

    /// Lapse a scope: it stops serving and keeps answering with the reason.
    fn lapse(&self, token: &str, reason: Lapse) -> Option<Tomb> {
        let mut registry = self.registry();
        if let Some(scope) = registry.live.remove(token) {
            let tomb = Tomb {
                scope_id: scope.scope_id,
                reason,
                binding: scope.binding,
            };
            registry.remember(token.to_string(), tomb.clone());
            return Some(tomb);
        }
        registry.lapsed.get(token).cloned()
    }

    /// The scope behind a token, if it is live and its time has not run out, after the authority
    /// agrees that nothing it was bound to changed.
    pub(super) async fn activate(&self, token: &str) -> Result<Active, ScopeError> {
        let scope = {
            let registry = self.registry();
            match registry.live.get(token) {
                Some(scope) => scope.clone(),
                None => {
                    return Err(match registry.lapsed.get(token) {
                        Some(tomb) => ScopeError::Lapsed {
                            reason: tomb.reason,
                            scope_id: tomb.scope_id.clone(),
                        },
                        None => ScopeError::Unknown,
                    })
                }
            }
        };
        let lapsed = |tomb: Option<Tomb>| match tomb {
            Some(tomb) => ScopeError::Lapsed {
                reason: tomb.reason,
                scope_id: tomb.scope_id,
            },
            None => ScopeError::Unknown,
        };
        if self.clock.now_ms() >= scope.expires_ms {
            return Err(lapsed(self.lapse(token, Lapse::Expired)));
        }
        match self.authority.resolve(&scope.binding).await {
            Ok(resolved) if resolved.generation == scope.generation => Ok(Active {
                scope_id: scope.scope_id,
                binding: scope.binding,
                dispatch: scope.dispatch,
                directory: scope.directory,
                resolved,
                lock: scope.lock,
            }),
            Ok(_) | Err(AuthorityError::Refused(_)) => {
                Err(lapsed(self.lapse(token, Lapse::RootsChanged)))
            }
            Err(AuthorityError::Lapsed(reason)) => Err(lapsed(self.lapse(token, reason))),
            Err(AuthorityError::Unavailable(detail)) => Err(ScopeError::Unavailable(detail)),
        }
    }

    /// The token of a scope that the caller may act on.
    fn owned_token(&self, dispatch: &HostDispatch, scope_id: &str) -> Result<String, ScopeError> {
        let registry = self.registry();
        let Some(token) = registry.by_id.get(scope_id).cloned().or_else(|| {
            registry
                .live
                .iter()
                .find(|(_, scope)| scope.scope_id == scope_id)
                .map(|(token, _)| token.clone())
        }) else {
            return Err(ScopeError::Unknown);
        };
        let binding = registry
            .live
            .get(&token)
            .map(|scope| &scope.binding)
            .or_else(|| registry.lapsed.get(&token).map(|tomb| &tomb.binding));
        let Some(binding) = binding else {
            return Err(ScopeError::Unknown);
        };
        if binding.tenant_id == dispatch.tenant_id
            && binding.workspace_id == dispatch.workspace_id
            && binding_principal(binding) == principal_of(dispatch)
        {
            Ok(token)
        } else {
            Err(ScopeError::Forbidden)
        }
    }

    /// The live scopes the caller may act on, oldest first. Never a token. Each candidate's authority is
    /// resolved again first (as a request to it would), so a scope whose run ended, member left or root changed
    /// is closed here and not listed.
    pub(super) async fn list(
        &self,
        dispatch: &HostDispatch,
        directory: Option<&str>,
    ) -> Result<Vec<Listed>, ScopeError> {
        let candidates: Vec<(u64, String, String)> = {
            let mut registry = self.registry();
            self.sweep(&mut registry);
            registry
                .live
                .iter()
                .filter(|(_, scope)| {
                    scope.binding.tenant_id == dispatch.tenant_id
                        && scope.binding.workspace_id == dispatch.workspace_id
                        && binding_principal(&scope.binding) == principal_of(dispatch)
                        && directory.is_none_or(|wanted| scope.directory == wanted)
                })
                .map(|(token, scope)| (scope.created_ms, scope.scope_id.clone(), token.clone()))
                .collect()
        };
        let mut candidates = candidates;
        candidates.sort();
        let mut listed = Vec::with_capacity(candidates.len());
        for (_, _, token) in candidates {
            match self.activate(&token).await {
                Ok(active) => {
                    let registry = self.registry();
                    if let Some(scope) = registry.live.get(&token) {
                        listed.push(Listed {
                            scope_id: active.scope_id,
                            directory: scope.directory.clone(),
                            expires_ms: scope.expires_ms,
                            ttl_ms: scope.expires_ms.saturating_sub(self.clock.now_ms()),
                        });
                    }
                }
                // Lapsed or gone while we looked: not live, so not listed.
                Err(ScopeError::Lapsed { .. } | ScopeError::Unknown) => {}
                Err(other) => return Err(other),
            }
        }
        Ok(listed)
    }

    fn sweep(&self, registry: &mut Registry) {
        let now = self.clock.now_ms();
        let expired: Vec<String> = registry
            .live
            .iter()
            .filter(|(_, scope)| now >= scope.expires_ms)
            .map(|(token, _)| token.clone())
            .collect();
        for token in expired {
            if let Some(scope) = registry.live.remove(&token) {
                registry.remember(
                    token,
                    Tomb {
                        scope_id: scope.scope_id,
                        reason: Lapse::Expired,
                        binding: scope.binding,
                    },
                );
            }
        }
    }

    pub(super) async fn open(
        &self,
        dispatch: &HostDispatch,
        directory: &str,
    ) -> Result<Opened, OpenError> {
        let directory = clean_directory(directory).map_err(OpenError::Invalid)?;
        let binding = binding_of(dispatch);
        let resolved = self
            .authority
            .resolve(&binding)
            .await
            .map_err(|error| match error {
                AuthorityError::Lapsed(reason) => OpenError::Lapsed(reason),
                AuthorityError::Refused(detail) => OpenError::Refused(detail),
                AuthorityError::Unavailable(detail) => OpenError::Unavailable(detail),
            })?;
        let base = tokio::fs::canonicalize(resolved.workspace_root.join(&directory))
            .await
            .map_err(|_| OpenError::Invalid("that directory does not exist".to_string()))?;
        let is_directory = tokio::fs::metadata(&base)
            .await
            .map(|meta| meta.is_dir())
            .unwrap_or(false);
        if !base.starts_with(&resolved.workspace_root) || !is_directory {
            return Err(OpenError::Invalid(
                "that is not a directory inside the workspace root".to_string(),
            ));
        }
        let now = self.clock.now_ms();
        let mut registry = self.registry();
        self.sweep(&mut registry);
        if let Some((token, scope)) = registry.live.iter().find(|(_, scope)| {
            scope.binding == binding
                && scope.directory == directory
                && scope.generation == resolved.generation
        }) {
            return Ok(Opened {
                scope_id: scope.scope_id.clone(),
                token: token.clone(),
                expires_ms: scope.expires_ms,
                ttl_ms: scope.expires_ms.saturating_sub(now),
            });
        }
        if registry.live.len() >= self.limits.max_scopes {
            return Err(OpenError::TooMany(self.limits.max_scopes));
        }
        let mut raw = vec![0_u8; self.limits.token_bytes];
        SystemRandom::new()
            .fill(&mut raw)
            .map_err(|_| OpenError::Unavailable("no source of randomness".to_string()))?;
        let token = hex::encode(raw);
        let scope_id = format!(
            "pvs_{}",
            &hex::encode(Sha256::digest(token.as_bytes()))[..16]
        );
        let expires_ms = now + self.limits.ttl_ms;
        registry.live.insert(
            token.clone(),
            Scope {
                scope_id: scope_id.clone(),
                binding,
                dispatch: dispatch.clone(),
                directory,
                generation: resolved.generation,
                created_ms: now,
                expires_ms,
                lock: Arc::new(tokio::sync::Mutex::new(())),
            },
        );
        registry.by_id.insert(scope_id.clone(), token.clone());
        Ok(Opened {
            scope_id,
            token,
            expires_ms,
            ttl_ms: self.limits.ttl_ms,
        })
    }

    pub(super) async fn renew(
        &self,
        dispatch: &HostDispatch,
        scope_id: &str,
    ) -> Result<Renewed, ScopeError> {
        let token = self.owned_token(dispatch, scope_id)?;
        self.activate(&token).await?;
        let now = self.clock.now_ms();
        let mut registry = self.registry();
        let Some(scope) = registry.live.get_mut(&token) else {
            return Err(ScopeError::Unknown);
        };
        let ceiling = scope.created_ms + self.limits.max_lifetime_ms;
        let expires = (now + self.limits.ttl_ms).min(ceiling);
        if expires <= now {
            drop(registry);
            return Err(match self.lapse(&token, Lapse::Expired) {
                Some(tomb) => ScopeError::Lapsed {
                    reason: tomb.reason,
                    scope_id: tomb.scope_id,
                },
                None => ScopeError::Unknown,
            });
        }
        scope.expires_ms = scope.expires_ms.max(expires);
        Ok(Renewed {
            expires_ms: scope.expires_ms,
            lifetime_remaining_ms: ceiling.saturating_sub(now),
        })
    }

    pub(super) fn close(&self, dispatch: &HostDispatch, scope_id: &str) -> Result<(), ScopeError> {
        let token = self.owned_token(dispatch, scope_id)?;
        self.lapse(&token, Lapse::Closed);
        Ok(())
    }

    /// Check that the caller owns the scope and that it still serves; the scope's suggestions role
    /// is `suggest::role_of(scope_id)`.
    pub(super) async fn check_owned(
        &self,
        dispatch: &HostDispatch,
        scope_id: &str,
    ) -> Result<Active, ScopeError> {
        let token = self.owned_token(dispatch, scope_id)?;
        self.activate(&token).await
    }

    /// Append a suggestion to the scope's suggestions Artifact, as the member the scope acts for.
    pub(super) async fn stage(
        &self,
        active: &Active,
        suggestion: Suggestion,
    ) -> Result<u64, StageError> {
        suggestion.validate().map_err(StageError::Invalid)?;
        let _one_at_a_time = active.lock.lock().await;
        let ctx = Ctx::new(&self.env, &active.dispatch);
        let role = suggest::role_of(&active.scope_id);
        let existing = find_output(&ctx, &role)
            .await
            .map_err(|outcome| StageError::Store(message_of(&outcome)))?;
        let mut log = match existing {
            Some((_, version)) => {
                let bytes = read_version_bytes(&ctx, &version.id)
                    .await
                    .map_err(|outcome| StageError::Store(message_of(&outcome)))?;
                Log::parse(&bytes, &self.limits).map_err(StageError::Store)?
            }
            None => Log::new(&active.scope_id),
        };
        if log.suggestions.len() >= self.limits.max_suggestions {
            return Err(StageError::TooMany(self.limits.max_suggestions));
        }
        let index = log.suggestions.len() as u64;
        let at = i64::try_from(self.clock.now_ms())
            .ok()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map_or_else(String::new, |time| {
                time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            });
        log.suggestions.push(Stored {
            index,
            at,
            kind: suggestion.kind,
            selector: suggestion.selector,
            before: suggestion.before,
            after: suggestion.after,
            note: suggestion.note,
        });
        let mut record = QualityRecord::new(suggest::ADAPTER, Trust::UntrustedExternal);
        record.environment.insert(
            "scope_id".to_string(),
            Value::String(active.scope_id.clone()),
        );
        record.checks.push(Check::passed(
            "output.webdev.preview.suggestions",
            format!(
                "{} suggestion(s), each bounded plain text labelled {}",
                log.suggestions.len(),
                suggest::TRUST
            ),
        ));
        commit_output(
            &ctx,
            OutputSpec {
                role,
                kind: ArtifactKind::Data,
                title: "Preview suggestions".to_string(),
                media_type: suggest::MEDIA_TYPE.to_string(),
                bytes: log.to_bytes(),
                record,
            },
        )
        .await
        .map_err(|outcome| StageError::Store(message_of(&outcome)))?;
        Ok(index)
    }
}

fn message_of(outcome: &HostOutcome) -> String {
    match outcome {
        HostOutcome::Failure { error, .. } => error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the store refused")
            .to_string(),
        _ => "the store did not answer".to_string(),
    }
}

/// The preview scopes of this server and the loopback listener that serves them.
pub struct PreviewService {
    inner: Arc<Inner>,
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for PreviewService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewService")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

impl Drop for PreviewService {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl PreviewService {
    /// Bind `127.0.0.1` on an unused port and start serving.
    ///
    /// # Errors
    /// The bind error.
    pub async fn start(
        pool: PgPool,
        objects: Arc<dyn ObjectStore>,
        config: &OutputsConfig,
        authority: Arc<dyn PreviewAuthority>,
        clock: Arc<dyn Clock>,
    ) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let inner = Arc::new(Inner {
            env: Env {
                pool,
                objects,
                config: Arc::new(config.clone()),
                render: None,
                media: None,
                transcription: None,
                preview: None,
            },
            limits: config.preview.clone(),
            authority,
            clock,
            registry: Mutex::new(Registry::default()),
            port: addr.port(),
        });
        let router = super::serve::router(Arc::clone(&inner));
        let task = tokio::spawn(async move {
            // A failed server leaves every scope unreachable, which is the safe state.
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self { inner, addr, task })
    }

    /// The address the scopes are served on.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The URL that serves a scope.
    #[must_use]
    pub fn url_for(&self, token: &str) -> String {
        format!("http://127.0.0.1:{}/p/{token}/", self.addr.port())
    }

    /// The limits in force.
    #[must_use]
    pub fn limits(&self) -> &Preview {
        &self.inner.limits
    }

    /// The service's clock.
    #[must_use]
    pub fn now_ms(&self) -> u64 {
        self.inner.clock.now_ms()
    }

    /// Open a scope on a directory of the workspace root.
    ///
    /// # Errors
    /// [`OpenError`].
    pub async fn open(
        &self,
        dispatch: &HostDispatch,
        directory: &str,
    ) -> Result<Opened, OpenError> {
        self.inner.open(dispatch, directory).await
    }

    /// The live scopes the caller opened (or, without an acting member, the caller's run), optionally on
    /// one directory. The host-only `webdev.preview_scopes` tool reads this; it never returns a token.
    ///
    /// # Errors
    /// [`ScopeError::Unavailable`] when the authority cannot be read.
    pub async fn list(
        &self,
        dispatch: &HostDispatch,
        directory: Option<&str>,
    ) -> Result<Vec<Listed>, ScopeError> {
        self.inner.list(dispatch, directory).await
    }

    /// Extend a scope.
    ///
    /// # Errors
    /// [`ScopeError`].
    pub async fn renew(
        &self,
        dispatch: &HostDispatch,
        scope_id: &str,
    ) -> Result<Renewed, ScopeError> {
        self.inner.renew(dispatch, scope_id).await
    }

    /// Close a scope.
    ///
    /// # Errors
    /// [`ScopeError`].
    pub fn close(&self, dispatch: &HostDispatch, scope_id: &str) -> Result<(), ScopeError> {
        self.inner.close(dispatch, scope_id)
    }

    /// Check that a scope is the caller's and still serves.
    ///
    /// # Errors
    /// [`ScopeError`].
    pub async fn check_owned(
        &self,
        dispatch: &HostDispatch,
        scope_id: &str,
    ) -> Result<Active, ScopeError> {
        self.inner.check_owned(dispatch, scope_id).await
    }

    /// Stage a suggestion for a scope (what the HTTP endpoint does).
    ///
    /// # Errors
    /// [`StageError`].
    pub async fn stage(&self, active: &Active, suggestion: Suggestion) -> Result<u64, StageError> {
        self.inner.stage(active, suggestion).await
    }
}

/// The kind of a suggestion, as a string (a small convenience for results).
#[must_use]
pub const fn kind_str(kind: Kind) -> &'static str {
    match kind {
        Kind::Text => "text",
        Kind::Style => "style",
        Kind::Comment => "comment",
    }
}
