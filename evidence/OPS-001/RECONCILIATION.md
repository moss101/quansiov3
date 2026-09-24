# OPS-001 reconciliation: enterprise identity and service principals

**Canonical owner** (the task's `paths`): `crates/server/src/control/identity/enterprise/`. It is
part of the identity owner (APP-002), which owns `users`, `tenant_memberships`,
`workspace_memberships`, `sessions` and `service_principals`. Enterprise identity writes those
tables through this module. The API files call the owner and write only `commands`.

**Existing code (2026-09-24).** Classification: PARTIAL, a model with nothing using it.

- `enterprise.rs` holds in-memory structs: a group map, a `DirectoryUser` with a `Vec<Session>`,
  and a `ServicePrincipal::mint` that SHA-256-hashes a `sec_` string. No route, table, IdP, SCIM
  endpoint or principal authentication uses them.
- `tests/enterprise_identity.rs` exercises those structs only.
- Already real and reused here:
  - `sessions` rows are checked per request, so revocation takes effect on the next request;
  - refresh-family revocation;
  - ES256 access tokens;
  - PKCE state rows (`oauth_states`, limited to the three consumer providers);
  - member management with events and audit;
  - the hash-chained audit log;
  - the `service_principals` table (id, tenant_id, name, scopes, key_hash, status,
    last_used_at);
  - the `sp_` prefix.
- Gaps in shipped code:
  - `RemoveMember` and a suspension leave the user's sessions live;
  - `authenticate` does not look at membership or user status;
  - nothing authenticates a service principal;
  - `CreateServicePrincipal` and `RevokeServicePrincipal` are in the catalog with no handler.

**Design:**

- **SSO (OIDC; one connection per tenant).**
  - Connection settings: issuer, client id, an optional client-secret custody handle
    (resolved from the environment the way the model gateway resolves handles), groups claim,
    default workspace, `enforced`.
  - Discovery at `<issuer>/.well-known/openid-configuration`. The discovered `issuer` must equal
    the configured one. Discovery and JWKS are cached in memory, and JWKS is fetched again once
    when a `kid` is unknown (key rotation).
  - Authorization code with PKCE S256, a `state` and a `nonce`. They are stored hashed in
    `sso_states` behind the `auth_flow` gate and are single-use.
  - The id_token is verified with `ring`, no new JWT dependency:
    - RS256 or ES256 only (`none` and HMAC are refused);
    - signature against the JWKS key named by `kid`;
    - `iss` exact; `aud` contains the client id; `azp` when there are several audiences;
    - `exp`, `nbf`, `iat` within the configured skew; `nonce`;
    - a non-empty `sub`; an `email_verified` of false is refused.
  - Issuers must be `https`. Loopback `http` is allowed only by a configuration flag that
    defaults off, so a tenant-admin issuer URL cannot reach the server's loopback.
- **Roles come from the server-side mapping only.** `sso_group_mappings` maps an external group
  to a tenant role (admin|billing|member) and/or a workspace role.
  - If several groups match, the highest wins: tenant admin > billing > member; workspace
    admin > approver > editor > viewer.
  - `owner` is never mapped, and an owner's membership is never changed by a mapping.
  - Client-sent roles and any id_token claim other than the configured groups claim are
    ignored.
- **Linking.**
  - An (issuer, subject) pair already linked signs in as its user.
  - An unknown email is provisioned just in time.
  - A user this tenant provisioned (SCIM) links on first SSO.
  - A pre-existing account links only when the complete request carries that same user's
    bearer session, so the user consents; otherwise it is refused (`CONFLICT_STATE`).
  - SSO sessions are tenant-bound (`auth_method = "sso"`).
  - A suspended or removed membership cannot sign in.
- **Enforcement.** With `enforced`, a non-owner cannot get a session in the tenant by magic
  link or OAuth. Owners keep the other methods as break-glass. Turning enforcement on revokes
  existing non-SSO sessions of non-owners.
- **SCIM 2.0** (RFC 7643/7644 subset).
  - Routes:
    - `/scim/v2/ServiceProviderConfig`
    - `/scim/v2/Users` with GET (filters `userName eq`, `externalId eq`) and POST
    - `/scim/v2/Users/{id}` with GET, PUT, PATCH and DELETE
    - `/scim/v2/Groups` and `/scim/v2/Groups/{id}` with the same verbs
  - Authenticated by a service principal key holding the `Provisioning` scope.
  - `active=false` suspends the tenant membership and its workspace memberships and revokes
    every session of that user in the tenant, in one transaction with `member.*` events and
    audit. DELETE removes the membership. `active=true` restores it with roles recomputed.
  - Group membership changes recompute the member's roles from the mappings.
  - Groups are `DirectoryGroup` (`dgr_`, a new DOMAIN §1.1 prefix).
- **Service principals (workload identity).**
  - `CreateServicePrincipal {name, scopes}`: human session with manage_tenant.
    - Scopes are §14 command families or `Provisioning`: non-empty, no duplicates, `Trust` not
      grantable (approvals stay human).
    - Returns `qsk_<ULID>_<secret>` once; the stored command result omits it.
    - `key_hash` is an argon2id PHC string (DOMAIN §2). Authentication parses the id from the
      key, reads the row, and verifies on a blocking thread. A digest cache skips re-hashing
      only while the row's hash is unchanged.
  - `RevokeServicePrincipal` takes effect on the next request.
  - A principal is admitted only to:
    - `POST /v1/commands/<Name>` when the command's family is in its scopes;
    - SCIM, with `Provisioning`.
    Every other route refuses a `qsk_` bearer. A principal cannot manage principals.
  - Every principal-authenticated command is audited, applied or refused, with actor
    `{kind: service, id: sp_…}`. `last_used_at` is stamped.
- **Session revocation / device identity.**
  - `authenticate` also requires an active user and an active membership in the session's
    tenant, so every deprovisioning path takes effect on the next request.
  - `RemoveMember` and a tenant-scope suspension revoke the user's sessions in the tenant.
  - `RevokeUserSessions {user_id}` for tenant administrators.
  - `GET /v1/auth/sessions` lists the caller's sessions by client (desktop|web|cli), with the
    method and last use. `POST /v1/auth/sessions/{id}/revoke` revokes one.
- **SAML.** Not implemented natively. A SAML-only IdP needs XML-DSig verification from a vetted
  library, a new dependency not in the approved set. The connection model carries
  `protocol` (`oidc` today) so SAML is additive. Every major enterprise IdP (Entra ID, Okta,
  Google, Ping, OneLogin, Keycloak) also speaks OIDC. This is recorded as a limitation in the
  task notes.

**Persistent state (migration `0019_enterprise_identity.sql`, additive):**

- `sso_connections`
- `sso_group_mappings`
- `sso_states` (behind the auth_flow gate)
- `external_identities`
- `directory_groups`
- `directory_group_members`

All carry RLS: forced, tenant isolation, and the auth_flow gate where a lookup precedes the
tenant. Grants go to `quansio_app`.

**Contracts (additive):**

- DOMAIN §1.1: `DirectoryGroup | dgr_`.
- DOMAIN §2: the enterprise shapes, sign-in and session endpoints, SCIM routes, and principal
  and linking rules.
- DOMAIN §14: `ConfigureSso`, `SetGroupRoleMapping`, `RemoveGroupRoleMapping` and
  `RevokeUserSessions` in the Identity family; `/service-principals` and `/sso` read
  projections.
- Proto `ENTITY_PREFIX_DIRECTORY_GROUP = 52`, then the catalog and contracts are regenerated.

**Configuration:** `config/identity.yaml` covers:

- SSO state TTL, metadata cache, clock skew, HTTP timeout, loopback flag;
- argon2 parameters;
- ungrantable scopes;
- the SCIM page limit.

**Effects:**

- Outbound HTTPS to the tenant's configured IdP (discovery, JWKS, token), part of sign-in.
- No tool effects.

**Capabilities/approvals:** RBAC manage_tenant for configuration, principals and session
revocation.

**Real boundaries:**

- Deterministic tests run a real OIDC provider over loopback HTTP in the test (discovery,
  JWKS with ES256 and RS256 keys and rotation, authorization-code with PKCE, id_token signing).
- The live IdP proof is gated on `QUANSIO_TEST_OIDC_ISSUER`, `QUANSIO_TEST_OIDC_CLIENT_ID`,
  `QUANSIO_TEST_OIDC_CLIENT_SECRET`, `QUANSIO_TEST_OIDC_USERNAME` and
  `QUANSIO_TEST_OIDC_PASSWORD`. It covers discovery, JWKS, and a real id_token from the
  sandbox's password grant verified by the same verifier. Absent those variables it prints
  `BLOCKED_EXTERNAL`.

**Rollback/recovery:**

- The tables are additive.
- Disabling a connection (`status = disabled`) stops SSO.
- Revocations are durable rows, so a restart cannot resurrect a session.
- An argon2 hash is not reversible, so a lost key means rotation (revoke, then create).

## Plan

- [ ] migration 0019 + README inventory
- [ ] DOMAIN §1.1/§2/§14 + prefix in core/proto + regenerate catalog/contracts
- [ ] `config/identity.yaml` + loader
- [ ] enterprise module: jwt, oidc, mapping, scim, principals, sessions; replace the toy structs
- [ ] auth.rs: authenticate checks membership/user status; sign-in enforcement
- [ ] members.rs: removal/suspension revokes sessions
- [ ] API: SSO + sessions routes, SCIM router, principal resolution/gate, command handlers,
      read projections, one-time key never stored in the command result
- [ ] tests: in-test OIDC provider (ES256/RS256, rotation), id_token negatives, client-claim
      mapping, SCIM deprovision/reactivate/groups, token/session revocation, principals
      (scopes, argon2id, gate, audit, revocation), enforcement, RLS as `quansio_app`, live gated
- [ ] mutation checks, evidence, CI
