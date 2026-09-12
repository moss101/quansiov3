# quansio-intelligence

**Canonical owner:** `python/intelligence` — the Quansio V8.1 intelligence plane.

Owns model gateway, context projection/rendering, knowledge, memory, embeddings,
trust support, skills support, capability compilation, evaluation, artifact
adapters and connector adapters. Everything here talks to the trusted Rust plane
through generated typed RPC contracts and may only *propose*; the runtime
validates and commits (DOSSIER.md §3, D-002).

Build/test: `uv run --project python pytest` · Lint: `uv run --project python ruff check` ·
Types: `uv run --project python mypy intelligence`
