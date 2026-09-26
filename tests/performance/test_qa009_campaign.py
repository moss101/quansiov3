"""QA-009: the served product measured against DOSSIER §21.2 (`tests/performance/slos.yaml`).

Every number here is taken from outside the deployable (`qa009_harness`): real HTTP and WebSocket
clients against `quansio-server` on PostgreSQL, the object store and the search index, serving as a
least-privileged role. Each test records its measurement (`QUANSIO_PERF_OUT/<metric>.json` when set)
and fails when the §21.2 target is missed.

Environment: `QUANSIO_TEST_POSTGRES_URL`, `QUANSIO_TEST_SERVER_BIN`, `QUANSIO_TEST_SEED_BIN`,
`QUANSIO_TEST_MINIO_*` (see `qa009_harness`); `QUANSIO_TEST_PERF_SOAK_SECONDS` (default 600) sets the
soak length. Absent → the suite reports `BLOCKED_EXTERNAL` and skips.
"""

from __future__ import annotations

import base64
import json
import os
import random
import threading
import time
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest
import yaml

import qa009_harness as harness
from qa009_harness import (
    UNLIMITED,
    WORKSPACE,
    ApiFailure,
    Connection,
    ServerUnderLoad,
    WebSocket,
    meter,
    record,
    summary,
)

SLOS: dict[str, Any] = yaml.safe_load(
    (Path(__file__).parent / "slos.yaml").read_text()
)["slos"]
BLOCKED = harness.blocked_reason()
pytestmark = pytest.mark.skipif(BLOCKED is not None, reason=BLOCKED or "")

#: Clients held open at once in the latency phases: a busy team's desktops and the web app.
CLIENTS = 8
VOCABULARY = [
    "launch",
    "checklist",
    "evidence",
    "bundle",
    "migration",
    "rollback",
    "runbook",
    "invoice",
    "renewal",
    "contract",
    "audit",
    "latency",
    "budget",
    "quota",
    "connector",
    "webhook",
    "routine",
    "teammate",
    "approval",
    "preview",
    "artifact",
    "release",
    "incident",
    "postmortem",
    "roadmap",
    "onboarding",
    "security",
    "review",
    "retention",
    "backup",
    "restore",
    "pipeline",
]


def document(index: int) -> str:
    rng = random.Random(index)
    words = [rng.choice(VOCABULARY) for _ in range(300)]
    return f"# Note {index}\n\n" + " ".join(words) + f"\n\nmarker-{index}\n"


@pytest.fixture(scope="module")
def served() -> Iterator[ServerUnderLoad]:
    """A populated workspace: 200 threads, 40 of them with a conversation, 120 artifacts."""
    with harness.start("latency", UNLIMITED) as server:
        owner = server.client()
        threads = [
            owner.command(
                "CreateThread", {"workspace_id": WORKSPACE, "title": f"thread {index}"}
            )["result"]["thread_id"]
            for index in range(200)
        ]
        for thread in threads[:40]:
            owner.command(
                "PostMessage",
                {
                    "workspace_id": WORKSPACE,
                    "thread_id": thread,
                    "content": "Summarize the launch checklist",
                },
            )
        for index in range(120):
            owner.command(
                "CreateArtifact",
                {
                    "workspace_id": WORKSPACE,
                    "title": f"note-{index}.md",
                    "media_type": "text/markdown",
                    "bytes_base64": base64.b64encode(document(index).encode()).decode(),
                },
            )
        server.samples["threads"] = threads
        yield server


