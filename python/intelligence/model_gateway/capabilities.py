"""Capability vocabulary used by the model catalog and the adapters.

Capability names are behavior switches the adapters read from `config/models.yaml`
(`ModelSpec.capabilities`), never source constants that name a model (D-018).
"""

from __future__ import annotations

TOOLS = "tools"
STREAMING = "streaming"
VISION = "vision"
DOCUMENTS = "documents"
PROMPT_CACHING = "prompt_caching"
STRUCTURED_OUTPUT = "structured_output"
