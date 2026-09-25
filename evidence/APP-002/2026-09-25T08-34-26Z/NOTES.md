# APP-002 regression fix: InviteMember for an existing account under row-level security

**Found by:** OPS-001 (2026-09-25), when its enterprise suite moved its commands onto the
application role after APP-001's API RLS fix.

**Failing test:** `cargo test -p quansio-server --test identity
the_whole_sign_in_flow_works_as_the_application_role_under_rls`, extended so the signed-in user
invites an account that already exists with its own personal tenant. On `main` at `24dc5fe`:

```
assertion `left == right` failed: {"code":"INTERNAL",...,"message":"identity event store: event store database error: error returned from database: duplicate key value violates unique constraint \"users_primary_email_key\"","retryable":true}
  left: 500
 right: 200
```

**Cause:** `IdentityStore::invite_member` looked the address up with `SELECT ... FROM users WHERE
primary_email = $1` under the inviter's user context. As `quansio_app`, the users policy hides
an account that is neither the inviter nor a member of this tenant, so the account was treated as
new and inserted again.

**Fix:** the id-only `identity_user_by_email()` lookup (migration 0019). The account's display
name is then read under its own user context, and the inviter's context is restored.

**Verification:** `QUANSIO_TEST_POSTGRES_URL` pointed at a private database on the canonical dev
server. The shared development database already carries an unreleased migration (OPS-001's 0020),
which a main-based compose test would refuse.
