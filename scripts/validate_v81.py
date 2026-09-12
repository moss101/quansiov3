#!/usr/bin/env python3
"""Quansio V8.1 authority validator / generator.

Usage:
  python scripts/validate_v81.py            validate the authority set (exit 1 on any error)
  python scripts/validate_v81.py --write    regenerate TASKS.md, registries/task-graph.json, MANIFEST.json
  python scripts/validate_v81.py --ready    list dependency-ready tasks (per DOSSIER §20 readiness rule)
  python scripts/validate_v81.py --next     print the single next task to claim (deterministic)

Python 3.9+ ; standard library only.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
from collections import defaultdict, deque
from pathlib import Path
from typing import Dict, List, Optional

ROOT = Path(__file__).resolve().parents[1]

# Only these files are covered by MANIFEST.json. Product code is NOT part of the manifest.
AUTHORITY_FILES = [
    "DOSSIER.md",
    "DOMAIN.md",
    "AGENTS.md",
    "IMPLEMENTATION_MASTER_PROMPT.md",
    "TASKS.md",
    "registries/tasks.json",
    "registries/task-graph.json",
    "registries/progress.json",
    "scripts/validate_v81.py",
]

REQUIRED_TASK_FIELDS = {
    "id", "milestone", "title", "owner", "language", "component", "depends_on",
    "goal", "build", "acceptance", "tests", "real_boundary", "ga_required", "paths",
}

PROGRESS_FIELDS = {
    "coverage", "status", "claimed_by", "started_at", "updated_at", "git_commit",
    "tests", "artifacts", "real_boundary_evidence", "implementation_complete", "blocker", "notes",
}
RBE_FIELDS = {"boundary", "environment", "command", "artifact", "sha256", "recorded_at"}

DOSSIER_NEEDLES = [
    "Rust — trusted authority and execution plane",
    "Python — intelligence plane",
    "Memory is not recovery",
    "Universal Effect Ledger",
    "Business Capability Packs",
    "## 23. Open decisions",
    "## 24. Decision log",
    "Support matrix",
    "Provisional SLOs",
]
DOMAIN_NEEDLES = [
    "## 0. Glossary",
    "## 1. Identity scheme",
    "## 5. Runtime model",
    "## 6. Capability model",
    "## 7. Effects, policy and approvals",
    "## 9. Events and streaming",
    "## 12. Content trust and injection defense",
    "## 14. Public command catalog",
    "## 15. Error taxonomy",
]

TASK_ID_RE = re.compile(r"^[A-Z]{2,4}-\d{3}$")
COMMIT_RE = re.compile(r"^[0-9a-f]{7,40}$")
ISO_RE = re.compile(r"^\d{4}-\d{2}-\d{2}(T\d{2}:\d{2}:\d{2}(\.\d+)?Z)?$")
DECISION_RE = re.compile(r"^- \*\*D-(\d{3}):\*\*", re.M)

STARTED_STATUSES = {"RECONCILING", "IN_PROGRESS", "BLOCKED_EXTERNAL", "BLOCKED_CONFLICT", "PASS", "FAIL"}


def load_json(rel: str):
    return json.loads((ROOT / rel).read_text())


# --------------------------------------------------------------------------- generators
def render_tasks(reg) -> str:
    names = {m["id"]: m for m in reg["milestones"]}
    groups = defaultdict(list)
    for t in reg["tasks"]:
        groups[t["milestone"]].append(t)
    lines = [
        "# QUANSIO V8.1 — GENERATED TASK CATALOG", "",
        "> Generated from `registries/tasks.json` by `scripts/validate_v81.py --write`. Do not hand edit.", "",
        f"**Revision:** {reg.get('revision', 1)}  ",
        f"**Task count:** {len(reg['tasks'])}  ",
        f"**GA-required:** {sum(1 for t in reg['tasks'] if t['ga_required'])}  ",
        f"**Real-boundary:** {sum(1 for t in reg['tasks'] if t['real_boundary'])}", "",
    ]
    for mid, m in names.items():
        lines += [f"## {mid} — {m['title']}", "", f"**Exit criteria:** {m['exit_criteria']}", ""]
        for t in groups[mid]:
            lines += [
                f"### {t['id']} — {t['title']}",
                f"- **Owner:** {t['owner']}",
                f"- **Language:** {t['language']}",
                f"- **Component:** `{t['component']}`",
                f"- **Paths:** {', '.join('`'+p+'`' for p in t['paths'])}",
                f"- **Depends on:** {', '.join(t['depends_on']) if t['depends_on'] else 'none'}",
                f"- **Real boundary:** {'required' if t['real_boundary'] else 'not required'}",
                f"- **GA:** {'required' if t['ga_required'] else 'deferrable'}",
                f"- **Goal:** {t['goal']}",
                "",
                "**Build**",
            ]
            lines += [f"- {x}" for x in t["build"]]
            lines += ["", "**Acceptance**"] + [f"- {x}" for x in t["acceptance"]]
            lines += ["", "**Required tests/evidence**"] + [f"- {x}" for x in t["tests"]]
            lines += [""]
    return "\n".join(lines)


def expected_graph(reg):
    return {
        "version": reg["version"],
        "revision": reg.get("revision", 1),
        "nodes": [t["id"] for t in reg["tasks"]],
        "edges": [{"from": d, "to": t["id"]} for t in reg["tasks"] for d in t["depends_on"]],
    }


def build_manifest():
    entries = []
    for rel in AUTHORITY_FILES:
        p = ROOT / rel
        if rel == "MANIFEST.json" or not p.exists():
            continue
        data = p.read_bytes()
        entries.append({"path": rel, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})
    return {"version": "8.1", "algorithm": "sha256", "files": entries}


# --------------------------------------------------------------------------- readiness
def is_ready(tid: str, tasks: Dict[str, dict], prog: Dict[str, dict]) -> bool:
    """DOSSIER §20: every dependency PASS, or BLOCKED_EXTERNAL with implementation_complete."""
    for d in tasks[tid]["depends_on"]:
        p = prog.get(d, {})
        if p.get("status") == "PASS":
            continue
        if p.get("status") == "BLOCKED_EXTERNAL" and p.get("implementation_complete") is True:
            continue
        return False
    return True


def ready_tasks(reg, prog) -> List[str]:
    tasks = {t["id"]: t for t in reg["tasks"]}
    p = prog["tasks"]
    return [tid for tid in tasks if p[tid]["status"] in {"NOT_STARTED", "FAIL"} and is_ready(tid, tasks, p)]


def next_task(reg, prog) -> Optional[str]:
    """Deterministic: lowest milestone, then most transitive dependents, then lowest id."""
    tasks = {t["id"]: t for t in reg["tasks"]}
    mi = {m["id"]: i for i, m in enumerate(reg["milestones"])}
    children = defaultdict(set)
    for t in reg["tasks"]:
        for d in t["depends_on"]:
            children[d].add(t["id"])

    def downstream(tid):
        seen, q = set(), deque([tid])
        while q:
            n = q.popleft()
            for c in children[n]:
                if c not in seen:
                    seen.add(c); q.append(c)
        return len(seen)

    ready = ready_tasks(reg, prog)
    if not ready:
        return None
    return sorted(ready, key=lambda x: (mi[tasks[x]["milestone"]], -downstream(x), x))[0]


# --------------------------------------------------------------------------- validation
def validate(write: bool = False) -> int:
    errors: List[str] = []
    reg = load_json("registries/tasks.json")
    if reg.get("version") != "8.1":
        errors.append("tasks.json version must be 8.1")

    # milestones
    ms = [m["id"] for m in reg["milestones"]]
    mi = {m: i for i, m in enumerate(ms)}
    if len(ms) != len(set(ms)):
        errors.append("duplicate milestone ids")
    for m in reg["milestones"]:
        if not m.get("exit_criteria"):
            errors.append(f"milestone {m.get('id')}: missing exit_criteria")

    # task fields
    ids: List[str] = []
    for t in reg["tasks"]:
        tid = t.get("id", "?")
        missing = REQUIRED_TASK_FIELDS - set(t)
        if missing:
            errors.append(f"{tid}: missing fields {sorted(missing)}")
            continue
        if not TASK_ID_RE.match(tid):
            errors.append(f"{tid}: id must match AAA-000")
        ids.append(tid)
        if t["milestone"] not in mi:
            errors.append(f"{tid}: unknown milestone")
        for f in ("build", "acceptance", "tests", "paths"):
            if not isinstance(t[f], list) or not t[f] or not all(isinstance(x, str) and x.strip() for x in t[f]):
                errors.append(f"{tid}: {f} must be a non-empty list of strings")
        for f in ("real_boundary", "ga_required"):
            if not isinstance(t[f], bool):
                errors.append(f"{tid}: {f} must be boolean")
        if not isinstance(t["depends_on"], list) or len(t["depends_on"]) != len(set(t["depends_on"])):
            errors.append(f"{tid}: depends_on must be a list without duplicates")
        if tid in t["depends_on"]:
            errors.append(f"{tid}: depends on itself")
    if len(ids) != len(set(ids)):
        errors.append("duplicate task IDs")
    idset = set(ids)
    tasks = {t["id"]: t for t in reg["tasks"] if t.get("id") in idset}

    for t in reg["tasks"]:
        for d in t["depends_on"]:
            if d not in idset:
                errors.append(f"{t['id']}: unknown dependency {d}")
            elif mi.get(tasks[d]["milestone"], 0) > mi.get(t["milestone"], 0):
                errors.append(f"{t['id']}: depends on later milestone {d}")
        if not t.get("ga_required"):
            for d in t["depends_on"]:
                pass  # deferrable tasks may depend on anything
        else:
            for d in t["depends_on"]:
                if d in tasks and not tasks[d]["ga_required"]:
                    errors.append(f"{t['id']}: GA-required task depends on deferrable task {d}")

    # acyclic
    indeg = {i: 0 for i in ids}
    adj = defaultdict(list)
    for t in reg["tasks"]:
        for d in t["depends_on"]:
            if d in idset:
                adj[d].append(t["id"]); indeg[t["id"]] += 1
    q = deque([i for i, v in indeg.items() if v == 0]); seen = []
    while q:
        n = q.popleft(); seen.append(n)
        for v in adj[n]:
            indeg[v] -= 1
            if indeg[v] == 0:
                q.append(v)
    if len(seen) != len(ids):
        errors.append("task dependency graph contains a cycle")

    # terminal task must be reachable from everything GA-required (REL-006 is the sink)
    if "REL-006" not in idset:
        errors.append("REL-006 must exist as the release sink task")

    # generated graph
    exp_graph = expected_graph(reg)
    gp = ROOT / "registries/task-graph.json"
    if write:
        gp.write_text(json.dumps(exp_graph, indent=2) + "\n")
    elif not gp.exists() or load_json("registries/task-graph.json") != exp_graph:
        errors.append("task-graph.json is stale (run --write)")

    # progress
    prog = load_json("registries/progress.json")
    ptasks = prog.get("tasks", {})
    if set(ptasks) != idset:
        errors.append("progress.json task set does not exactly match tasks.json")
    allowed_status = set(prog.get("allowed_status", []))
    allowed_cov = set(prog.get("allowed_coverage", []))
    if not ISO_RE.match(str(prog.get("updated_at", ""))):
        errors.append("progress.json updated_at must be ISO date")

    for tid, p in ptasks.items():
        if tid not in tasks:
            continue
        task = tasks[tid]
        missing = PROGRESS_FIELDS - set(p)
        if missing:
            errors.append(f"{tid}: progress missing fields {sorted(missing)}")
            continue
        st = p["status"]
        if st not in allowed_status:
            errors.append(f"{tid}: invalid progress status {st!r}")
            continue
        if p["coverage"] is not None and p["coverage"] not in allowed_cov:
            errors.append(f"{tid}: invalid coverage {p['coverage']!r}")
        if not isinstance(p["implementation_complete"], bool):
            errors.append(f"{tid}: implementation_complete must be boolean")
        for f in ("tests", "artifacts", "real_boundary_evidence"):
            if not isinstance(p[f], list):
                errors.append(f"{tid}: {f} must be a list")
        if p["git_commit"] is not None and not COMMIT_RE.match(str(p["git_commit"])):
            errors.append(f"{tid}: git_commit must be 7-40 hex chars")
        for f in ("started_at", "updated_at"):
            if p[f] is not None and not ISO_RE.match(str(p[f])):
                errors.append(f"{tid}: {f} must be ISO-8601 UTC")
        for e in p["real_boundary_evidence"]:
            if not isinstance(e, dict) or (RBE_FIELDS - set(e)):
                errors.append(f"{tid}: real_boundary_evidence entries need {sorted(RBE_FIELDS)}")

        if st == "NOT_STARTED":
            if p["git_commit"] or p["claimed_by"] or p["implementation_complete"]:
                errors.append(f"{tid}: NOT_STARTED must have no claim/commit/implementation_complete")
            continue

        if st in STARTED_STATUSES:
            if not p["claimed_by"]:
                errors.append(f"{tid}: {st} requires claimed_by")
            if not p["started_at"] or not p["updated_at"]:
                errors.append(f"{tid}: {st} requires started_at and updated_at")
        if st in {"IN_PROGRESS", "BLOCKED_EXTERNAL", "BLOCKED_CONFLICT", "PASS", "FAIL"} and p["coverage"] is None:
            errors.append(f"{tid}: {st} requires coverage classification")
        if st.startswith("BLOCKED") and not p["blocker"]:
            errors.append(f"{tid}: {st} requires a blocker description")
        if st == "FAIL" and not p["notes"]:
            errors.append(f"{tid}: FAIL requires notes naming the failing test")
        if st == "DEFERRED_NON_GA" and task["ga_required"]:
            errors.append(f"{tid}: DEFERRED_NON_GA not allowed for GA-required task")

        # readiness: started tasks need ready deps
        if st in {"RECONCILING", "IN_PROGRESS", "PASS", "FAIL", "BLOCKED_EXTERNAL", "BLOCKED_CONFLICT"}:
            if not is_ready(tid, tasks, ptasks):
                errors.append(f"{tid}: {st} but dependencies are not ready (PASS or BLOCKED_EXTERNAL+implementation_complete)")
        if st == "PASS":
            if not p["git_commit"]:
                errors.append(f"{tid}: PASS missing git_commit")
            if not p["tests"]:
                errors.append(f"{tid}: PASS missing tests")
            if not p["artifacts"]:
                errors.append(f"{tid}: PASS missing evidence artifacts")
            if p["implementation_complete"] is not True:
                errors.append(f"{tid}: PASS requires implementation_complete=true")
            if task["real_boundary"] and not p["real_boundary_evidence"]:
                errors.append(f"{tid}: PASS missing real-boundary evidence")
            for d in task["depends_on"]:
                if ptasks.get(d, {}).get("status") != "PASS":
                    errors.append(f"{tid}: PASS but dependency {d} is not PASS")
        # a task depending on a deferred task must itself be deferred
        for d in task["depends_on"]:
            if ptasks.get(d, {}).get("status") == "DEFERRED_NON_GA" and st not in {"DEFERRED_NON_GA", "NOT_STARTED"}:
                errors.append(f"{tid}: depends on deferred task {d} but is {st}")

    # generated task view
    rendered = render_tasks(reg)
    tp = ROOT / "TASKS.md"
    if write:
        tp.write_text(rendered + "\n")
    elif not tp.exists() or tp.read_text() != rendered + "\n":
        errors.append("TASKS.md is stale (run --write)")

    # authority documents
    dp = ROOT / "DOSSIER.md"
    if not dp.exists():
        errors.append("DOSSIER.md missing")
    else:
        dossier = dp.read_text()
        for needle in DOSSIER_NEEDLES:
            if needle not in dossier:
                errors.append(f"DOSSIER missing required section/invariant: {needle}")
        nums = [int(x) for x in DECISION_RE.findall(dossier)]
        if nums != list(range(1, len(nums) + 1)):
            errors.append("DOSSIER decision log must be D-001..D-nnn sequential without gaps or duplicates")
    dm = ROOT / "DOMAIN.md"
    if not dm.exists():
        errors.append("DOMAIN.md missing")
    else:
        domain = dm.read_text()
        for needle in DOMAIN_NEEDLES:
            if needle not in domain:
                errors.append(f"DOMAIN missing required section: {needle}")
    for rel in ("AGENTS.md", "IMPLEMENTATION_MASTER_PROMPT.md"):
        if not (ROOT / rel).exists():
            errors.append(f"{rel} missing")
        elif "DOMAIN.md" not in (ROOT / rel).read_text():
            errors.append(f"{rel} must reference DOMAIN.md in the read order")

    # manifest (authority files only)
    mp = ROOT / "MANIFEST.json"
    if write or not mp.exists():
        mp.write_text(json.dumps(build_manifest(), indent=2) + "\n")
    elif load_json("MANIFEST.json") != build_manifest():
        errors.append("MANIFEST.json is stale (run --write)")

    if errors:
        print("V8.1 VALIDATION: FAIL")
        for e in errors:
            print(" -", e)
        return 1
    n_pass = sum(1 for p in ptasks.values() if p.get("status") == "PASS")
    print(f"V8.1 VALIDATION: PASS ({len(ids)} tasks, {len(ms)} milestones, {n_pass} PASS, "
          f"{len(ready_tasks(reg, prog))} ready)")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--write", action="store_true", help="regenerate TASKS.md, task-graph.json and MANIFEST.json")
    ap.add_argument("--ready", action="store_true", help="list dependency-ready tasks")
    ap.add_argument("--next", action="store_true", help="print the next task to claim")
    args = ap.parse_args()
    if args.ready or args.next:
        reg = load_json("registries/tasks.json")
        prog = load_json("registries/progress.json")
        if args.ready:
            for tid in ready_tasks(reg, prog):
                t = next(x for x in reg["tasks"] if x["id"] == tid)
                print(f"{tid}\t{t['milestone']}\t{t['title']}")
        if args.next:
            nt = next_task(reg, prog)
            print(nt if nt else "NONE")
        return 0
    return validate(args.write)


if __name__ == "__main__":
    raise SystemExit(main())
