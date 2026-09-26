-- The background loop sleeps until its next due work instead of a fixed interval (CORE-008,
-- found by QA-009: a routine window waited for the next one-second pass, so dispatch took up to
-- a second against a 500 ms budget). `routines` and `durable_timers` are readable
-- only for the active tenant (0001), so, like `background_tenants()` (0016), this function returns
-- one timestamp and nothing else: the earliest routine window or timer across every tenant.
-- Claimed routine fires are not the loop's: the fire executor runs them, and retries them on its
-- own cadence.
CREATE OR REPLACE FUNCTION background_next_due() RETURNS TIMESTAMPTZ
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public
    AS $$
        SELECT MIN(due) FROM (
            SELECT MIN(next_due_at) AS due FROM routines
             WHERE status = 'active' AND next_due_at IS NOT NULL
            UNION ALL
            SELECT MIN(due_at) FROM durable_timers WHERE status = 'scheduled'
            UNION ALL
            SELECT MIN(claimed_until) FROM durable_timers WHERE status = 'claimed'
        ) AS due_work
    $$;
REVOKE ALL ON FUNCTION background_next_due() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION background_next_due() TO quansio_app;
