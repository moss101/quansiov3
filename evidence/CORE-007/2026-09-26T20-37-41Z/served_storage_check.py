"""CORE-007 served check: an uploaded artifact is an event, and its bytes are metered."""
import base64, json, sys, time
sys.path.insert(0, "tests/performance")
import qa009_harness as h
with h.start("storprobe") as s:
    c = s.client()
    body = b"quarterly figures " * 100
    c.command("CreateArtifact", {"workspace_id": h.WORKSPACE, "title": "q.md", "media_type": "text/markdown", "bytes_base64": base64.b64encode(body).decode()})
    u = c.get("/v1/usage", workspace_id=h.WORKSPACE)
    events = [row[0] for row in s.sql("SELECT type FROM runtime_events WHERE aggregate_type = 'artifact' ORDER BY sequence")]
    print("artifact events", events, "bytes written", len(body), "storage metered", h.meter(u, "storage_bytes"))
    assert events == ["artifact.created", "artifact.version_added"] and h.meter(u, "storage_bytes") == len(body)
