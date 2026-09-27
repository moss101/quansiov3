"""QA-009: the SLO configuration and the in-process retrieval floor.

The served measurements — load, soak, overload, backpressure and usage accuracy against the
deployable — are `test_qa009_campaign.py` and `test_qa009_live.py`. This suite pins
`slos.yaml` to DOSSIER.md §21.2 and times the in-process wiki retrieval and deletion.
"""

from __future__ import annotations

import importlib.util
import sys
import time
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
DOSSIER = ROOT / "DOSSIER.md"
SLOS = ROOT / "tests" / "performance" / "slos.yaml"


def _load_wiki():
    spec = importlib.util.spec_from_file_location(
        "qa009_wiki_navigate", ROOT / "packs" / "skills" / "wiki" / "navigate.py"
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def test_slo_configuration_matches_the_dossier() -> None:
    """Named test: the SLO table is DOSSIER §21.2 verbatim; drift is a failure."""
    config = yaml.safe_load(SLOS.read_text())
    assert config["ratified"] is False, "§21.2 is provisional until the owner ratifies"
    text = DOSSIER.read_text()
    section = text.split("### 21.2 Provisional SLOs", 1)[1].split("### 21.3", 1)[0]
    # The configuration cannot drift from the document: each entry either carries its
    # target verbatim or quotes the document fragment it normalizes.
    for name, entry in config["slos"].items():
        fragment = entry.get("dossier") or f"≤ {entry['target']} {entry['unit']}"
        assert fragment in section, f"{name}: {fragment!r} not in DOSSIER §21.2"
    # And the document's own row names are covered by the configuration.
    for phrase, expected_key in (
        ("API read", "api_read_p95_ms"),
        ("Event delivery lag", "event_delivery_lag_p95_ms"),
        ("Index query", "index_query_p95_ms"),
        ("deletion visible in retrieval", "memory_knowledge_deletion_visible_s"),
        ("RPO ≤ 5 min", "dr_rpo_min"),
        ("RTO ≤ 60 min", "dr_rto_min"),
    ):
        assert phrase in section, f"DOSSIER row {phrase!r} missing"
        assert expected_key in config["slos"]


def test_in_process_latencies_hold_the_index_and_deletion_budgets() -> None:
    """Named test: index query and deletion visibility against §21.2 budgets."""
    wiki = _load_wiki()
    pages = wiki.load_corpus()

    started = time.perf_counter()
    result = wiki.navigate("onboarding a teammate", pages=pages, budget=128)
    query_ms = (time.perf_counter() - started) * 1000.0
    assert result.hit_ids, "the benchmark query must retrieve"
    assert query_ms < 300.0, f"index query took {query_ms:.1f} ms (§21.2: ≤ 300 ms)"

    # Deletion visibility: a deleted page leaves retrieval on the next query, and the
    # whole navigation re-run (the in-process analogue of the derived-index refresh)
    # stays inside the 60 s visibility budget.
    started = time.perf_counter()
    mutated = dict(pages)
    deleted = mutated["kn_wiki_artifacts"]
    mutated["kn_wiki_artifacts"] = type(deleted)(
        entry=type(deleted.entry)(
            id=deleted.entry.id,
            tenant_id=deleted.entry.tenant_id,
            scope=deleted.entry.scope,
            kind=deleted.entry.kind,
            provenance=deleted.entry.provenance,
            content_ref=deleted.entry.content_ref,
            status=deleted.entry.status,
            confidence=deleted.entry.confidence,
            version=deleted.entry.version,
            superseded_by=deleted.entry.superseded_by,
            embedding_ref=deleted.entry.embedding_ref,
        )
        if False
        else deleted.entry,
        title=deleted.title,
        body=deleted.body,
        links=deleted.links,
    )
    # Mark it deleted through the shipped lifecycle edge.
    from intelligence.knowledge.models import KnowledgeStatus

    mutated["kn_wiki_artifacts"] = type(deleted)(
        entry=deleted.entry.with_status(KnowledgeStatus.DELETED),
        title=deleted.title,
        body=deleted.body,
        links=deleted.links,
    )
    after = wiki.navigate("versioned source files", pages=mutated, budget=128)
    visible_ms = (time.perf_counter() - started) * 1000.0
    assert "kn_wiki_artifacts" not in after.hit_ids, "a deleted page answered retrieval"
    assert visible_ms < 60_000.0, f"deletion visibility took {visible_ms:.1f} ms (§21.2: ≤ 60 s)"
