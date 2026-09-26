//! Identity, onboarding, workspaces and layered settings (APP-002).
//!
//! A fresh user is provisioned a personal tenant and a workspace without reading a
//! configuration file. Secrets remain `sec_` handles. Settings merge is deterministic
//! and most-restrictive for security controls. Sign-in and sessions live in [`auth`],
//! OAuth providers in [`oauth`], organisation SSO, SCIM and service principals in [`enterprise`].

pub mod auth;
pub mod enterprise;
pub mod members;
pub mod oauth;
pub mod settings;

use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_events::{EventDraft, EventError, EventStore, EventType};
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::control::schema;
use crate::runtime::state_machine::RuntimeIdentity;

pub use auth::{
    AuthConfig, AuthError, AuthStore, IssuedSession, SessionScope, SignedInUser, SigningKey,
};
pub use enterprise::{
    Acting, EnterpriseConfig, EnterpriseError, EnterpriseIdentity, PrincipalScope, ScimError,
    ScimReply, SsoSettings, SsoStart,
};
pub use members::{Invitation, MemberAdmin, MemberView, WorkspaceMembershipView};
pub use settings::{
    effective_settings, merge_settings, platform_defaults, settings_effects, validate_settings,
    SettingsEffect,
};

/// How a user proved their address at sign-up (DOMAIN.md §2 `auth_methods`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignUpMethod {
    /// Passwordless email link; nothing to keep beyond the method name.
    EmailMagicLink,
    /// OAuth sign-in; the provider's stable subject identifies the account.
    OAuth {
        /// `google` | `microsoft` | `github`.
        provider: String,
        /// The provider's subject claim.
        subject: String,
    },
    /// A stored credential referenced by a `sec_` handle (never the material itself).
    CredentialHandle(String),
}

impl SignUpMethod {
    fn as_json(&self) -> Result<Value, IdentityError> {
        Ok(match self {
            Self::EmailMagicLink => json!({"method": "email_magic_link"}),
            Self::OAuth { provider, subject } => {
                if !matches!(provider.as_str(), "google" | "microsoft" | "github") {
                    return Err(IdentityError::Schema(format!(
                        "unknown OAuth provider {provider}"
                    )));
                }
                json!({"method": format!("oauth_{provider}"), "subject": subject})
            }
            Self::CredentialHandle(handle) => {
                if !handle.starts_with("sec_") {
                    return Err(IdentityError::Schema(
                        "credential_handle must be a sec_ secret handle".to_string(),
                    ));
                }
                json!({"method": "email_magic_link", "credential_handle": handle})
            }
        })
    }
}

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
    /// The entity is not in this tenant.
    #[error("identity not found: {0}")]
    NotFound(String),
    /// The change contradicts the current state.
    #[error("identity conflict: {0}")]
    Conflict(String),
}

impl IdentityError {
    /// The DOMAIN.md §15 code this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Event(_) | Self::Database(_) => "INTERNAL",
            Self::Schema(_) => "VALIDATION_SCHEMA",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Conflict(_) => "CONFLICT_STATE",
        }
    }
}

