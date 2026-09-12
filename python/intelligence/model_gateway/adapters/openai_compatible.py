"""OpenAI-compatible generic endpoint adapter (OD-004, DOSSIER.md §2.1).

Self-hosted and local inference servers (vLLM, llama.cpp, Ollama, LM Studio, TGI) speak the
OpenAI chat-completions wire format with a narrower and older surface. This adapter keeps the
normalized contract identical — streaming deltas, strict tool calling, usage, typed stop
reasons — while staying conservative about provider extensions:

* `max_tokens` (the long-standing field) instead of `max_completion_tokens`;
* no `parallel_tool_calls` (many servers reject unknown fields);
* `stream_options.include_usage` is requested, but a server that ignores it yields zero usage
  with an intact stop reason instead of a failure.

The endpoint and credential handle are indirection-only in the catalog
(`QUANSIO_OPENAI_COMPATIBLE_BASE_URL`, `QUANSIO_OPENAI_COMPATIBLE_KEY_HANDLE`), so a local
endpoint is configured, never hard-coded.
"""

from __future__ import annotations

from intelligence.model_gateway.adapters.openai import OpenAIAdapter
from intelligence.model_gateway.catalog import ProviderKind


class OpenAICompatibleAdapter(OpenAIAdapter):
    """The generic endpoint profile of the OpenAI chat-completions wire contract."""

    kind = ProviderKind.OPENAI_COMPATIBLE

    MAX_TOKENS_FIELD = "max_tokens"
    SEND_STREAM_OPTIONS = True
    SEND_PARALLEL_TOOL_CALLS = False

    __slots__ = ()
