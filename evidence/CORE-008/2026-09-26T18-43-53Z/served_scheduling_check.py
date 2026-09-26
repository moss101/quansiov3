"""CORE-008 served check: twenty 5 s routines at shipped defaults; window to claim, run and start."""
import json, sys, time, os
sys.path.insert(0, "tests/performance")
import qa009_harness as h
overrides = json.loads(sys.argv[1])
with h.start("schedprobe", overrides) as s:
    c = s.client()
    for i in range(int(sys.argv[2])):
        c.command("CreateRoutine", {"workspace_id": h.WORKSPACE, "title": f"r{i}", "prompt": "Note the time", "trigger": {"kind": "interval", "spec": "5s"}})
    time.sleep(40)
    rows = s.sql("SELECT extract(epoch FROM (f.created_at - f.window_at))*1000, extract(epoch FROM (r.created_at - f.created_at))*1000, extract(epoch FROM (r.started_at - r.created_at))*1000, extract(epoch FROM (r.started_at - f.window_at))*1000 FROM routine_fires f JOIN runs r ON r.id=f.run_id WHERE f.source='schedule'")
    for i, name in enumerate(["window->claimed", "claimed->run_created", "run_created->started", "window->started"]):
        print(name, h.summary([float(r[i]) for r in rows]))
