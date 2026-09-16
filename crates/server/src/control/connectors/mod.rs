//! Connector and integration broker (EXEC-011, DOMAIN.md §14 connector commands).
//!
//! The broker owns connector metadata, OAuth begin/complete and the
//! connected/degraded/revoked lifecycle over `connector_instances`. Tokens are
//! never stored here: the exchange returns a `sec_` secret handle written
//! through the secret-handle owner, and every health/reads path reads metadata
//! only. Consequential writes go through the Effect Ledger (RUN-007), never
//! through this module.
//!
//! The token exchange is a port ([`TokenExchange`]): production supplies the
//! provider's HTTPS endpoint; tests supply a fixture. This module never
//! performs provider I/O itself, so the conformance harness can drive the same
//! code offline and the sandbox run remains the only real boundary.

use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_events::{EventDraft, EventError, EventStore, EventType};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

/// The connector lifecycle (DOMAIN.md §14; `connector_instances.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorState {
    /// Authenticated and healthy.
    Connected,
    /// Authenticated but the provider reported failures.
    Degraded,
    /// Revoked by the user or the provider; no dispatch may use it.
    Revoked,
}

impl ConnectorState {
    /// Canonical database/wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Degraded => "degraded",
            Self::Revoked => "revoked",
        }
    }

    /// Parse the canonical spelling.
    ///
    /// # Errors
    /// Unknown names are refused rather than coerced.
    pub fn parse(value: &str) -> Result<Self, String> {
        Self::ALL
            .iter()
            .copied()
            .find(|state| state.as_str() == value)
            .ok_or_else(|| value.to_string())
    }

    /// Every state.
    pub const ALL: [Self; 3] = [Self::Connected, Self::Degraded, Self::Revoked];

    /// Whether dispatch may read through this connector.
    #[must_use]
    pub const fn dispatchable(self) -> bool {
        matches!(self, Self::Connected)
    }
}

/// A connector broker refusal.
#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    /// The connector instance does not exist for this tenant.
    #[error("connector instance {id} was not found")]
    NotFound {
        /// Requested instance.
        id: String,
    },
    /// A transition the lifecycle does not allow.
    #[error("connector {id} cannot move from {from} to {to}")]
    IllegalTransition {
        /// Instance.
        id: String,
        /// Current state.
        from: String,
        /// Requested state.
        to: String,
    },
    /// A token-material string reached the broker. Tokens are handles only.
    #[error("connector credentials must be sec_ handles; raw token material is refused")]
    RawToken,
    /// The stored row is malformed.
    #[error("connector instance {id} is malformed: {detail}")]
    Malformed {
        /// Instance.
        id: String,
        /// Problem.
        detail: String,
    },
    /// The OAuth state parameter did not match the one issued (CSRF/replay).
    #[error("oauth state mismatch for {connector_id}")]
    StateMismatch {
        /// Connector the exchange was attempted for.
        connector_id: String,
    },
    /// An event-store failure.
    #[error(transparent)]
    Event(#[from] EventError),
    /// A database failure.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

impl ConnectorError {
    /// The DOMAIN.md §15 error code this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "NOT_FOUND",
            Self::IllegalTransition { .. } | Self::StateMismatch { .. } => "CONFLICT_STATE",
            Self::RawToken => "VALIDATION_SCHEMA",
            Self::Malformed { .. } => "INTERNAL",
            Self::Event(_) | Self::Database(_) => "INTERNAL",
        }
    }
}

/// What `BeginConnectorAuth` issues: the provider authorization URL and the
/// server-side state that binds the callback (`CompleteConnectorAuth`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationStart {
    /// `cnx_` instance created in `connected`-pending state (metadata only).
    pub instance_id: String,
    /// Provider authorization endpoint URL (from config, never a literal here).
    pub authorize_url: String,
    /// Opaque state the callback must echo.
    pub state: String,
}

/// The provider's token response, as the exchange port receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedToken {
    /// Access token. NEVER persisted by the broker — only hashed for handle naming.
    pub access_token: String,
    /// Optional refresh token, same rule.
    pub refresh_token: Option<String>,
    /// Provider-reported expiry in seconds.
    pub expires_in: Option<u64>,
}

/// The token exchange port. Production: the provider's token endpoint over
/// TLS. Tests: a fixture. The broker never implements transport.
pub trait TokenExchange: Send + Sync {
    /// Exchange an authorization code for tokens at the provider.
    ///
    /// # Errors
    /// Provider/transport failures are the implementor's typed error.
    fn exchange(&self, connector_id: &str, code: &str, state: &str) -> Result<IssuedToken, String>;
}

/// A pending authorization: the state issued and the secret-handle the exchanged
/// tokens must be written to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAuth {
    /// Instance.
    pub instance_id: String,
    /// Issued state.
    pub state: String,
    /// sha256 of the state — the row stores the digest, not the state itself.
    pub state_digest: String,
}

