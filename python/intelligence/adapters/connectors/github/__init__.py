"""GitHub connector package: the base adapter plus EXEC-012's PR and CI adapters."""

from __future__ import annotations

from .ci import GITHUB_CI
from .connector import GITHUB
from .pr import GITHUB_PR

__all__ = ["GITHUB", "GITHUB_CI", "GITHUB_PR"]
