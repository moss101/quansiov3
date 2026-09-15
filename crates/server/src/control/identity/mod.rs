//! Identity, onboarding, workspaces and layered settings (APP-002).
//!
//! A fresh user is provisioned a personal tenant and a workspace without reading a
//! configuration file. Secrets remain `sec_` handles. Settings merge is deterministic
//! and most-restrictive for security controls.

pub mod enterprise;
pub mod settings;

use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_events::{EventDraft, EventError, EventStore, EventType};
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::control::schema;
use crate::runtime::state_machine::RuntimeIdentity;

pub use enterprise::{
    live_oidc_configured, DirectoryUser, GroupRoleMap, ServicePrincipal, Session, SsoAssertion,
    LIVE_OIDC_ISSUER,
};
pub use settings::{effective_settings, merge_settings, platform_defaults};

/// Errors from identity/onboarding.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// Event store refused the commit.
    #[error("identity event store: {0}")]
    Event(#[from] EventError),
    /// Database error.
    #[error("identity database: {0}")]
    Database(#[from] sqlx::Error),
    /// Schema/id error.
    #[error("identity schema: {0}")]
    Schema(String),
}

/// Result of onboarding a brand-new user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardedUser {
    /// `usr_` id.
    pub user_id: String,
    /// Personal `tn_` tenant.
    pub tenant_id: String,
    /// Default `ws_` workspace.
    pub workspace_id: String,
    /// Email used to sign in.
    pub email: String,
}

/// Identity owner.
#[derive(Debug, Clone)]
pub struct IdentityStore {
    events: EventStore,
}

impl IdentityStore {
    /// Bind to a pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            events: EventStore::new(pool),
        }
    }

    /// Provision a personal tenant and workspace for a new email, with no config file.
    ///
    /// Auth methods record magic-link and oauth providers as names only. Credential
    /// material is a `sec_` handle, never a raw secret.
    ///
    /// # Errors
    /// Returns [`IdentityError`] when ids or the database refuse the write.
    pub async fn onboard_email(
        &self,
        email: &str,
        display_name: &str,
        credential_handle: &str,
    ) -> Result<OnboardedUser, IdentityError> {
        if !credential_handle.starts_with("sec_") {
            return Err(IdentityError::Schema(
                "credential_handle must be a sec_ secret handle".to_string(),
            ));
        }
        let mut generator = UlidGenerator::new();
        let user_id = CanonicalId::generate(Prefix::User, &mut generator).to_string();
        let tenant_id = CanonicalId::generate(Prefix::Tenant, &mut generator).to_string();
        let workspace_id = CanonicalId::generate(Prefix::Workspace, &mut generator).to_string();
        let membership_id = format!("tm_{}", &tenant_id[3..]);
        let workspace_membership_id = format!("wm_{}", &workspace_id[3..]);
        let identity = RuntimeIdentity::system(&tenant_id, "identity", {
            quansio_core::CorrelationId::generate(&mut generator)
        });
        let email = email.to_string();
        let display_name = display_name.to_string();
        let handle = credential_handle.to_string();
        let tenant_for_event = tenant_id.clone();
        let workspace_for_event = workspace_id.clone();
        let user_for_event = user_id.clone();
        let email_for_row = email.clone();
        self.events
            .commit_mutation_tx(&tenant_for_event, move |tx, batch| {
                Box::pin(async move {
                    sqlx::query(
                        "INSERT INTO tenants (id, name, personal, settings) \
                         VALUES ($1, $2, TRUE, '{}'::jsonb)",
                    )
                    .bind(&tenant_id)
                    .bind(format!("{display_name}'s workspace"))
                    .execute(&mut **tx)
                    .await?;
                    schema::set_tenant_context(tx, &tenant_id)
                        .await
                        .map_err(|error| EventError::MutationRejected {
                            owner: "crates/server/src/control/identity",
                            message: error.to_string(),
                        })?;
                    sqlx::query(
                        "INSERT INTO users (id, primary_email, display_name, auth_methods, settings) \
                         VALUES ($1, $2, $3, $4, '{}'::jsonb)",
                    )
                    .bind(&user_id)
                    .bind(&email_for_row)
                    .bind(&display_name)
                    .bind(json!([
                        {"method": "email_magic_link", "credential_handle": handle},
                        {"method": "oauth_google"},
                        {"method": "oauth_microsoft"},
                        {"method": "oauth_github"}
                    ]))
                    .execute(&mut **tx)
                    .await?;
                    sqlx::query(
                        "INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) \
                         VALUES ($1, $2, $3, 'owner', 'active')",
                    )
                    .bind(&membership_id)
                    .bind(&tenant_id)
                    .bind(&user_id)
                    .execute(&mut **tx)
                    .await?;
                    sqlx::query(
                        "INSERT INTO workspaces (id, tenant_id, name, settings) \
                         VALUES ($1, $2, 'Personal', '{}'::jsonb)",
                    )
                    .bind(&workspace_id)
                    .bind(&tenant_id)
                    .bind("Personal")
                    .execute(&mut **tx)
                    .await?;
                    sqlx::query(
                        "INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) \
                         VALUES ($1, $2, $3, $4, 'admin', 'active')",
                    )
                    .bind(&workspace_membership_id)
                    .bind(&tenant_id)
                    .bind(&workspace_id)
                    .bind(&user_id)
                    .execute(&mut **tx)
                    .await?;
                    let event_type = EventType::parse("workspace.created")?;
                    batch.emit(
                        EventDraft::new(
                            "workspace",
                            workspace_id.clone(),
                            1,
                            event_type,
                            identity.correlation_id,
                            identity.actor.clone(),
                        )
                        .with_workspace(workspace_id.clone())
                        .with_payload(json!({
                            "workspace_id": workspace_id,
                            "tenant_id": tenant_id,
                            "user_id": user_id,
                        })),
                    );
                    Ok(())
                })
            })
            .await?;
        Ok(OnboardedUser {
            user_id: user_for_event,
            tenant_id: tenant_for_event,
            workspace_id: workspace_for_event,
            email,
        })
    }

    /// Load effective settings for a workspace by merging the four layers.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn effective_for_workspace(
        &self,
        tenant_id: &str,
        workspace_id: &str,
        user_id: &str,
    ) -> Result<Value, IdentityError> {
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        let tenant: Value = sqlx::query_scalar("SELECT settings FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .fetch_one(&mut *tx)
            .await?;
        let workspace: Value =
            sqlx::query_scalar("SELECT settings FROM workspaces WHERE id = $1 AND tenant_id = $2")
                .bind(workspace_id)
                .bind(tenant_id)
                .fetch_one(&mut *tx)
                .await?;
        let user: Value = sqlx::query_scalar("SELECT settings FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(effective_settings(&tenant, &workspace, &user))
    }
}
