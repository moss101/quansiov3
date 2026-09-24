# APP-002 regression fix: a returning user's sign-in under row-level security

**Found by:** OPS-001 reconciliation (2026-09-24), while writing enterprise sign-in tests that
run as the application role.

**Failing test:** `cargo test -p quansio-server --test identity
the_whole_sign_in_flow_works_as_the_application_role_under_rls`, extended to sign the same address
in a second time. Before the fix, at `8d7474d` (main):

```
assertion `left == right` failed: {"code":"VALIDATION_BOUNDS","correlation_id":"auth","message":"identity event store: event store database error: error returned from database: duplicate key value violates unique constraint \"users_primary_email_key\"","retryable":false}
  left: 400
 right: 200
```

**Cause:** `AuthStore::lookup_by_email` read `users` and `tenant_memberships` before any user or
tenant context was set.
- As `quansio_app` the policies hide both (`user_self_or_member`, `tenant_isolation`), so a
  returning account was "not found" and sign-in tried to onboard the address again.
- The development stack connects as the superuser, which bypasses RLS, so only the RLS test
  (fresh sign-in only) exercised the application role.

**Fix:** migration `0019_sign_in_lookup.sql` adds `identity_user_by_email()` and
`identity_sign_in_tenant()`: security definer, returning ids only. The lookup then sets the user
and tenant context and reads the rest under the ordinary policies. A suspended account is now
refused as `AUTH_INVALID_TOKEN` instead of failing on the duplicate insert.