/// Repository path of this module's canonical owner.
pub const IDENTITY_OWNER: &str = "crates/server/src/control/identity";

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
    /// Credential material is a `sec_` handle, never a raw secret.
    ///
    /// # Errors
    /// Returns [`IdentityError`] when the handle is not a `sec_` handle or the database
    /// refuses the write.
    pub async fn onboard_email(
        &self,
        email: &str,
        display_name: &str,
        credential_handle: &str,
    ) -> Result<OnboardedUser, IdentityError> {
        self.onboard(
            email,
            display_name,
            SignUpMethod::CredentialHandle(credential_handle.to_string()),
        )
        .await
    }

    /// Provision a personal tenant and workspace for a new user, with no config file.
    ///
    /// `auth_methods` records how the address was proven plus the other baseline methods
    /// the account may add; it never holds credential material.
    ///
    /// # Errors
    /// Returns [`IdentityError`] when the method is malformed or the database refuses the write.
    pub async fn onboard(
        &self,
        email: &str,
        display_name: &str,
        method: SignUpMethod,
    ) -> Result<OnboardedUser, IdentityError> {
        let signed_up_with = method.as_json()?;
        let mut auth_methods = vec![signed_up_with.clone()];
        for baseline in [
            "email_magic_link",
            "oauth_google",
            "oauth_microsoft",
            "oauth_github",
        ] {
            if signed_up_with["method"] != baseline {
                auth_methods.push(json!({"method": baseline}));
            }
        }
        let auth_methods = Value::Array(auth_methods);
        let mut generator = UlidGenerator::new();
        let user_id = CanonicalId::generate(Prefix::User, &mut generator).to_string();
        let tenant_id = CanonicalId::generate(Prefix::Tenant, &mut generator).to_string();
        let workspace_id = CanonicalId::generate(Prefix::Workspace, &mut generator).to_string();
        let root_node_id = CanonicalId::generate(Prefix::WorkNode, &mut generator).to_string();
        let membership_id = format!("tm_{}", &tenant_id[3..]);
        let workspace_membership_id = format!("wm_{}", &workspace_id[3..]);
        let identity = RuntimeIdentity::system(&tenant_id, "identity", {
            quansio_core::CorrelationId::generate(&mut generator)
        });
        let email = email.to_string();
        let display_name = display_name.to_string();
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
                    // The users policy admits an insert only for the acting user (row-level
                    // security, enforced for the application role): the new user is that actor.
                    schema::set_user_context(tx, &user_id)
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
                    .bind(&auth_methods)
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
                    insert_root_objective(tx, &tenant_id, &workspace_id, &user_id, &root_node_id)
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

    /// Create a workspace in `tenant_id` with `user_id` as its admin (§14 `CreateWorkspace`).
    ///
    /// # Errors
    /// Returns [`IdentityError`] when the caller is not an active tenant member or the write fails.
    pub async fn create_workspace(
        &self,
        tenant_id: &str,
        user_id: &str,
        name: &str,
    ) -> Result<String, IdentityError> {
        let name = name.trim();
        if name.is_empty() || name.len() > 120 {
            return Err(IdentityError::Schema(
                "workspace name must be 1..=120 characters".to_string(),
            ));
        }
        let mut generator = UlidGenerator::new();
        let workspace_id = CanonicalId::generate(Prefix::Workspace, &mut generator).to_string();
        let root_node_id = CanonicalId::generate(Prefix::WorkNode, &mut generator).to_string();
        let membership_id = format!("wm_{}", &workspace_id[3..]);
        let identity = RuntimeIdentity::system(tenant_id, "identity", {
            quansio_core::CorrelationId::generate(&mut generator)
        });
        let tenant = tenant_id.to_string();
        let user = user_id.to_string();
        let name = name.to_string();
        let workspace_for_result = workspace_id.clone();
        self.events
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let member: Option<(String,)> = sqlx::query_as(
                        "SELECT role FROM tenant_memberships WHERE tenant_id = $1 AND user_id = $2 AND status = 'active'",
                    )
                    .bind(&tenant)
                    .bind(&user)
                    .fetch_optional(&mut **tx)
                    .await?;
                    if member.is_none() {
                        return Err(EventError::MutationRejected {
                            owner: "crates/server/src/control/identity",
                            message: "the caller is not an active member of the tenant".to_string(),
                        });
                    }
                    sqlx::query(
                        "INSERT INTO workspaces (id, tenant_id, name, settings) VALUES ($1, $2, $3, '{}'::jsonb)",
                    )
                    .bind(&workspace_id)
                    .bind(&tenant)
                    .bind(&name)
                    .execute(&mut **tx)
                    .await?;
                    sqlx::query(
                        "INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) \
                         VALUES ($1, $2, $3, $4, 'admin', 'active')",
                    )
                    .bind(&membership_id)
                    .bind(&tenant)
                    .bind(&workspace_id)
                    .bind(&user)
                    .execute(&mut **tx)
                    .await?;
                    insert_root_objective(tx, &tenant, &workspace_id, &user, &root_node_id).await?;
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
                            "tenant_id": tenant,
                            "user_id": user,
                            "name": name,
                        })),
                    );
                    Ok(())
                })
            })
            .await?;
        Ok(workspace_for_result)
    }

    /// Update a workspace's name and/or settings (§14 `UpdateWorkspace`).
    ///
    /// Settings are validated first: provider credentials must be `sec_` handles, the execution
    /// target one of the supported kinds, the primary model a catalog id. Returns the new
    /// effective settings and the restart/reindex effects the change implies.
    ///
    /// # Errors
    /// Returns [`IdentityError::Schema`] for an invalid setting, or a database error.
    pub async fn update_workspace(
        &self,
        tenant_id: &str,
        user_id: &str,
        workspace_id: &str,
        name: Option<&str>,
        settings: Option<&Value>,
    ) -> Result<SettingsChange, IdentityError> {
        if let Some(settings) = settings {
            validate_settings(settings).map_err(IdentityError::Schema)?;
        }
        let before = self
            .effective_for_workspace(tenant_id, workspace_id, user_id)
            .await?;
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        let admin: Option<(String,)> = sqlx::query_as(
            "SELECT role FROM workspace_memberships WHERE workspace_id = $1 AND user_id = $2 AND status = 'active'",
        )
        .bind(workspace_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        match admin {
            Some((role,)) if role == "admin" => {}
            _ => {
                return Err(IdentityError::Schema(
                    "only a workspace admin may update the workspace".to_string(),
                ))
            }
        }
        if let Some(name) = name {
            let name = name.trim();
            if name.is_empty() || name.len() > 120 {
                return Err(IdentityError::Schema(
                    "workspace name must be 1..=120 characters".to_string(),
                ));
            }
            sqlx::query("UPDATE workspaces SET name = $3, updated_at = now() WHERE id = $1 AND tenant_id = $2")
                .bind(workspace_id)
                .bind(tenant_id)
                .bind(name)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(settings) = settings {
            sqlx::query(
                "UPDATE workspaces SET settings = settings || $3::jsonb, updated_at = now() \
                 WHERE id = $1 AND tenant_id = $2",
            )
            .bind(workspace_id)
            .bind(tenant_id)
            .bind(settings)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        let after = self
            .effective_for_workspace(tenant_id, workspace_id, user_id)
            .await?;
        let effects = settings_effects(&before, &after);
        Ok(SettingsChange {
            effective: after,
            effects,
        })
    }

    /// Update the caller's own settings layer (§14 `UpdateUserSettings`).
    ///
    /// # Errors
    /// Returns [`IdentityError::Schema`] for an invalid setting, or a database error.
    pub async fn update_user_settings(
        &self,
        tenant_id: &str,
        workspace_id: &str,
        user_id: &str,
        settings: &Value,
    ) -> Result<SettingsChange, IdentityError> {
        validate_settings(settings).map_err(IdentityError::Schema)?;
        let before = self
            .effective_for_workspace(tenant_id, workspace_id, user_id)
            .await?;
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        // The users policy admits an update only by the user itself (row-level security,
        // enforced for the application role), so the transaction acts as that user.
        schema::set_user_context(&mut tx, user_id)
            .await
            .map_err(|error| IdentityError::Schema(error.to_string()))?;
        sqlx::query(
            "UPDATE users SET settings = settings || $2::jsonb, updated_at = now() WHERE id = $1",
        )
        .bind(user_id)
        .bind(settings)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let after = self
            .effective_for_workspace(tenant_id, workspace_id, user_id)
            .await?;
        let effects = settings_effects(&before, &after);
        Ok(SettingsChange {
            effective: after,
            effects,
        })
    }
}

/// Every workspace starts with a root objective: the WorkGraph node direct conversation and
/// runs anchor to until the user shapes work of their own (DOMAIN.md §2, §3.1). Without it
/// a fresh workspace cannot take a message, which is not "usable".
async fn insert_root_objective(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    workspace_id: &str,
    user_id: &str,
    node_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO work_nodes (id, tenant_id, workspace_id, kind, title, description, created_by, origin) \
         VALUES ($1, $2, $3, 'objective', 'Inbox', 'Conversation and work that has not been given an objective of its own yet', \
                 $4, 'user')",
    )
    .bind(node_id)
    .bind(tenant_id)
    .bind(workspace_id)
    .bind(json!({ "kind": "user", "id": user_id }))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The outcome of a settings update: what is now in force, and what it costs to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsChange {
    /// The merged effective settings after the change.
    pub effective: Value,
    /// Restart/reindex consequences of the change, in a stable order.
    pub effects: Vec<SettingsEffect>,
}
