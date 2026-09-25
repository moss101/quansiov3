# OPS-001 evidence notes: enterprise identity and service principals

## What is proven, and against what

`crates/server/tests/enterprise_identity.rs` runs a real OpenID provider in the test over loopback
HTTP:
- discovery;
- a JWKS with an ES256 key and an RS256 key (`tests/fixtures/oidc-test-idp-rsa.der`, a throwaway
  key of this test IdP);
- the authorization-code flow with PKCE S256, with confidential-client authentication by HTTP
  Basic;
- signed id_tokens.

The browser half of the flow is a real HTTP client following the authorization URL to the IdP's
302. Every request runs as the application role `quansio_app`, with row-level security in
force on the new identity tables and on every table the commands touch.

| Acceptance / test | Test |
|---|---|
| SSO sandbox test; groups mapped server-side; client claims ignored | `sso_sign_in_maps_groups_server_side_and_ignores_client_claims`: the id_token carries `roles/role/tenant_role: owner/admin` and the complete request body names `owner` and a group. The resulting roles come only from the mapping rows; the highest wins per scope; removed groups drop roles; removing a mapping recomputes at once; `owner` is never mapped; RS256 and ES256 both verify; unmapped users land as `viewer` in the default workspace. |
| id_token verification fails closed | `id_tokens_that_do_not_verify_are_refused_and_issue_no_session`. Refused cases: another issuer; another audience; several audiences without `azp`; expired; signed by an unpublished key; `alg: none`; HMAC; an unknown kid even after a JWKS refetch; another request's nonce; `email_verified: false`; no email. None of these provisions an account or issues a session. The state is single-use, is bound to its redirect, and is consumed before the code exchange (a replay never reaches the IdP). Key rotation verifies after one JWKS refetch. An `http` issuer is refused unless loopback issuers are admitted, and a raw secret is refused as a custody handle. |
| SCIM deprovision test; "deprovisioned identity loses access promptly" | `scim_deprovision_revokes_access_promptly`. After an Entra-style `PATCH active "False"`, the next request with the user's access token is 401; the refresh token is refused; SSO sign-in is refused; live sessions reach 0; memberships are suspended; the `member.updated` event's actor is `service`. Also covered: re-activation; group removal drops the mapped role; `DELETE` ends access; the last owner cannot be deprovisioned; an owner's memberships survive group recomputation. SCIM is refused without a key, to a session token, and to a principal without `Provisioning`. Every SCIM change is audited. |
| Token revocation test | `token_and_session_revocation_take_effect_on_the_next_request`. The session list shows one current session; the user revokes their other session (401 on next use); another person's session cannot be revoked; `RevokeUserSessions` is admin-only and immediate; `UpdateMembership suspended` and `RemoveMember` revoke, and a refresh cannot revive them. Defense in depth: a membership or account suspended directly in the database refuses its unrevoked session on the next request, and restoring it restores access. |
| "Service identity scopes are explicit and auditable" | `service_principal_scopes_are_explicit_and_auditable`. Empty, unknown, `Trust` and repeated scopes are refused. The `qsk_` key is returned once; the command-log replay and the stored result never hold it; `key_hash` is `$argon2id$`. An in-scope command runs; an out-of-scope one is 403 before any owner sees it. A principal cannot mint principals or configure SSO, even holding Identity. Every other route refuses a principal key. Each principal command is in the audit chain as `{kind: service, id}`, applied or refused; `last_used_at` is stamped; a forged secret and a revoked key are 401 on the next request. |
| Account linking and SSO enforcement | `existing_accounts_link_only_with_consent_and_enforcement_admits_only_sso`. An existing account is linked only when its own bearer session completes SSO (the IdP alone, or another person's session, gets `CONFLICT_STATE`). A second subject for a linked address is refused. Enforcement revokes non-owners' non-SSO sessions and refuses their magic link; the owner keeps a break-glass sign-in; a new address is provisioned through SSO. |

## Real boundary

`a_real_idp_issues_id_tokens_the_verifier_accepts` reads `QUANSIO_TEST_OIDC_ISSUER`, `_CLIENT_ID`,
`_CLIENT_SECRET`, `_USERNAME` and `_PASSWORD` (a sandbox realm). With them it fetches the real
discovery document and JWKS, obtains a real id_token by the sandbox's password grant, verifies it
with the product verifier, and checks that a tampered token is refused. These variables were
not provided. The test printed `BLOCKED_EXTERNAL` (captured in `cargo-enterprise-identity.log`),
so the task is `BLOCKED_EXTERNAL` with the implementation complete.

## Found while reconciling (recorded, not hidden)

1. **APP-002 regression: a returning user's sign-in as `quansio_app` failed on
   `users_primary_email_key`.** Fixed on `fix/APP-002-returning-user-rls` (migration
   `0019_sign_in_lookup.sql`, id-only security-definer lookups) and merged before this task's
   code. See `evidence/APP-002/2026-09-24T16-08-02Z/`.
2. **APP-001: the command surface was not RLS-clean for `quansio_app`.** The command log and
   several reads ran with no tenant context. Handed to a separate session, which fixed it on
   `fix/APP-001-api-queries-under-rls` (merged to main, `6bce25e`).
3. **APP-002 regression: `InviteMember` for an account that already exists elsewhere, as
   `quansio_app`, failed on `users_primary_email_key`.** Fixed on
   `fix/APP-002-invite-existing-user-rls` (merged `691f44f`). See
   `evidence/APP-002/2026-09-25T08-34-26Z/`.

After 2 and 3 were merged, this suite moved every request, commands included, onto the
application role.

## Limitations

- **SAML.** Not implemented natively: SAML assertions need XML-DSig verification from a vetted
  library, a dependency outside the approved set. The connection carries `protocol` (only `oidc`
  today), so SAML is additive. Entra ID, Okta, Google, Ping, OneLogin and Keycloak all offer OIDC.
- **One SSO connection per tenant.**
- **Email-domain discovery.** It needs domain verification, so sign-in starts from the
  organization's tenant id.
- **Mapping authority.** Group-to-role mappings manage every scope they mention; scopes they do
  not mention keep what an administrator set.
