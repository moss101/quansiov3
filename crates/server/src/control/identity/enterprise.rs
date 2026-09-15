//! Enterprise identity: SSO mapping, SCIM deprovision, service principals, session revoke (OPS-001).
//!
//! Client-supplied roles are never trusted. External groups map through a server-side
//! table. Deprovisioning revokes sessions immediately. Service principal scopes are
//! explicit and auditable.

use std::collections::BTreeMap;
use std::env;

use sha2::{Digest, Sha256};

use crate::policy::rbac::WorkspaceRole;

/// Environment variable that enables a live OIDC sandbox run.
pub const LIVE_OIDC_ISSUER: &str = "QUANSIO_TEST_OIDC_ISSUER";

/// An IdP assertion. `roles` from the client are ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsoAssertion {
    /// Issuer.
    pub issuer: String,
    /// Subject.
    pub subject: String,
    /// Groups the IdP asserts.
    pub groups: Vec<String>,
    /// Roles the *client* claimed. Must not be used.
    pub client_roles: Vec<String>,
}

/// Server-side group → workspace role map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupRoleMap {
    inner: BTreeMap<String, WorkspaceRole>,
}

impl GroupRoleMap {
    /// Bind one IdP group to an internal role.
    pub fn bind(&mut self, group: impl Into<String>, role: WorkspaceRole) {
        self.inner.insert(group.into(), role);
    }

    /// Map assertion groups. Client roles are discarded.
    #[must_use]
    pub fn map_assertion(&self, assertion: &SsoAssertion) -> Vec<WorkspaceRole> {
        let _ignored = &assertion.client_roles;
        let mut roles: Vec<WorkspaceRole> = assertion
            .groups
            .iter()
            .filter_map(|group| self.inner.get(group).copied())
            .collect();
        roles.sort_by_key(|role| role.as_str().to_string());
        roles.dedup();
        roles
    }
}

/// Directory user as SCIM sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryUser {
    /// User id.
    pub user_id: String,
    /// Active in the directory.
    pub active: bool,
    /// Sessions currently issued.
    pub sessions: Vec<Session>,
}

/// An issued session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Session id.
    pub session_id: String,
    /// Revoked.
    pub revoked: bool,
}

impl DirectoryUser {
    /// SCIM deprovision: mark inactive and revoke every session.
    pub fn scim_deprovision(&mut self) {
        self.active = false;
        for session in &mut self.sessions {
            session.revoked = true;
        }
    }

    /// Whether the user can still present a live session.
    #[must_use]
    pub fn has_access(&self) -> bool {
        self.active && self.sessions.iter().any(|session| !session.revoked)
    }

    /// Revoke one session by id.
    pub fn revoke_session(&mut self, session_id: &str) -> bool {
        match self
            .sessions
            .iter_mut()
            .find(|session| session.session_id == session_id)
        {
            Some(session) => {
                session.revoked = true;
                true
            }
            None => false,
        }
    }
}

/// Service identity with explicit scopes. No wildcard, no implied admin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServicePrincipal {
    /// `sp_` id.
    pub id: String,
    /// Tenant.
    pub tenant_id: String,
    /// Name.
    pub name: String,
    /// Explicit scopes.
    pub scopes: Vec<String>,
    /// Hash of the key material, never the key.
    pub key_hash: String,
    /// Status.
    pub status: String,
}

impl ServicePrincipal {
    /// Mint a principal. Empty scopes are refused; `*` is refused.
    ///
    /// # Errors
    /// Returns a message when scopes are missing or a wildcard.
    pub fn mint(
        id: impl Into<String>,
        tenant_id: impl Into<String>,
        name: impl Into<String>,
        scopes: Vec<String>,
        key: &str,
    ) -> Result<Self, String> {
        if scopes.is_empty() {
            return Err("service identity scopes must be explicit".to_string());
        }
        if scopes.iter().any(|scope| scope == "*" || scope.is_empty()) {
            return Err("wildcard or empty scopes are not auditable".to_string());
        }
        if !key.starts_with("sec_") {
            return Err("service keys are sec_ handles, never raw material".to_string());
        }
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        Ok(Self {
            id: id.into(),
            tenant_id: tenant_id.into(),
            name: name.into(),
            scopes,
            key_hash: format!("{:x}", hasher.finalize()),
            status: "active".to_string(),
        })
    }

    /// Audit record: identity, scopes, key hash. Never the raw key.
    #[must_use]
    pub fn audit_record(&self) -> String {
        format!(
            "sp={} tenant={} scopes={} key_hash={}",
            self.id,
            self.tenant_id,
            self.scopes.join(","),
            self.key_hash
        )
    }

    /// Whether a requested scope is held.
    #[must_use]
    pub fn allows(&self, scope: &str) -> bool {
        self.status == "active" && self.scopes.iter().any(|held| held == scope)
    }
}

/// Live OIDC sandbox is available when the issuer env var is set.
#[must_use]
pub fn live_oidc_configured() -> bool {
    env::var(LIVE_OIDC_ISSUER)
        .ok()
        .is_some_and(|value| !value.is_empty())
}
