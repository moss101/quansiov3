"""Which directories under the repository root belong to *this* checkout (OPS-007).

A git worktree or clone below the root -- `.claude/worktrees/<name>/` is the everyday case,
since agents work in worktrees -- is a complete tree of its own, often of this repository at
another commit. Walking into it reports that tree's manifests, lockfiles and negative test
fixtures as if they were this checkout's: the supply-chain gate then fails (or, for a finding
that only the other branch has fixed, passes) on the strength of code that is not under
review. The scanners therefore stop at any directory that carries its own `.git` entry, which
is a directory for a clone and a file for a worktree.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import FrozenSet

GIT_ENTRY = ".git"


def nested_checkout_roots(root: Path) -> FrozenSet[Path]:
    """Directories strictly below `root` that are git checkouts of their own."""
    found = set()
    for directory, dirnames, filenames in os.walk(root):
        current = Path(directory)
        if current != root and (GIT_ENTRY in dirnames or GIT_ENTRY in filenames):
            found.add(current)
            dirnames[:] = []  # its subtree belongs to that checkout, not to this one
            continue
        dirnames[:] = [name for name in dirnames if name != GIT_ENTRY]
    return frozenset(found)


def in_nested_checkout(path: Path, nested: FrozenSet[Path]) -> bool:
    """True when `path` lies inside one of `nested` (from `nested_checkout_roots`)."""
    return any(parent in nested for parent in path.parents)