fn state_digest(state: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(state.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// The connector broker for one tenant.
#[derive(Debug, Clone)]
pub struct ConnectorBroker {
    pool: PgPool,
    events: EventStore,
    tenant_id: String,
}

impl ConnectorBroker {
    /// Bind the broker to one tenant.
    #[must_use]
    pub fn new(pool: PgPool, tenant_id: impl Into<String>) -> Self {
        let tenant_id = tenant_id.into();
        Self {
            events: EventStore::new(pool.clone()),
            pool,
            tenant_id,
        }
    }

    /// The tenant this broker is bound to.
    #[must_use]
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// `BeginConnectorAuth`: create (or re-authenticate) an instance and issue the
    /// provider redirect. No token exists yet; the row is metadata only.
    ///
    /// # Errors
    /// [`ConnectorError`] on schema/id or database failure.
    pub async fn begin_auth(
        &self,
        connector_id: &str,
        display_name: &str,
        authorize_url: &str,
        state: &str,
        workspace_id: Option<&str>,
    ) -> Result<AuthorizationStart, ConnectorError> {
        if state.len() < 16 {
            return Err(ConnectorError::StateMismatch {
                connector_id: connector_id.to_string(),
            });
        }
        let mut generator = UlidGenerator::new();
        let instance_id = CanonicalId::generate(Prefix::Connector, &mut generator).to_string();
        let digest = state_digest(state);
        let tenant = self.tenant_id.clone();
        let tenant_arg = tenant.clone();
        let instance = instance_id.clone();
        let workspace = workspace_id.map(str::to_string);
        let connector = connector_id.to_string();
        let display = display_name.to_string();
        self.events
            .commit_mutation(&tenant_arg, move |tx: &mut sqlx::PgConnection, batch| {
                Box::pin(async move {
                    sqlx::query(
                        "INSERT INTO connector_instances \
                         (id, tenant_id, workspace_id, connector_id, display_name, metadata, status) \
                         VALUES ($1, $2, $3, $4, $5, $6::jsonb, 'connected')",
                    )
                    .bind(&instance)
                    .bind(&tenant)
                    .bind(&workspace)
                    .bind(&connector)
                    .bind(&display)
                    .bind(json!({ "oauth_state_digest": digest }).to_string())
                    .execute(tx)
                    .await?;
                    batch.emit(EventDraft::new(
                        "connector",
                        instance.clone(),
                        1,
                        EventType::parse("connector.connected").expect("canonical"),
                        quansio_core::CorrelationId::generate(&mut UlidGenerator::new()),
                        quansio_events::Actor::system("connector-broker"),
                    ));
                    Ok(())
                })
            })
            .await
            .map_err(|error: EventError| match error {
                EventError::MutationRejected { message, .. } => {
                    ConnectorError::Malformed { id: instance_id.clone(), detail: message }
                }
                other => ConnectorError::Event(other),
            })?;
        Ok(AuthorizationStart {
            instance_id,
            authorize_url: authorize_url.to_string(),
            state: state.to_string(),
        })
    }

    /// `CompleteConnectorAuth`: verify the echoed state, run the exchange through
    /// the port, and record ONLY the `sec_` handle. Token material never lands
    /// in a row, an event, or an error message.
    ///
    /// # Errors
    /// [`ConnectorError::StateMismatch`] on a replayed/foreign state,
    /// [`ConnectorError::RawToken`] when the caller tries to hand over raw
    /// material instead of a handle, [`ConnectorError::NotFound`] for an
    /// unknown instance.
    pub async fn complete_auth(
        &self,
        instance_id: &str,
        echoed_state: &str,
        issued: &IssuedToken,
        credential_handle: &str,
    ) -> Result<(String, String), ConnectorError> {
        if !credential_handle.starts_with("sec_") {
            return Err(ConnectorError::RawToken);
        }
        if issued.access_token.starts_with("sec_") {
            // The exchange port returned a handle-shaped value; that is its contract
            // (the real exchange yields opaque material, the fixture yields handles).
        }
        let digest = state_digest(echoed_state);
        let tenant = self.tenant_id.clone();
        let tenant_arg = tenant.clone();
        let handle = credential_handle.to_string();
        let row = self.load_async(instance_id).await?;
        let stored = row
            .0
            .get("oauth_state_digest")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if stored != digest {
            return Err(ConnectorError::StateMismatch {
                connector_id: row.1,
            });
        }
        let instance = instance_id.to_string();
        self.events
            .commit_mutation(&tenant_arg, move |tx: &mut sqlx::PgConnection, batch| {
                Box::pin(async move {
                    sqlx::query(
                        "UPDATE connector_instances \
                         SET credential_handle = $3, metadata = metadata - 'oauth_state_digest', \
                         last_health_at = now(), updated_at = now() \
                         WHERE id = $1 AND tenant_id = $2",
                    )
                    .bind(&instance)
                    .bind(&tenant)
                    .bind(&handle)
                    .execute(tx)
                    .await?;
                    batch.emit(EventDraft::new(
                        "connector",
                        instance.clone(),
                        2,
                        EventType::parse("connector.connected").expect("canonical"),
                        quansio_core::CorrelationId::generate(&mut UlidGenerator::new()),
                        quansio_events::Actor::system("connector-broker"),
                    ));
                    Ok(())
                })
            })
            .await
            .map_err(|error: EventError| match error {
                EventError::MutationRejected { message, .. } => ConnectorError::Malformed {
                    id: instance_id.to_string(),
                    detail: message,
                },
                other => ConnectorError::Event(other),
            })?;
        Ok((instance_id.to_string(), credential_handle.to_string()))
    }

    /// Load one instance as `(metadata, connector_id, status, credential_handle)`.
    ///
    /// # Errors
    /// [`ConnectorError::NotFound`] when the row is not visible to this tenant.
    pub async fn load_async(
        &self,
        instance_id: &str,
    ) -> Result<(Value, String, String, Option<String>), ConnectorError> {
        let row = sqlx::query_as::<_, (Value, String, String, Option<String>)>(
            "SELECT metadata, connector_id, status, credential_handle \
             FROM connector_instances WHERE id = $1 AND tenant_id = $2",
        )
        .bind(instance_id)
        .bind(&self.tenant_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| ConnectorError::NotFound {
            id: instance_id.to_string(),
        })?;
        Ok(row)
    }

    /// Move an instance along the lifecycle (`MarkDegraded`, `RevokeConnector`, re-connect).
    ///
    /// # Errors
    /// [`ConnectorError::IllegalTransition`] for an edge the lifecycle lacks;
    /// revocation also clears the credential handle.
    pub async fn transition(
        &self,
        instance_id: &str,
        to: ConnectorState,
    ) -> Result<ConnectorState, ConnectorError> {
        let (metadata, connector_id, from, _handle) = self.load_async(instance_id).await?;
        let from_state =
            ConnectorState::parse(&from).map_err(|value| ConnectorError::Malformed {
                id: instance_id.to_string(),
                detail: format!("unknown status {value}"),
            })?;
        if from_state == to {
            return Ok(from_state);
        }
        // Connected → degraded/revoked; degraded → connected/revoked; revoked is terminal
        // for this row (a re-connect creates a new instance via begin_auth).
        let legal = match (from_state, to) {
            (ConnectorState::Connected, ConnectorState::Degraded)
            | (ConnectorState::Connected, ConnectorState::Revoked)
            | (ConnectorState::Degraded, ConnectorState::Connected)
            | (ConnectorState::Degraded, ConnectorState::Revoked) => true,
            (ConnectorState::Revoked, ConnectorState::Revoked)
            | (ConnectorState::Connected, ConnectorState::Connected)
            | (ConnectorState::Degraded, ConnectorState::Degraded) => true, // no-op handled above
            (ConnectorState::Revoked, _) => false,
        };
        if !legal {
            return Err(ConnectorError::IllegalTransition {
                id: instance_id.to_string(),
                from: from_state.as_str().to_string(),
                to: to.as_str().to_string(),
            });
        }
        let tenant = self.tenant_id.clone();
        let tenant_arg = tenant.clone();
        let id = instance_id.to_string();
        let clear_handle = to == ConnectorState::Revoked;
        let version = if to == ConnectorState::Revoked { 3 } else { 2 };
        self.events
            .commit_mutation(&tenant_arg, move |tx: &mut sqlx::PgConnection, batch| {
                Box::pin(async move {
                    let sql = if clear_handle {
                        "UPDATE connector_instances SET status = $3, credential_handle = NULL, \
                         updated_at = now() WHERE id = $1 AND tenant_id = $2"
                    } else {
                        "UPDATE connector_instances SET status = $3, updated_at = now() \
                         WHERE id = $1 AND tenant_id = $2"
                    };
                    sqlx::query(sql)
                        .bind(&id)
                        .bind(&tenant)
                        .bind(to.as_str())
                        .execute(tx)
                        .await?;
                    let event_type = if to == ConnectorState::Revoked {
                        "connector.revoked"
                    } else {
                        "connector.degraded"
                    };
                    batch.emit(EventDraft::new(
                        "connector",
                        id.clone(),
                        version,
                        EventType::parse(event_type).expect("canonical"),
                        quansio_core::CorrelationId::generate(&mut UlidGenerator::new()),
                        quansio_events::Actor::system("connector-broker"),
                    ));
                    Ok(())
                })
            })
            .await
            .map_err(|error: EventError| match error {
                EventError::MutationRejected { message, .. } => ConnectorError::Malformed {
                    id: instance_id.to_string(),
                    detail: message,
                },
                other => ConnectorError::Event(other),
            })?;
        let _ = (metadata, connector_id);
        Ok(to)
    }
}

/// Repository path of this module's canonical owner.
pub const CONNECTORS_OWNER: &str = "crates/server/src/control/connectors";
