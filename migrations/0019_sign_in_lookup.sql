-- Sign-in lookup under row-level security (APP-002 fix). `users` is visible only to the user
-- itself or through a membership in the active tenant, and `tenant_memberships` only in the
-- active tenant (0001). A returning user's sign-in happens before either context exists, so as
-- the application role the account was invisible and sign-in tried to onboard it a second time.
-- These return ids and nothing else: the session authority then sets the user and tenant
-- context and reads the rest under the ordinary policies.

-- The account for an address, whatever its tenants.
CREATE OR REPLACE FUNCTION identity_user_by_email(p_email TEXT) RETURNS TEXT
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public
    AS $$ SELECT id FROM users WHERE primary_email = p_email $$;
REVOKE ALL ON FUNCTION identity_user_by_email(TEXT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION identity_user_by_email(TEXT) TO quansio_app;

-- The tenant a sign-in lands in: the personal tenant first, else the earliest active or invited
-- membership (signing in accepts an invitation).
CREATE OR REPLACE FUNCTION identity_sign_in_tenant(p_user TEXT) RETURNS TEXT
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public
    AS $$
        SELECT t.id FROM tenant_memberships m JOIN tenants t ON t.id = m.tenant_id
        WHERE m.user_id = p_user AND m.status IN ('active', 'invited')
        ORDER BY t.personal DESC, m.status ASC, m.created_at ASC LIMIT 1
    $$;
REVOKE ALL ON FUNCTION identity_sign_in_tenant(TEXT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION identity_sign_in_tenant(TEXT) TO quansio_app;
