"""Shared finding shape for the supply-chain gate (OPS-007).

A finding is printed as `rule: detail`, matching the other CI gates
(`scripts/ci/arch_check.py`, `scripts/ci/inventory.py`). Findings fail the gate;
`informational` results are reported but never fail it, so optional tools that need
network access can degrade explicitly instead of silently passing or blocking.
"""
from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class Finding:
    rule: str
    detail: str
    informational: bool = False

    def __str__(self) -> str:
        return f"{self.rule}: {self.detail}"