def test_api_reads_hold_the_p95_budget(served: ServerUnderLoad) -> None:
    thread = served.samples["threads"][0]
    reads: list[tuple[str, dict[str, str]]] = [
        ("/v1/me", {}),
        ("/v1/threads", {"workspace_id": WORKSPACE}),
        ("/v1/messages", {"thread_id": thread}),
        ("/v1/questions", {"thread_id": thread}),
        ("/v1/artifacts", {"workspace_id": WORKSPACE}),
        ("/v1/approvals", {"workspace_id": WORKSPACE}),
        ("/v1/workgraph", {"workspace_id": WORKSPACE}),
        ("/v1/effects", {"workspace_id": WORKSPACE}),
        ("/v1/knowledge", {"workspace_id": WORKSPACE}),
        ("/v1/memory", {"workspace_id": WORKSPACE}),
        ("/v1/skills", {"workspace_id": WORKSPACE}),
        ("/v1/members", {"workspace_id": WORKSPACE}),
        ("/v1/teammates", {"workspace_id": WORKSPACE}),
        ("/v1/routines", {"workspace_id": WORKSPACE}),
        ("/v1/notifications", {"workspace_id": WORKSPACE}),
        ("/v1/audit", {"workspace_id": WORKSPACE}),
        ("/v1/usage", {"workspace_id": WORKSPACE}),
        ("/v1/targets", {"workspace_id": WORKSPACE}),
    ]
    connections = [served.client() for _ in range(CLIENTS)]
    for (
        resource,
        query,
    ) in reads:  # warm every path once (and fail loudly on a broken one)
        connections[0].get(resource, **query)
    timings: dict[str, list[float]] = {resource: [] for resource, _ in reads}
    lock = threading.Lock()

    def read(connection: Connection, index: int) -> None:
        resource, query = reads[index % len(reads)]
        started = time.perf_counter()
        connection.get(resource, **query)
        elapsed = (time.perf_counter() - started) * 1000
        with lock:
            timings[resource].append(elapsed)

    failures = harness.run_concurrently(read, connections, 3600)
    everything = [sample for samples in timings.values() for sample in samples]
    result = record(
        "api_read_p95_ms",
        {
            "clients": CLIENTS,
            "overall": summary(everything),
            "by_resource": {
                resource: summary(samples) for resource, samples in timings.items()
            },
            "failures": [str(error) for error in failures[:5]],
            "target_ms": SLOS["api_read_p95_ms"]["target"],
        },
    )
    assert not failures, failures[:3]
    assert result["overall"]["p95_ms"] <= SLOS["api_read_p95_ms"]["target"], result[
        "by_resource"
    ]


def test_mutating_commands_are_accepted_inside_the_p95_budget(
    served: ServerUnderLoad,
) -> None:
    threads = served.samples["threads"]
    kinds = ("CreateThread", "CreateArtifact", "PostMessage")
    timings: dict[str, list[float]] = {kind: [] for kind in kinds}
    lock = threading.Lock()

    def invoke(connection: Connection, index: int) -> None:
        kind = kinds[index % len(kinds)]
        if kind == "CreateThread":
            params: dict[str, Any] = {
                "workspace_id": WORKSPACE,
                "title": f"load {index}",
            }
        elif kind == "CreateArtifact":
            params = {
                "workspace_id": WORKSPACE,
                "title": f"load-{index}.md",
                "media_type": "text/markdown",
                "bytes_base64": base64.b64encode(
                    document(10_000 + index).encode()
                ).decode(),
            }
        else:
            params = {
                "workspace_id": WORKSPACE,
                "thread_id": threads[100 + index % 100],
                "content": f"note {index}",
            }
        started = time.perf_counter()
        connection.command(kind, params)
        elapsed = (time.perf_counter() - started) * 1000
        with lock:
            timings[kind].append(elapsed)

    failures = harness.run_concurrently(
        invoke, [served.client() for _ in range(CLIENTS)], 900
    )
    everything = [sample for samples in timings.values() for sample in samples]
    result = record(
        "api_command_accepted_p95_ms",
        {
            "clients": CLIENTS,
            "settings": served.settings,
            "overall": summary(everything),
            "by_command": {kind: summary(samples) for kind, samples in timings.items()},
            "note": "PostMessage runs the turn inline; here the model is the in-server stub",
            "failures": [str(error) for error in failures[:5]],
            "target_ms": SLOS["api_command_accepted_p95_ms"]["target"],
        },
    )
    assert not failures, failures[:3]
    assert (
        result["overall"]["p95_ms"] <= SLOS["api_command_accepted_p95_ms"]["target"]
    ), result["by_command"]


