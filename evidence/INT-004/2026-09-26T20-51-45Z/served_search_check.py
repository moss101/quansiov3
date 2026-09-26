"""INT-004 served check: a search right after each of 20 writes, over a corpus of 120 notes."""
import base64, sys, time
sys.path.insert(0, "tests/performance")
import qa009_harness as h
def note(i):
    return base64.b64encode((f"# Note {i}\n\n" + "launch checklist evidence " * 80 + f"\nmarker-{i}\n").encode()).decode()
with h.start("searchprobe", h.UNLIMITED) as s:
    c = s.client()
    for i in range(120):
        c.command("CreateArtifact", {"workspace_id": h.WORKSPACE, "title": f"n{i}.md", "media_type": "text/markdown", "bytes_base64": note(i)})
    c.get("/v1/search", q="launch", workspace_id=h.WORKSPACE)
    after = []
    for i in range(1000, 1020):
        c.command("CreateArtifact", {"workspace_id": h.WORKSPACE, "title": f"n{i}.md", "media_type": "text/markdown", "bytes_base64": note(i)})
        t0 = time.perf_counter(); r = c.get("/v1/search", q=f"marker-{i}", workspace_id=h.WORKSPACE); after.append((time.perf_counter() - t0) * 1000)
        assert f"n{i}.md" in str(r)
    print("first query after a write", h.summary(after))
