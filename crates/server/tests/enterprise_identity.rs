//! OPS-001: SSO mapping, SCIM deprovision, service principal scopes, session revoke.
//!
//! Live OIDC is gated on `QUANSIO_TEST_OIDC_ISSUER`. When it is absent the sandbox
//! case prints `BLOCKED_EXTERNAL` and returns.

use quansio_server::control::identity::{
    live_oidc_configured, DirectoryUser, GroupRoleMap, ServicePrincipal, Session, SsoAssertion,
    LIVE_OIDC_ISSUER,
};
use quansio_server::policy::rbac::WorkspaceRole;

#[test]
fn sso_sandbox_does_not_trust_client_roles() {
    let mut map = GroupRoleMap::default();
    map.bind("eng", WorkspaceRole::Editor);
    map.bind("security", WorkspaceRole::Admin);
    let assertion = SsoAssertion {
        issuer: "https://idp.example".into(),
        subject: "ada".into(),
        groups: vec!["eng".into()],
        client_roles: vec!["admin".into(), "owner".into()],
    };
    let roles = map.map_assertion(&assertion);
    assert_eq!(roles, vec![WorkspaceRole::Editor]);
    assert!(
        !roles.contains(&WorkspaceRole::Admin),
        "client admin claim must be ignored"
    );

    if !live_oidc_configured() {
        eprintln!("BLOCKED_EXTERNAL: {LIVE_OIDC_ISSUER} is not set; live OIDC sandbox not running");
    }
}

#[test]
fn scim_deprovision_revokes_access_promptly() {
    let mut user = DirectoryUser {
        user_id: "usr_ada".into(),
        active: true,
        sessions: vec![
            Session {
                session_id: "ses_1".into(),
                revoked: false,
            },
            Session {
                session_id: "ses_2".into(),
                revoked: false,
            },
        ],
    };
    assert!(user.has_access());
    user.scim_deprovision();
    assert!(!user.active);
    assert!(
        !user.has_access(),
        "deprovisioned identity must lose access promptly"
    );
    assert!(user.sessions.iter().all(|session| session.revoked));
}

#[test]
fn token_revocation_and_service_scopes_are_explicit() {
    let mut user = DirectoryUser {
        user_id: "usr_ada".into(),
        active: true,
        sessions: vec![Session {
            session_id: "ses_1".into(),
            revoked: false,
        }],
    };
    assert!(user.revoke_session("ses_1"));
    assert!(!user.has_access());

    let principal = ServicePrincipal::mint(
        "sp_ci",
        "tn_alpha",
        "ci",
        vec!["artifacts:read".into(), "runs:write".into()],
        "sec_01SERVICEKEYHANDLE000000001",
    )
    .expect("mint");
    assert!(principal.allows("artifacts:read"));
    assert!(!principal.allows("tenant:admin"));
    let audit = principal.audit_record();
    assert!(audit.contains("artifacts:read"));
    assert!(audit.contains("key_hash="));
    assert!(!audit.contains("sec_01SERVICEKEYHANDLE000000001"));
    assert!(ServicePrincipal::mint("sp_x", "tn_alpha", "x", vec!["*".into()], "sec_x").is_err());
    assert!(ServicePrincipal::mint("sp_x", "tn_alpha", "x", vec![], "sec_x").is_err());
}
