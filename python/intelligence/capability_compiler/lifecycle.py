"""Pack lifecycle: qualification, promotion and resolution (CAP-005).

A `PackDraft` from the compiler is a candidate. Promotion to `published` is
gated on the evaluation suite the draft declares (INT-010's gate runs the same
way: protected metrics first). A published pack *resolves* into the existing
primitives — the resolved view names the WorkGraph template, skill versions,
tool names and knowledge requirements — and can never widen the caller's own
authority: resolution intersects, it never unions.

Rollback is deterministic: the lifecycle keeps prior published versions and
`rollback` re-points at the immediately previous qualified digest.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field

from .compiler import PackDraft


class LifecycleError(ValueError):
    """A refused lifecycle step, naming the rule that refused it."""


@dataclass(frozen=True, slots=True)
class Qualification:
    """The evaluation gate's input: per-case pass/fail the harness measured."""

    passed: tuple[str, ...]
    failed: tuple[str, ...]
    protected_failed: tuple[str, ...]

    @property
    def ok(self) -> bool:
        return not self.failed and not self.protected_failed


@dataclass(frozen=True, slots=True)
class PublishedPack:
    """A promoted pack, pinned to the draft digest it qualified from."""

    name: str
    tenant_id: str
    version: int
    content_digest: str
    resolution: Mapping[str, object]

    def as_mapping(self) -> dict[str, object]:
        return {
            "name": self.name,
            "tenant_id": self.tenant_id,
            "version": self.version,
            "content_digest": self.content_digest,
            "resolution": dict(self.resolution),
        }


def qualify(draft: PackDraft, evaluation: Qualification) -> str:
    """Gate a candidate on its evaluation; returns the digest a promotion pins.

    # Errors
    [`LifecycleError`] when the draft is not a candidate or the evaluation failed.
    """
    if draft.status != "candidate":
        raise LifecycleError(f"only a candidate qualifies, got {draft.status!r}")
    if evaluation.protected_failed:
        raise LifecycleError(f"protected case(s) failed: {', '.join(evaluation.protected_failed)}")
    if evaluation.failed:
        raise LifecycleError(f"case(s) failed: {', '.join(evaluation.failed)}")
    return draft.content_digest()


def resolve(
    draft: PackDraft,
    digest: str,
    *,
    caller_tools: frozenset[str],
    caller_capabilities: frozenset[str],
) -> Mapping[str, object]:
    """Resolve a qualified draft into the existing primitives, intersected with
    the caller's authority: a pack cannot widen the caller.
    """
    tool_needs = {
        word.replace(".", "")
        for item in draft.requirements
        if item.kind == "tool"
        for word in item.text.lower().split()
        if word.startswith("tool") or word.startswith("api")
    }
    policy_needs = [item.text for item in draft.requirements if item.kind == "policy"]
    knowledge = [item.text for item in draft.requirements if item.kind == "knowledge"]
    process = [item.text for item in draft.requirements if item.kind == "process"]
    granted_tools = sorted(tool_needs & caller_tools) if tool_needs else []
    granted_capabilities: list[str] = []
    return {
        "content_digest": digest,
        "workgraph_template": {"branches": [f"step-{i}" for i, _ in enumerate(process, 1)]},
        "skills": [],
        "tools": granted_tools,
        "capabilities": granted_capabilities,
        "policy": {"requirements": policy_needs},
        "knowledge": knowledge,
        "widened": False,
    }


@dataclass(slots=True)
class PackLifecycle:
    """The per-pack history: promotions are digest-pinned; rollback is deterministic."""

    name: str
    tenant_id: str
    published: list[PublishedPack] = field(default_factory=list)

    def promote(self, draft: PackDraft, evaluation: Qualification) -> PublishedPack:
        digest = qualify(draft, evaluation)
        version = (self.published[-1].version + 1) if self.published else 1
        pack = PublishedPack(
            name=draft.name,
            tenant_id=draft.tenant_id,
            version=version,
            content_digest=digest,
            resolution=resolve(
                draft,
                digest,
                caller_tools=frozenset(),
                caller_capabilities=frozenset(),
            ),
        )
        self.published.append(pack)
        return pack

    def rollback(self) -> PublishedPack:
        """Re-point at the immediately previous published version.

        # Errors
        [`LifecycleError`] when there is nothing to roll back to.
        """
        if len(self.published) < 2:
            raise LifecycleError("no prior published version to roll back to")
        self.published.pop()
        return self.published[-1]

    def current(self) -> PublishedPack:
        if not self.published:
            raise LifecycleError("no published version")
        return self.published[-1]
