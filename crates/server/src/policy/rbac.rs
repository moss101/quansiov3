//! Role-based access control for command and action families (DOMAIN.md §2).
//!
//! Roles are declared at two scopes: `WorkspaceMembership` holds
//! `admin | editor | approver | viewer`, and `TenantMembership` holds
//! `owner | admin | billing | member`. Tenant `owner`/`admin` inherit workspace `admin`
//! on every workspace; `billing` sees usage only. A missing role is a denial: there is
//! no default-permit role.

use core::fmt;

use serde::{Deserialize, Serialize};

/// A workspace-scope role (DOMAIN.md §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceRole {
    /// Manages members, policies, connectors, teammates and packs.
    Admin,
    /// Creates work and messages.
    Editor,
    /// Approves effects within the workspace.
    Approver,
    /// Reads only.
    Viewer,
}

impl WorkspaceRole {
    /// Every workspace role, most privileged first.
    pub const ALL: [Self; 4] = [Self::Admin, Self::Editor, Self::Approver, Self::Viewer];

    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Editor => "editor",
            Self::Approver => "approver",
            Self::Viewer => "viewer",
        }
    }

    /// Parse a canonical workspace role.
    ///
    /// # Errors
    /// Returns the rejected value when it is not a canonical role.
    pub fn parse(value: &str) -> Result<Self, String> {
        Self::ALL
            .iter()
            .copied()
            .find(|role| role.as_str() == value)
            .ok_or_else(|| value.to_string())
    }
}

impl fmt::Display for WorkspaceRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A tenant-scope role (DOMAIN.md §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TenantRole {
    /// Owns the tenant.
    Owner,
    /// Administers the tenant.
    Admin,
    /// Sees usage and billing figures only.
    Billing,
    /// An ordinary member.
    Member,
}

impl TenantRole {
    /// Every tenant role, most privileged first.
    pub const ALL: [Self; 4] = [Self::Owner, Self::Admin, Self::Billing, Self::Member];

    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Billing => "billing",
            Self::Member => "member",
        }
    }

    /// Parse a canonical tenant role.
    ///
    /// # Errors
    /// Returns the rejected value when it is not a canonical role.
    pub fn parse(value: &str) -> Result<Self, String> {
        Self::ALL
            .iter()
            .copied()
            .find(|role| role.as_str() == value)
            .ok_or_else(|| value.to_string())
    }

    /// Whether this tenant role inherits workspace `admin` everywhere (DOMAIN.md §2).
    #[must_use]
    pub const fn inherits_workspace_admin(self) -> bool {
        matches!(self, Self::Owner | Self::Admin)
    }
}

impl fmt::Display for TenantRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The roles one actor holds, resolved from memberships.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ActorRoles {
    /// Tenant-scope role, when the actor is a tenant member.
    pub tenant: Option<TenantRole>,
    /// Workspace-scope role, when the actor is a workspace member.
    pub workspace: Option<WorkspaceRole>,
}

impl ActorRoles {
    /// Build the roles an actor holds.
    #[must_use]
    pub const fn new(tenant: Option<TenantRole>, workspace: Option<WorkspaceRole>) -> Self {
        Self { tenant, workspace }
    }

    /// The effective workspace role, applying tenant `owner`/`admin` inheritance.
    #[must_use]
    pub fn effective_workspace_role(self) -> Option<WorkspaceRole> {
        if self
            .tenant
            .is_some_and(TenantRole::inherits_workspace_admin)
        {
            return Some(WorkspaceRole::Admin);
        }
        self.workspace
    }
}

/// The command/action families policy gates (DOMAIN.md §2, §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionFamily {
    /// Read workspace state.
    Read,
    /// Create work nodes, messages and plans.
    AuthorWork,
    /// Approve a pending effect.
    ApproveEffect,
    /// Manage members, policies, connectors, teammates and packs.
    ManageWorkspace,
    /// Execute a proposed effect of a given tier.
    ExecuteEffect {
        /// Consequence tier of the proposed effect.
        tier: u8,
    },
    /// Operator controls (kill switch, drain, freeze).
    AdminOps,
    /// Tenant administration (roles, members, tenant policy).
    ManageTenant,
    /// Read usage and billing figures.
    ViewUsage,
}

/// Why a role check refused an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RbacFailure {
    /// The actor holds no role at the required scope.
    NoRole,
    /// The actor's role is insufficient for the action family.
    InsufficientRole,
}

impl RbacFailure {
    /// The canonical reason string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoRole => "rbac_no_role",
            Self::InsufficientRole => "rbac_insufficient_role",
        }
    }
}

