"""Composition reachability: every authority the build ships is composed into it (QA-002).

A canonical module can be implemented, tested and still absent from the final build: its port is
installed only with the fail-closed default, or its service is constructed only by tests. QA-001's
coverage work found several (startup recovery, question expiry, the completion verifier, the plan
and memory ports, proposal trust), none of which the forbidden-wiring rules could see, because
nothing was *wrongly* wired — something was not wired at all.

The scan reads the Rust production sources (`crates/*/src`, with `#[cfg(test)]` modules and
comments removed) and, for every trait with implementations there, classifies each implementation
as a fail-closed default (`Unavailable*`, `No*`) or a real one, and asks whether production code
ever names it outside its own declaration. It reports:

* `NOT_COMPOSED` — a trait has a real implementation that production never references: the
  authority exists and the build runs without it;
* `DEFAULT_ONLY` — a trait production relies on has no real implementation at all, only its
  fail-closed default;
* `UNCOMPOSED` — a named authority service (or its lifecycle entry point) that the final build must
  run is never constructed (or called) by production code outside its own module. The list of
  obligations is curated, because a heuristic over every `new` cannot tell a service from a value
  type built by parsing.

Standard library only.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, Iterable, List, Set, Tuple

DEFAULT_PREFIXES = ("Unavailable", "No", "Noop")

IMPL = re.compile(r"\bimpl(?:\s*<[^{};]*?>)?\s+(\w+)(?:\s*<[^{};]*?>)?\s+for\s+(\w+)")
DECLARATION = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait|type)\s+(\w+)|"
    r"^\s*impl(?:\s*<[^{};]*?>)?\s+(?:\w+(?:\s*<[^{};]*?>)?\s+for\s+)?(\w+)|"
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?use\s"
)


@dataclass(frozen=True)
class Finding:
    """One composition gap."""

    kind: str
    trait: str
    detail: str

    def key(self) -> Tuple[str, str]:
        return (self.kind, self.trait)


def strip_comments(text: str) -> str:
    """Blank comments and the contents of string and char literals, keeping line structure.

    A type named inside a string (SQL, a message) or a comment is neither an implementation
    nor a construction. Raw strings (`r"…"`, `r#"…"#`) are handled; lifetimes are left alone.
    """
    out: List[str] = []
    i, n = 0, len(text)

    def blank(segment: str) -> str:
        return "".join("\n" if ch == "\n" else " " for ch in segment)

    while i < n:
        c = text[i]
        raw = re.match(r'r(#*)"', text[i:]) if c == "r" and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")) else None
        if raw:
            hashes = raw.group(1)
            start = i + len(raw.group(0))
            end = text.find('"' + hashes, start)
            end = n if end == -1 else end + 1 + len(hashes)
            out.append('""' + blank(text[start:end]))
            i = end
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            out.append('""' + blank(text[i + 1 : j]))
            i = j + 1
        elif c == "'" and i + 2 < n and text[i + 2] == "'" and text[i + 1] != "\\":
            out.append("' '")
            i += 3
        elif c == "'" and text.startswith("\\", i + 1) and i + 3 < n and text[i + 3] == "'":
            out.append("'  '")
            i += 4
        elif text.startswith("//", i):
            j = text.find("\n", i)
            i = n if j == -1 else j
        elif text.startswith("/*", i):
            j = text.find("*/", i + 2)
            end = n if j == -1 else j + 2
            out.append(blank(text[i:end]))
            i = end
        else:
            out.append(c)
            i += 1
    return "".join(out)


def strip_test_modules(text: str) -> str:
    """Remove every `#[cfg(test)]`-gated item (a test module or function) by brace matching."""
    result = text
    while True:
        match = re.search(r"#\[cfg\(test\)\]", result)
        if not match:
            return result
        start = match.start()
        brace = result.find("{", match.end())
        semicolon = result.find(";", match.end())
        if brace == -1 or (semicolon != -1 and semicolon < brace):
            end = (semicolon + 1) if semicolon != -1 else len(result)
        else:
            depth, end = 0, len(result)
            for index in range(brace, len(result)):
                if result[index] == "{":
                    depth += 1
                elif result[index] == "}":
                    depth -= 1
                    if depth == 0:
                        end = index + 1
                        break
        removed = result[start:end]
        result = result[:start] + "\n" * removed.count("\n") + result[end:]


USE_STATEMENT = re.compile(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?use\s[^;]*;")
TRAIT_DECLARATION = re.compile(r"\btrait\s+(\w+)")


def strip_use_statements(text: str) -> str:
    """Remove `use` statements (a re-export names a type without constructing it)."""
    return USE_STATEMENT.sub(lambda match: "\n" * match.group(0).count("\n"), text)


def production_sources(root: Path) -> Dict[str, str]:
    """Rust production sources, keyed by repository-relative path, tests and comments removed.

    Generated contract bindings are left out: they declare services and traits the build
    implements elsewhere, and name nothing the runtime composes.
    """
    sources: Dict[str, str] = {}
    for path in sorted((root / "crates").glob("*/src/**/*.rs")):
        relative = path.relative_to(root).as_posix()
        if "/generated/" in relative:
            continue
        sources[relative] = strip_use_statements(strip_test_modules(strip_comments(path.read_text())))
    return sources


def declared_traits(sources: Dict[str, str]) -> Set[str]:
    """Traits the workspace itself declares (a standard or foreign trait is not a port)."""
    return {name for text in sources.values() for name in TRAIT_DECLARATION.findall(text)}


def implementations(sources: Dict[str, str]) -> Dict[str, Set[str]]:
    """Trait -> the types implementing it in production code."""
    found: Dict[str, Set[str]] = {}
    for text in sources.values():
        for trait, implementor in IMPL.findall(text):
            found.setdefault(trait, set()).add(implementor)
    return found


def referenced(name: str, sources: Dict[str, str]) -> bool:
    """Whether production code names `name` anywhere other than a declaration or a `use`."""
    word = re.compile(rf"\b{re.escape(name)}\b")
    for text in sources.values():
        for line in text.splitlines():
            if word.search(line) and not DECLARATION.match(line):
                return True
    return False


def is_default(name: str) -> bool:
    return any(name.startswith(prefix) and name[len(prefix) : len(prefix) + 1].isupper()
               for prefix in DEFAULT_PREFIXES)


def scan(root: Path, ignore_traits: Iterable[str] = ()) -> List[Finding]:
    sources = production_sources(root)
    findings: List[Finding] = []
    ignored = set(ignore_traits)
    ports = declared_traits(sources)
    for trait, implementors in sorted(implementations(sources).items()):
        if trait in ignored or trait not in ports:
            continue
        real = sorted(name for name in implementors if not is_default(name))
        defaults = sorted(name for name in implementors if is_default(name))
        composed_real = [name for name in real if referenced(name, sources)]
        composed_defaults = [name for name in defaults if referenced(name, sources)]
        if real and not composed_real:
            findings.append(Finding(
                "NOT_COMPOSED", trait,
                f"{', '.join(real)} implement {trait} but production never constructs them"
                + (f"; it runs on {', '.join(composed_defaults)}" if composed_defaults else ""),
            ))
        elif not real and composed_defaults:
            findings.append(Finding(
                "DEFAULT_ONLY", trait,
                f"production composes {trait} only with its fail-closed {', '.join(composed_defaults)}",
            ))
    return findings


@dataclass(frozen=True)
class Obligation:
    """An authority the served build must compose: a type to construct or a function to call."""

    name: str
    kind: str  # "type" or "call"
    owner: str
    concern: str


#: The authorities the final build runs (DOSSIER.md §5 owners). Each must be constructed (a type)
#: or called (a lifecycle entry point) by production code outside the module that defines it.
OBLIGATIONS: Tuple[Obligation, ...] = (
    Obligation("RuntimeEngine", "type", "RUN-001", "the authoritative run state machine"),
    Obligation("ToolDispatchService", "type", "RUN-011", "tool dispatch through capability, policy and the Effect Ledger"),
    Obligation("RuntimeBudgets", "type", "RUN-010", "budget enforcement in the turn loop"),
    Obligation("AgentDelegationPort", "type", "RUN-002", "delegation and the AgentGraph"),
    Obligation("QuestionService", "type", "RUN-011", "durable questions"),
    Obligation("expire_due", "call", "RUN-011", "question expiry"),
    Obligation("recover_on_start", "call", "RUN-009", "recovery of non-terminal runs on start"),
    Obligation("Scheduler", "type", "CORE-008", "timers and routine fires"),
    Obligation("FireExecutor", "type", "CORE-008", "routine fire execution"),
    Obligation("Notifier", "type", "APP-010", "notifications"),
    Obligation("catch_up", "call", "OPS-004", "the usage projection"),
    Obligation("WebhookDeliverer", "type", "OPS-006", "webhook delivery"),
    Obligation("LiveHub", "type", "CORE-003", "live frames to subscribers"),
    Obligation("TenantMemberRoles", "type", "RUN-006", "acting roles for a run"),
    Obligation("ServerToolHost", "type", "EXEC-006", "the server tool host"),
    Obligation("GatewayModelSource", "type", "INT-002", "the model gateway"),
    Obligation("ContractVerifier", "type", "RUN-008", "CompletionContract verification"),
    Obligation("Orchestrator", "type", "RUN-004", "fan-out/fan-in, the ready queue and subtree cancellation"),
    Obligation("GraphWorkGraph", "type", "RUN-004", "the WorkGraph port orchestration applies"),
    Obligation("ProjectionRunner", "type", "CORE-009", "event projections"),
    Obligation("OutboxPublisher", "type", "CORE-003", "the transactional outbox publisher"),
    Obligation("admit_effect", "call", "OPS-004", "hard quotas refusing before an effect is reserved"),
    Obligation("KmsKeyProvider", "type", "EXEC-007", "KMS-backed secret keys"),
    Obligation("ComputerDispatcher", "type", "EXEC-010", "computer use"),
    Obligation("ComputerSequence", "type", "ADP-002", "native computer action sequences: terminal conditions and permission parking"),
    Obligation("WorkerHost", "type", "EXEC-002", "the qworkerd daemon's host"),
)


def defining_files(name: str, kind: str, sources: Dict[str, str]) -> Set[str]:
    if kind == "type":
        pattern = re.compile(rf"\b(?:struct|enum)\s+{re.escape(name)}\b")
    else:
        pattern = re.compile(rf"\bfn\s+{re.escape(name)}\b")
    return {path for path, text in sources.items() if pattern.search(text)}


def composed(obligation: Obligation, sources: Dict[str, str]) -> bool:
    """Whether production code constructs or calls it.

    Outside the defining module any mention counts. Inside it (a composition root may define
    what it composes) only a construction (`Name::…(`, `Name {`) or a call (`name(`) that is
    not the definition itself counts, so a type's own `impl` and a function's own `fn` line do
    not make it composed.
    """
    own = defining_files(obligation.name, obligation.kind, sources)
    name = re.escape(obligation.name)
    if obligation.kind == "type":
        anywhere = re.compile(rf"\b{name}\b")
        inside = re.compile(rf"\b{name}(?:::\w+\s*[(<]|\s*\{{)")
    else:
        anywhere = inside = re.compile(rf"(?:\.|::|\b){name}\s*[(<]")
    definition = re.compile(rf"\bfn\s+{name}\b")
    for path, text in sources.items():
        pattern = inside if path in own else anywhere
        for line in text.splitlines():
            if DECLARATION.match(line):
                continue
            for match in pattern.finditer(line):
                if obligation.kind == "call" and definition.match(line, max(0, match.start() - 3)):
                    continue
                if obligation.kind == "call" and line[: match.start()].rstrip().endswith("fn"):
                    continue
                return True
    return False


def uncomposed(root: Path, obligations: Iterable[Obligation] = OBLIGATIONS) -> List[Finding]:
    sources = production_sources(root)
    findings: List[Finding] = []
    for obligation in obligations:
        if not defining_files(obligation.name, obligation.kind, sources):
            findings.append(Finding("UNDEFINED", obligation.name,
                                    f"{obligation.owner}: no production definition of {obligation.name}"))
        elif not composed(obligation, sources):
            verb = "constructed" if obligation.kind == "type" else "called"
            findings.append(Finding("UNCOMPOSED", obligation.name,
                                    f"{obligation.owner}: {obligation.name} ({obligation.concern}) is never "
                                    f"{verb} by production code outside its module"))
    return findings