def _event_lags(served: ServerUnderLoad, count: int) -> list[float]:
    stream = WebSocket(
        served.origin,
        served.token,
        {"subscribe": [{"channel": f"workspace:{WORKSPACE}"}]},
    )
    try:
        while stream.recv(0.5) is not None:  # drain the replay
            pass
        writer = served.client()
        lags: list[float] = []
        for index in range(count):
            thread = writer.command(
                "CreateThread", {"workspace_id": WORKSPACE, "title": f"lag {index}"}
            )
            committed = time.perf_counter()  # the response follows the commit
            thread_id = thread["result"]["thread_id"]
            while True:
                message = stream.recv(10)
                assert message is not None, f"no frame for {thread_id} within 10 s"
                frame = json.loads(message)
                event = frame.get("event") or {}
                if (
                    event.get("type") == "thread.created"
                    and event.get("aggregate_id") == thread_id
                ):
                    lags.append((time.perf_counter() - committed) * 1000)
                    break
            time.sleep(0.02)
        return lags
    finally:
        stream.close()


def test_events_reach_a_subscribed_client_inside_the_lag_budget(
    served: ServerUnderLoad,
) -> None:
    quiet = _event_lags(served, 150)
    # The same measurement while other people read (four clients reading as fast as they can).
    stop = threading.Event()

    def background(connection: Connection) -> None:
        while not stop.is_set():
            connection.get("/v1/threads", workspace_id=WORKSPACE)

    readers = [
        threading.Thread(target=background, args=(served.client(),), daemon=True)
        for _ in range(4)
    ]
    for reader in readers:
        reader.start()
    try:
        loaded = _event_lags(served, 150)
    finally:
        stop.set()
        for reader in readers:
            reader.join(timeout=10)
    result = record(
        "event_delivery_lag_p95_ms",
        {
            "transport": "WebSocket /v1/stream, channel workspace:<id>",
            "measured_from": "the HTTP response of the command (after its commit) to the frame's arrival",
            "quiet": summary(quiet),
            "under_read_load": summary(loaded),
            "target_ms": SLOS["event_delivery_lag_p95_ms"]["target"],
        },
    )
    assert (
        result["under_read_load"]["p95_ms"]
        <= SLOS["event_delivery_lag_p95_ms"]["target"]
    )
    assert result["quiet"]["p95_ms"] <= SLOS["event_delivery_lag_p95_ms"]["target"]


def test_search_answers_inside_the_index_budget(served: ServerUnderLoad) -> None:
    connections = [served.client() for _ in range(4)]
    connections[0].get(
        "/v1/search", q="launch", workspace_id=WORKSPACE
    )  # build the index once
    timings: list[float] = []
    lock = threading.Lock()

    def query(connection: Connection, index: int) -> None:
        rng = random.Random(index)
        term = rng.choice(VOCABULARY) if index % 5 else f"marker-{rng.randrange(120)}"
        started = time.perf_counter()
        connection.get("/v1/search", q=term, workspace_id=WORKSPACE, max_results="20")
        elapsed = (time.perf_counter() - started) * 1000
        with lock:
            timings.append(elapsed)

    failures = harness.run_concurrently(query, connections, 800)
    # Search right after a write: the first query after the corpus changes pays for the refresh.
    writer = served.client()
    after_write: list[float] = []
    for index in range(20):
        writer.command(
            "CreateArtifact",
            {
                "workspace_id": WORKSPACE,
                "title": f"fresh-{index}.md",
                "media_type": "text/markdown",
                "bytes_base64": base64.b64encode(
                    document(20_000 + index).encode()
                ).decode(),
            },
        )
        started = time.perf_counter()
        found = writer.get(
            "/v1/search", q=f"marker-{20_000 + index}", workspace_id=WORKSPACE
        )
        after_write.append((time.perf_counter() - started) * 1000)
        assert json.dumps(found).count(f"fresh-{index}.md"), (
            f"fresh-{index}.md not found right after it was written"
        )
    result = record(
        "index_query_p95_ms",
        {
            "corpus": "120 markdown artifacts (~2 KB) plus seeded knowledge and threads",
            "steady": summary(timings),
            "first_query_after_a_write": summary(after_write),
            "failures": [str(error) for error in failures[:5]],
            "target_ms": SLOS["index_query_p95_ms"]["target"],
        },
    )
    assert not failures, failures[:3]
    assert result["steady"]["p95_ms"] <= SLOS["index_query_p95_ms"]["target"]
    assert (
        result["first_query_after_a_write"]["p95_ms"]
        <= SLOS["index_query_p95_ms"]["target"]
    )


