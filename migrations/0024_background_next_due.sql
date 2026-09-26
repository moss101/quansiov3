-- The background loop sleeps until its next due work instead of a fixed interval (CORE-008,
-- found by QA-009: a routine window waited for the next one-second pass, so dispatch took up to
-- a second against a 500 ms budget). `routines`, `durable_timers` and `routine_fires` are readable
-- only for the active tenant (0001), so, like `background_tenants()` (0016), this function returns
-- one timestamp and nothing else: the earliest routine window, timer, or routine-fire retry or
-- lease expiry across every tenant.
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
            UNION ALL
            SELECT MIN(next_attempt_at) FROM routine_fires WHERE status = 'pending'
            UNION ALL
            SELECT MIN(lease_until) FROM routine_fires WHERE status = 'running'
        ) AS due_work
    $$;
REVOKE ALL ON FUNCTION background_next_due() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION background_next_due() TO quansio_app;
