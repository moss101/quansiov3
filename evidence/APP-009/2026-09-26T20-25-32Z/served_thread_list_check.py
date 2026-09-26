"""APP-009 served check: GET /v1/threads over a workspace of 200 threads, 200 reads."""
import sys, time
sys.path.insert(0, "tests/performance")
import qa009_harness as h
with h.start("thrprobe", h.UNLIMITED) as s:
    c = s.client()
    for i in range(200):
        c.command("CreateThread", {"workspace_id": h.WORKSPACE, "title": f"t{i}"})
    samples = []
    for _ in range(200):
        t0 = time.perf_counter(); r = c.get("/v1/threads", workspace_id=h.WORKSPACE); samples.append((time.perf_counter() - t0) * 1000)
    print("threads", len(r["threads"]), h.summary(samples))