def test_deleted_knowledge_and_memory_leave_retrieval_inside_the_budget(
    served: ServerUnderLoad,
) -> None:
    owner = served.client()
    knowledge = owner.get("/v1/knowledge", workspace_id=WORKSPACE)
    entries = [
        entry
        for entry in knowledge["knowledge"]
        if entry.get("status") in ("active", "verified")
    ]
    assert entries, knowledge
    target = entries[0]
    term = harness.SEEDED_KNOWLEDGE_TERM
    assert target["id"] in json.dumps(
        owner.get("/v1/search", q=term, workspace_id=WORKSPACE, channels="knowledge")
    )
    started = time.monotonic()
    owner.command("DeleteKnowledge", {"knowledge_id": target["id"]})
    harness.wait_for(
        lambda: (
            target["id"]
            not in json.dumps(
                owner.get(
                    "/v1/search", q=term, workspace_id=WORKSPACE, channels="knowledge"
                )
            )
        ),
        "the deleted knowledge to leave search",
        timeout=SLOS["memory_knowledge_deletion_visible_s"]["target"],
    )
    knowledge_s = time.monotonic() - started
    memory = owner.get("/v1/memory", workspace_id=WORKSPACE)["memory"]
    assert memory, "the seed writes one memory"
    remembered = memory[0]["id"]
    started = time.monotonic()
    owner.command("DeleteMemory", {"memory_id": remembered})
    harness.wait_for(
        lambda: (
            remembered
            not in json.dumps(owner.get("/v1/memory", workspace_id=WORKSPACE))
        ),
        "the deleted memory to leave retrieval",
        timeout=SLOS["memory_knowledge_deletion_visible_s"]["target"],
    )
    memory_s = time.monotonic() - started
    result = record(
        "memory_knowledge_deletion_visible_s",
        {
            "knowledge_s": round(knowledge_s, 3),
            "memory_s": round(memory_s, 3),
            "target_s": SLOS["memory_knowledge_deletion_visible_s"]["target"],
        },
    )
    assert max(result["knowledge_s"], result["memory_s"]) <= result["target_s"]


def test_slow_stream_consumers_do_not_grow_server_memory(
    served: ServerUnderLoad,
) -> None:
    baseline = served.rss_mb()
    stalled = [
        WebSocket(
            served.origin,
            served.token,
            {"subscribe": [{"channel": f"workspace:{WORKSPACE}"}]},
            read=False,
        )
        for _ in range(16)
    ]
    samples: list[float] = []
    try:
        writers = [served.client() for _ in range(4)]

        def write(connection: Connection, index: int) -> None:
            connection.command(
                "CreateThread",
                {"workspace_id": WORKSPACE, "title": f"flood {index} " + "x" * 400},
            )
            if index % 100 == 0:
                samples.append(served.rss_mb())

        failures = harness.run_concurrently(write, writers, 3000)
        after = served.rss_mb()
        # A reading consumer still gets fresh events promptly while the stalled ones hold on.
        lags = _event_lags(served, 30)
    finally:
        for stream in stalled:
            stream.close()
    result = record(
        "overload_slow_consumers",
        {
            "stalled_consumers": 16,
            "events_written": 3000 * 2,
            "rss_mb": {
                "before": round(baseline, 1),
                "peak": round(max(samples + [after]), 1),
                "after": round(after, 1),
            },
            "reading_consumer_lag": summary(lags),
            "failures": [str(error) for error in failures[:5]],
        },
    )
    assert not failures, failures[:3]
    # Bounded: stalled consumers cost socket buffers, not a queue that grows with the backlog.
    assert result["rss_mb"]["peak"] <= baseline + 150, result
    assert (
        result["reading_consumer_lag"]["p95_ms"]
        <= SLOS["event_delivery_lag_p95_ms"]["target"]
    )


