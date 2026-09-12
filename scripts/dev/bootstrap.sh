#!/usr/bin/env bash
# Deterministic clean bootstrap and gate run (GOV-003/GOV-005).
#
# Verifies the monorepo builds from a clean checkout across all four toolchains:
#   Rust workspace, Python intelligence plane, TypeScript workspace, native bridges.
# Exits non-zero on the first failing gate.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

# Governance tooling needs 3.11+ (tomllib); the product pins 3.12 in .python-version.
PYTHON="${PYTHON:-python3.12}"
if ! command -v "$PYTHON" >/dev/null 2>&1; then
  echo "bootstrap: $PYTHON not found; set PYTHON=<interpreter >=3.11>" >&2
  exit 2
fi

echo "== authority gate =="
"$PYTHON" scripts/validate_v81.py

echo "== architecture gates =="
"$PYTHON" scripts/ci/inventory.py --scan
"$PYTHON" scripts/ci/check_authority.py --check
"$PYTHON" scripts/ci/workspace_check.py
"$PYTHON" scripts/ci/legacy_map_check.py
"$PYTHON" scripts/ci/gen_contracts.py --check
"$PYTHON" scripts/ci/contract_compat.py

echo "== rust workspace =="
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

echo "== python intelligence plane =="
(cd python && uv sync --python 3.12 --frozen --quiet && uv run --frozen ruff check . && uv run --frozen ruff format --check . \
  && uv run --frozen mypy intelligence && uv run --frozen pytest -q)

echo "== typescript workspace =="
pnpm install --frozen-lockfile
pnpm build
pnpm typecheck
pnpm test
pnpm lint

echo "== native bridges (macos, optional) =="
if command -v swift >/dev/null 2>&1 && [ "$(uname -s)" = "Darwin" ]; then
  swift test --package-path native/macos
else
  echo "swift toolchain unavailable: macOS bridge build skipped (BLOCKED_EXTERNAL for that surface only)"
fi

echo "bootstrap: OK"
