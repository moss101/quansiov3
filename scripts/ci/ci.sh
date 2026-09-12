#!/usr/bin/env bash
# Quansio V8.1 baseline pipeline (GOV-005).
#
# Runs the same gates as `.github/workflows/ci.yml` and publishes a commit-bound
# machine-readable summary to artifacts/ci/. Local runs and CI runs share one
# definition of "baseline" (scripts/ci/ci_summary.py).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

PYTHON="${PYTHON:-python3.12}"
if ! command -v "$PYTHON" >/dev/null 2>&1; then
  echo "ci: $PYTHON not found; set PYTHON=<interpreter >=3.11>" >&2
  exit 2
fi

exec "$PYTHON" scripts/ci/ci_summary.py "$@"
