"""`config/outputs.yaml` for the output adapters (ADP-006, DOMAIN.md §10.4).

The server reads the same file, so a deliverable an adapter builds within these bounds is one the
authority will accept. Nothing here is a source constant: change the file, not the module.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from types import MappingProxyType
from typing import Any

import yaml

CONFIG_RELATIVE_PATH = Path("config") / "outputs.yaml"


@dataclass(frozen=True, slots=True)
class OutputsConfig:
    """The decoded file, read-only, by section."""

    raw: Mapping[str, Any]

    def section(self, *path: str) -> Mapping[str, Any]:
        node: Any = self.raw
        for key in path:
            node = node[key]
        if not isinstance(node, Mapping):
            raise KeyError("/".join(path))
        return node

    @property
    def deck(self) -> Mapping[str, Any]:
        return self.section("deck")

    @property
    def render(self) -> Mapping[str, Any]:
        return self.section("render")

    @property
    def pdf_document(self) -> Mapping[str, Any]:
        return self.section("pdf", "document")

    @property
    def diagram(self) -> Mapping[str, Any]:
        return self.section("diagram")

    @property
    def video(self) -> Mapping[str, Any]:
        return self.section("video")

    @property
    def transcription(self) -> Mapping[str, Any]:
        return self.section("transcription")

    @property
    def preview(self) -> Mapping[str, Any]:
        return self.section("preview")


def default_config_path() -> Path:
    """Locate `config/outputs.yaml` by walking up from this module to the repository root."""
    for parent in Path(__file__).resolve().parents:
        candidate = parent / CONFIG_RELATIVE_PATH
        if candidate.is_file():
            return candidate
    raise FileNotFoundError(f"{CONFIG_RELATIVE_PATH.as_posix()} not found above {Path(__file__).resolve()}")


def load_outputs_config(path: Path | None = None) -> OutputsConfig:
    """Load and freeze the configuration."""
    resolved = path or default_config_path()
    raw = yaml.safe_load(resolved.read_text(encoding="utf-8"))
    if not isinstance(raw, dict) or raw.get("version") != 1:
        raise ValueError(f"{resolved} is not an outputs configuration (version 1)")
    return OutputsConfig(raw=_freeze(raw))


def _freeze(value: Any) -> Any:
    if isinstance(value, dict):
        return MappingProxyType({key: _freeze(item) for key, item in value.items()})
    if isinstance(value, list):
        return tuple(_freeze(item) for item in value)
    return value


_SHIPPED: OutputsConfig | None = None


def shipped_config() -> OutputsConfig:
    """The repository's configuration, loaded once."""
    global _SHIPPED
    if _SHIPPED is None:
        _SHIPPED = load_outputs_config()
    return _SHIPPED
