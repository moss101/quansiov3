"""Minimal semver parsing and requirement matching for the supply-chain gate (OPS-007).

The gate must run with the Python standard library only and offline, so it carries the
small part of semver it needs instead of depending on `packaging`/`semver`. Two uses:

  * Cargo requirement strings (`"0.8"`, `">=1.2, <2"`, `"^1"`) against `Cargo.lock`
    resolved versions;
  * RustSec advisory `patched`/`unaffected` ranges against resolved versions.

Supported requirement operators: bare (Cargo caret), `^`, `~`, `=`, `>=`, `>`, `<`,
`<=`, `*`/`x`, comma-separated AND, `||`-separated OR. Anything else is treated as
unsatisfied, which fails closed: an unparseable requirement is a finding, never a pass.
"""
from __future__ import annotations

import re
from typing import Optional, Sequence, Tuple

_VERSION_RE = re.compile(
    r"^\s*v?(\d+(?:\.\d+)*)(?:-([0-9A-Za-z.-]+))?(?:\+([0-9A-Za-z.-]+))?\s*$"
)


class Version:
    """A parsed semantic version with prerelease comparison (build metadata ignored)."""

    __slots__ = ("prerelease", "release")

    def __init__(self, release: Tuple[int, ...], prerelease: Optional[Tuple[str, ...]]) -> None:
        self.release = release
        self.prerelease = prerelease

    def __eq__(self, other: object) -> bool:
        return isinstance(other, Version) and self._key() == other._key()

    def __hash__(self) -> int:
        return hash(self._key())

    def __lt__(self, other: "Version") -> bool:
        return self._key() < other._key()

    def __le__(self, other: "Version") -> bool:
        return self._key() <= other._key()

    def __gt__(self, other: "Version") -> bool:
        return self._key() > other._key()

    def __ge__(self, other: "Version") -> bool:
        return self._key() >= other._key()

    def _key(self):
        # A prerelease sorts before the corresponding release: (1,0,0,rc) < (1,0,0).
        return (self.release, 0 if self.prerelease is None else -1, self.prerelease or ())

    def __repr__(self) -> str:  # pragma: no cover - debugging aid
        text = ".".join(str(part) for part in self.release)
        if self.prerelease:
            text += "-" + ".".join(self.prerelease)
        return f"Version({text!r})"


def parse_version(text: str) -> Optional[Version]:
    """Parse a version string; returns None when it is not a version."""
    match = _VERSION_RE.match(str(text))
    if not match:
        return None
    release = tuple(int(part) for part in match.group(1).split("."))
    prerelease = tuple(match.group(2).split(".")) if match.group(2) else None
    return Version(release, prerelease)


def _pad(release: Tuple[int, ...], length: int) -> Tuple[int, ...]:
    return release + (0,) * (length - len(release))


def _lower_bound(version: Version, width: int) -> Version:
    return Version(_pad(version.release, width), version.prerelease)


def _bump(release: Tuple[int, ...], index: int) -> Tuple[int, ...]:
    parts = list(_pad(release, index + 1))
    parts[index] += 1
    return tuple(parts[: index + 1])


def _caret_upper(version: Version) -> Tuple[int, ...]:
    release = version.release
    for index, part in enumerate(release):
        if part != 0:
            return _bump(release, index)
    return _bump(release, len(release) - 1)


def _tilde_upper(version: Version) -> Tuple[int, ...]:
    if len(version.release) <= 1:
        return _bump(version.release, 0)
    return _bump(version.release, 1)


def _matches_one(version: Version, requirement: str) -> bool:
    requirement = requirement.strip()
    if requirement in {"", "*", "x", "X"}:
        return True
    match = re.match(r"^(>=|<=|>|<|=|\^|~)?\s*(.*)$", requirement)
    if not match:
        return False
    operator, remainder = match.group(1) or "", match.group(2).strip()
    bound = parse_version(remainder)
    if bound is None:
        return False
    if operator in {"", "^"}:
        upper = Version(_caret_upper(bound), None)
        return _lower_bound(bound, len(upper.release)) <= version < Version(
            _pad(upper.release, len(bound.release) + 1), None
        )
    if operator == "~":
        upper = Version(_tilde_upper(bound), None)
        return _lower_bound(bound, len(upper.release)) <= version < Version(
            _pad(upper.release, len(bound.release) + 1), None
        )
    if operator == "=":
        # `=1.2` is a prefix match (Cargo), `=1.2.3` is exact (prerelease-aware).
        width = len(bound.release)
        return _pad(version.release, width)[:width] == bound.release and (
            bound.prerelease is None or version.prerelease == bound.prerelease
        )
    if operator == ">=":
        return version >= _lower_bound(bound, len(bound.release))
    if operator == ">":
        return version > _lower_bound(bound, len(bound.release))
    if operator == "<":
        return version < _lower_bound(bound, len(bound.release))
    if operator == "<=":
        return version <= _lower_bound(bound, len(bound.release))
    return False


def satisfies(version: str, requirement: str) -> bool:
    """True when `version` satisfies `requirement` (Cargo/advisory style)."""
    parsed = parse_version(version)
    if parsed is None:
        return False
    for alternative in str(requirement).split("||"):
        clauses = [clause for clause in alternative.split(",") if clause.strip()]
        if clauses and all(_matches_one(parsed, clause) for clause in clauses):
            return True
    return False


def is_wildcard(requirement: str) -> bool:
    """True for a floating requirement (`*`, `x`, empty) that pins nothing."""
    text = str(requirement).strip()
    return text in {"", "*", "x", "X"} or "*" in text


def parse_requirement_name(requirement: str) -> Tuple[str, str]:
    """Split a PEP 508-ish requirement into (normalized name, specifier).

    `"psycopg[binary]>=3.2"` -> `("psycopg", ">=3.2")`. A requirement with no
    specifier returns an empty specifier, which callers treat as unpinned.
    """
    text = str(requirement).strip()
    match = re.match(r"^([A-Za-z0-9][A-Za-z0-9._-]*)\s*(?:\[[^\]]*\])?\s*(.*)$", text)
    if not match:
        return text, ""
    return normalize_name(match.group(1)), match.group(2).strip()


def normalize_name(name: str) -> str:
    """PEP 503 normalized project name (also used for cargo/npm comparisons)."""
    return re.sub(r"[-_.]+", "-", str(name)).lower()


def first_version(versions: Sequence[str]) -> Optional[str]:
    return versions[0] if versions else None


def any_satisfies(versions: Sequence[str], requirement: str) -> bool:
    return any(satisfies(version, requirement) for version in versions)
