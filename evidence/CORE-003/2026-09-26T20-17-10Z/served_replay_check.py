import json, sys, time
sys.path.insert(0, "tests/performance")
import qa009_harness as h
with h.start("wsprobe", h.UNLIMITED) as s:
    c = s.client()
    for i in range(int(sys.argv[1])):
        c.command("CreateThread", {"workspace_id": h.WORKSPACE, "title": f"t{i}"})
    ws = h.WebSocket(s.origin, s.token, {"subscribe": [{"channel": f"workspace:{h.WORKSPACE}"}]})
    n = 0; t0 = time.time()
    try:
        while True:
            m = ws.recv(2.0)
            if m is None: break
            n += 1
        print("replayed", n, "frames in", round(time.time() - t0, 2), "s")
        for i in range(20):
            c.command("CreateThread", {"workspace_id": h.WORKSPACE, "title": f"live{i}"})
            m = ws.recv(5)
            print("live", i, (m or "")[:80]) if i < 2 else None
        print("ok after live")
    except Exception as e:
        print("ERROR after", n, "frames:", repr(e), round(time.time() - t0, 2), "s")
    print("\n".join(l[:300] for l in s.log_text().splitlines() if '"level":"info"' not in l and "magic" not in l)[-1500:])