def test_overload_is_refused_and_bounded_at_shipped_defaults() -> None:
    with harness.start("overload") as server:  # every knob at its shipped default
        owner = server.client()
        accepted = refused = 0
        for index in range(200):
            try:
                owner.command(
                    "CreateThread",
                    {"workspace_id": WORKSPACE, "title": f"burst {index}"},
                )
                accepted += 1
            except ApiFailure as error:
                assert error.status == 429 and error.code == "RATE_LIMITED", error
                refused += 1
        # A read flood: far more clients than the database pool (8) has connections.
        baseline = server.rss_mb()
        stop = threading.Event()
        timings: list[float] = []
        errors: list[str] = []
        lock = threading.Lock()

        def flood() -> None:
            connection = server.client()
            while not stop.is_set():
                started = time.perf_counter()
                try:
                    connection.get("/v1/threads", workspace_id=WORKSPACE)
                except Exception as error:  # noqa: BLE001 - recorded below
                    with lock:
                        errors.append(str(error)[:200])
                    continue
                with lock:
                    timings.append((time.perf_counter() - started) * 1000)

        clients = [threading.Thread(target=flood, daemon=True) for _ in range(64)]
        for client in clients:
            client.start()
        rss: list[float] = []
        for _ in range(30):
            time.sleep(1)
            rss.append(server.rss_mb())
        stop.set()
        for client in clients:
            client.join(timeout=30)
        healthy = server.client().get("/v1/health")["status"]
        result = record(
            "overload_refusal_and_memory",
            {
                "command_burst": {
                    "sent": 200,
                    "accepted": accepted,
                    "refused_rate_limited": refused,
                },
                "read_flood": {
                    "clients": 64,
                    "seconds": 30,
                    "latency": summary(timings),
                    "errors": errors[:5],
                    "error_count": len(errors),
                },
                "rss_mb": {
                    "before": round(baseline, 1),
                    "first_10s_max": round(max(rss[:10]), 1),
                    "last_10s_max": round(max(rss[-10:]), 1),
                },
                "health_after": healthy,
            },
        )
        assert accepted <= 120 and refused >= 80, result["command_burst"]
        assert not errors, errors[:3]
        assert healthy == "ready"
        # No unbounded growth: once the flood is running, memory stays flat rather than climbing.
        assert (
            result["rss_mb"]["last_10s_max"]
            <= result["rss_mb"]["first_10s_max"] * 1.2 + 10
        ), result["rss_mb"]


def test_the_scheduler_dispatches_due_work_inside_the_budget() -> None:
    with harness.start("scheduler") as server:  # the shipped scheduler interval
        owner = server.client()
        for index in range(20):
            owner.command(
                "CreateRoutine",
                {
                    "workspace_id": WORKSPACE,
                    "title": f"every five seconds {index}",
                    "prompt": "Note the time",
                    "trigger": {"kind": "interval", "spec": "5s"},
                },
            )
        time.sleep(45)
        rows = server.sql(
            "SELECT extract(epoch FROM (r.started_at - f.window_at)) * 1000 "
            "FROM routine_fires f JOIN runs r ON r.id = f.run_id "
            "WHERE f.source = 'schedule' AND f.window_at IS NOT NULL AND r.started_at IS NOT NULL"
        )
        latencies = [float(row[0]) for row in rows]
        assert len(latencies) >= 100, (
            f"only {len(latencies)} scheduled fires dispatched"
        )
        result = record(
            "runtime_scheduling_latency_p95_ms",
            {
                "measured_from": "a routine fire's due window to its run's start (runs.started_at)",
                "routines": 20,
                "interval": "5s",
                "settings": server.settings or "shipped defaults",
                "dispatch": summary(latencies),
                "target_ms": SLOS["runtime_scheduling_latency_p95_ms"]["target"],
            },
        )
        assert (
            result["dispatch"]["p95_ms"]
            <= SLOS["runtime_scheduling_latency_p95_ms"]["target"]
        )