/// Whether `roles` may perform `action` (DOMAIN.md §2 role semantics).
///
/// # Errors
/// Returns the typed reason the action is refused; an unknown or missing role denies.
pub fn check(roles: ActorRoles, action: ActionFamily) -> Result<(), RbacFailure> {
    let workspace = roles.effective_workspace_role();
    match action {
        ActionFamily::ManageTenant => {
            if roles
                .tenant
                .is_some_and(TenantRole::inherits_workspace_admin)
            {
                Ok(())
            } else {
                Err(RbacFailure::InsufficientRole)
            }
        }
        ActionFamily::ViewUsage => {
            if roles.tenant.is_some_and(|role| role != TenantRole::Member) {
                Ok(())
            } else {
                Err(RbacFailure::InsufficientRole)
            }
        }
        ActionFamily::Read => match workspace {
            Some(_) => Ok(()),
            None => Err(RbacFailure::NoRole),
        },
        ActionFamily::AuthorWork => match workspace {
            Some(WorkspaceRole::Admin | WorkspaceRole::Editor) => Ok(()),
            Some(_) => Err(RbacFailure::InsufficientRole),
            None => Err(RbacFailure::NoRole),
        },
        ActionFamily::ApproveEffect => match workspace {
            Some(WorkspaceRole::Admin | WorkspaceRole::Approver) => Ok(()),
            Some(_) => Err(RbacFailure::InsufficientRole),
            None => Err(RbacFailure::NoRole),
        },
        ActionFamily::ManageWorkspace | ActionFamily::AdminOps => match workspace {
            Some(WorkspaceRole::Admin) => Ok(()),
            Some(_) => Err(RbacFailure::InsufficientRole),
            None => Err(RbacFailure::NoRole),
        },
        ActionFamily::ExecuteEffect { tier } => match workspace {
            Some(WorkspaceRole::Admin) => Ok(()),
            // Tier 3 and 4 effects are never executed on an editor's or viewer's role
            // alone: an approver (tier 3) or admin (tier 4) must be the actor.
            Some(WorkspaceRole::Approver) if tier <= 3 => Ok(()),
            Some(WorkspaceRole::Editor) if tier <= 2 => Ok(()),
            Some(_) => Err(RbacFailure::InsufficientRole),
            None => Err(RbacFailure::NoRole),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_names_round_trip() {
        for role in WorkspaceRole::ALL {
            assert_eq!(WorkspaceRole::parse(role.as_str()).expect("role"), role);
        }
        for role in TenantRole::ALL {
            assert_eq!(TenantRole::parse(role.as_str()).expect("role"), role);
        }
        assert!(WorkspaceRole::parse("root").is_err());
    }

    #[test]
    fn tenant_owner_and_admin_inherit_workspace_admin() {
        for tenant in [TenantRole::Owner, TenantRole::Admin] {
            let roles = ActorRoles::new(Some(tenant), None);
            assert_eq!(roles.effective_workspace_role(), Some(WorkspaceRole::Admin));
            assert!(check(roles, ActionFamily::ManageWorkspace).is_ok());
            assert!(check(roles, ActionFamily::AdminOps).is_ok());
        }
    }

    #[test]
    fn missing_role_denies_every_workspace_action() {
        let roles = ActorRoles::default();
        for action in [
            ActionFamily::Read,
            ActionFamily::AuthorWork,
            ActionFamily::ApproveEffect,
            ActionFamily::ManageWorkspace,
            ActionFamily::AdminOps,
            ActionFamily::ExecuteEffect { tier: 1 },
        ] {
            assert_eq!(check(roles, action), Err(RbacFailure::NoRole), "{action:?}");
        }
    }

    #[test]
    fn approver_approves_but_does_not_author_work() {
        let roles = ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Approver));
        assert!(check(roles, ActionFamily::ApproveEffect).is_ok());
        assert_eq!(
            check(roles, ActionFamily::AuthorWork),
            Err(RbacFailure::InsufficientRole)
        );
        assert!(check(roles, ActionFamily::ExecuteEffect { tier: 3 }).is_ok());
        assert_eq!(
            check(roles, ActionFamily::ExecuteEffect { tier: 4 }),
            Err(RbacFailure::InsufficientRole)
        );
    }

    #[test]
    fn viewer_only_reads_and_billing_only_views_usage() {
        let viewer = ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Viewer));
        assert!(check(viewer, ActionFamily::Read).is_ok());
        assert_eq!(
            check(viewer, ActionFamily::ExecuteEffect { tier: 0 }),
            Err(RbacFailure::InsufficientRole)
        );

        let billing = ActorRoles::new(Some(TenantRole::Billing), None);
        assert!(check(billing, ActionFamily::ViewUsage).is_ok());
        assert_eq!(
            check(billing, ActionFamily::ManageTenant),
            Err(RbacFailure::InsufficientRole)
        );
    }
}
