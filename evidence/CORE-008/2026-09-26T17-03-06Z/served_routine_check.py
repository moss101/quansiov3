"""CORE-008 served check: a routine created with the default absence policy fires on schedule."""
import json, sys, time
sys.path.insert(0, "tests/performance")
import qa009_harness as h
with h.start("core008") as s:
    c = s.client()
    r = c.command("CreateRoutine", {"workspace_id": h.WORKSPACE, "title": "every 5 s", "prompt": "Note the time", "trigger": {"kind": "interval", "spec": "5s"}})["result"]
    print("routine", r["id"], "absence_policy", r["absence_policy"], "next_due_at", r["next_due_at"])
    time.sleep(22)
    rows = s.sql("SELECT f.source, f.status, f.window_at, r.started_at FROM routine_fires f LEFT JOIN runs r ON r.id = f.run_id ORDER BY f.window_at")
    for row in rows:
        print("fire", row[0], row[1], row[2].isoformat(), "started", row[3].isoformat() if row[3] else None)
    skipped = s.sql("SELECT count(*) FROM runtime_events WHERE type = 'routine.skipped'")[0][0]
    print("fires", len(rows), "skipped_events", skipped)
    assert len(rows) >= 3 and all(row[1] == "fired" for row in rows) and skipped == 0
