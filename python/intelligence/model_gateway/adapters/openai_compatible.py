"""OpenAI-compatible generic endpoint adapter (OD-004, DOSSIER.md §2.1).

Self-hosted and local inference servers (vLLM, llama.cpp, Ollama, LM Studio, TGI) and hosted
services that expose the format (Z.ai, Azure, gateways) speak the OpenAI chat-completions wire
format with a narrower and older surface. This adapter keeps the normalized contract identical
— streaming deltas, strict tool calling, usage, typed stop reasons — while staying conservative
about provider extensions:

* `max_tokens` (the long-standing field) instead of `max_completion_tokens`;
* no `parallel_tool_calls` (many servers reject unknown fields);
* the operator channel is a `system` message (DOSSIER.md §7: the provider's operator channel
  *where available*, never a fake user turn); OpenAI's `developer` role is rejected by most
  servers, and a second `system` message is the dialect's own operator channel;
* `stream_options.include_usage` is requested, but a server that ignores it yields zero usage
  with an intact stop reason instead of a failure.

The base URL follows the OpenAI SDK convention every such service documents: it includes the
service's own version segment (`http://127.0.0.1:11434/v1`, `https://api.z.ai/api/paas/v4`),
and the wire paths are relative to it (`/chat/completions`, `/embeddings`). The first-party
OpenAI adapter is the one that addresses its endpoint from the root and versions the path,
because that version is OpenAI's, not the dialect's.

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

    CHAT_COMPLETIONS_PATH = "/chat/completions"
    OPERATOR_WIRE_ROLE = "system"
    MAX_TOKENS_FIELD = "max_tokens"
    SEND_STREAM_OPTIONS = True
    SEND_PARALLEL_TOOL_CALLS = False

    __slots__ = ()