def test_a_soak_holds_latency_and_memory_steady() -> None:
    seconds = int(os.environ.get("QUANSIO_TEST_PERF_SOAK_SECONDS", "600"))
    with harness.start("soak", UNLIMITED) as server:
        owner = server.client()
        threads = [
            owner.command(
                "CreateThread", {"workspace_id": WORKSPACE, "title": f"soak {index}"}
            )["result"]["thread_id"]
            for index in range(20)
        ]
        windows: list[dict[str, Any]] = []
        stop = threading.Event()
        timings: list[float] = []
        errors: list[str] = []
        lock = threading.Lock()

        def user(seed: int) -> None:
            rng = random.Random(seed)
            connection = server.client()
            while not stop.is_set():
                started = time.perf_counter()
                try:
                    roll = rng.random()
                    if roll < 0.7:
                        connection.get("/v1/threads", workspace_id=WORKSPACE)
                    elif roll < 0.85:
                        connection.get("/v1/messages", thread_id=rng.choice(threads))
                    elif roll < 0.95:
                        connection.command(
                            "PostMessage",
                            {
                                "workspace_id": WORKSPACE,
                                "thread_id": rng.choice(threads),
                                "content": "status?",
                            },
                        )
                    else:
                        connection.get(
                            "/v1/search",
                            q=rng.choice(VOCABULARY),
                            workspace_id=WORKSPACE,
                        )
                except Exception as error:  # noqa: BLE001 - recorded below
                    with lock:
                        errors.append(str(error)[:200])
                with lock:
                    timings.append((time.perf_counter() - started) * 1000)
                time.sleep(
                    0.05
                )  # a person, not a benchmark loop: ~20 actions a second each

        users = [
            threading.Thread(target=user, args=(seed,), daemon=True)
            for seed in range(CLIENTS)
        ]
        for thread in users:
            thread.start()
        window = max(10, seconds // 10)
        for _ in range(seconds // window):
            time.sleep(window)
            with lock:
                taken, timings[:] = list(timings), []
            windows.append({"rss_mb": round(server.rss_mb(), 1), **summary(taken)})
        stop.set()
        for thread in users:
            thread.join(timeout=30)
        result = record(
            "soak",
            {
                "seconds": seconds,
                "clients": CLIENTS,
                "windows": windows,
                "errors": errors[:5],
                "error_count": len(errors),
            },
        )
        assert not errors, errors[:3]
        first, last = windows[1], windows[-1]  # the first window includes warm-up
        assert last["rss_mb"] <= first["rss_mb"] * 1.25 + 20, result["windows"]
        assert last["p95_ms"] <= max(first["p95_ms"] * 2, 50), result["windows"]
        assert (
            max(entry["p95_ms"] for entry in windows)
            <= SLOS["api_command_accepted_p95_ms"]["target"]
        ), windows


def test_usage_meters_storage_exactly_as_the_tenant_grows() -> None:
    with harness.start("usage", UNLIMITED) as server:
        owner = server.client()

        def store(count: int, offset: int) -> int:
            total = 0
            for index in range(count):
                content = document(30_000 + offset + index).encode() * (1 + index % 4)
                owner.command(
                    "CreateArtifact",
                    {
                        "workspace_id": WORKSPACE,
                        "title": f"usage-{offset + index}.md",
                        "media_type": "text/markdown",
                        "bytes_base64": base64.b64encode(content).decode(),
                    },
                )
                total += len(content)
            return total

        expected = store(30, 0)
        early = owner.get("/v1/usage", workspace_id=WORKSPACE)
        assert meter(early, "storage_bytes") == float(expected), early["meters"]
        # A busy workspace: well past twenty thousand events.
        failures = harness.run_concurrently(
            lambda connection, index: connection.command(
                "CreateThread", {"workspace_id": WORKSPACE, "title": f"busy {index}"}
            ),
            [server.client() for _ in range(CLIENTS)],
            11_000,
        )
        assert not failures, failures[:3]
        events = server.sql("SELECT count(*) FROM runtime_events")[0][0]
        expected += store(10, 100)
        started = time.perf_counter()
        late = owner.get("/v1/usage", workspace_id=WORKSPACE)
        read_ms = (time.perf_counter() - started) * 1000
        result = record(
            "usage_accuracy_storage",
            {
                "artifact_bytes_written": expected,
                "storage_bytes_metered_early": meter(early, "storage_bytes"),
                "storage_bytes_metered_late": meter(late, "storage_bytes"),
                "tenant_events": events,
                "usage_read_ms_at_that_size": round(read_ms, 1),
            },
        )
        assert meter(early, "storage_bytes") > 0, early
        assert result["storage_bytes_metered_late"] == float(expected), result
        assert read_ms <= SLOS["api_read_p95_ms"]["target"], result
