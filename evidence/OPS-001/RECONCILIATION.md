# OPS-001 reconciliation

- Canonical owner: `crates/server/src/control/identity/enterprise.rs`.
- Existing: APP-002 personal onboarding in `identity/mod.rs`; RBAC roles in `policy/rbac.rs`.
- Coverage: GENUINE_GAP for SSO mapping, SCIM deprovision, service principals.
- Persistent state: service_principals table already exists (CORE-001). This task
  is the mapping/deprovision/scope policy; live IdP is a real boundary.
- Contracts: WorkspaceRole; service principal scopes JSONB.
- Real boundary: `QUANSIO_TEST_OIDC_ISSUER`. Absent → BLOCKED_EXTERNAL.
- PASS forbidden while APP-001/APP-002 are BLOCKED_EXTERNAL.
