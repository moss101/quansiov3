"""Configuration catalog tests (GOV-003): `config/models.yaml` and `config/flags.yaml`.

The catalog is the single place model identifiers live (D-018). These tests fail if the
catalog drifts from DOSSIER.md §7/§18 requirements or if a model id leaks into source.
"""
from __future__ import annotations

from pathlib import Path

import pytest
import yaml

from scripts.ci import inventory

ROOT = inventory.ROOT
MODELS = ROOT / "config" / "models.yaml"
FLAGS = ROOT / "config" / "flags.yaml"

REQUIRED_MODEL_FIELDS = {
    "id",
    "provider",
    "model_id",
    "capabilities",
    "context_window",
    "cost_class",
    "dlp_eligible",
}
GA_PROVIDER_KINDS = {"anthropic", "openai", "openai_compatible"}


@pytest.fixture(scope="module")
def catalog() -> dict:
    return yaml.safe_load(MODELS.read_text())


@pytest.fixture(scope="module")
def flags() -> dict:
    return yaml.safe_load(FLAGS.read_text())


def test_ga_provider_set_is_present(catalog):
    kinds = {p["kind"] for p in catalog["providers"].values()}
    assert GA_PROVIDER_KINDS <= kinds, f"GA providers must cover {GA_PROVIDER_KINDS}"


def test_credentials_are_handles_not_values(catalog):
    for name, provider in catalog["providers"].items():
        for key, value in provider.items():
            if "credential" in key or "key" in key:
                assert str(value).startswith(("provider/", "env:")) or key.endswith("_env"), (
                    f"{name}.{key} must reference a secret handle or environment variable name, not a value"
                )
            if key.endswith("_env"):
                assert str(value).isupper()


def test_models_declare_required_metadata(catalog):
    for model in catalog["models"]:
        missing = REQUIRED_MODEL_FIELDS - set(model)
        assert not missing, f"{model.get('id')}: missing {sorted(missing)}"
        assert isinstance(model["dlp_eligible"], bool)
        assert model["capabilities"], "a model must declare its capabilities"
        assert model["provider"] in catalog["providers"]
        assert model["context_window"] > 0


def test_model_ids_are_unique(catalog):
    ids = [m["id"] for m in catalog["models"]]
    assert len(ids) == len(set(ids))


def test_routing_is_deterministic_and_resolvable(catalog):
    routing = catalog["routing"]
    assert routing["strategy"] == "deterministic", "no router/planner LLM call may precede the primary model"
    ids = {m["id"] for m in catalog["models"]}
    assert routing["primary"] in ids
    assert set(routing["fallback_order"]) <= ids


def test_local_endpoint_is_not_dlp_eligible(catalog):
    local = next(m for m in catalog["models"] if m["id"] == "openai-compatible-local")
    assert local["dlp_eligible"] is False, "an unknown self-hosted endpoint must not receive protected content"


def test_flags_gate_rollout_only(flags):
    assert flags["flags"]["trust.escalation.enabled"] is True, "trust escalation is never disableable in GA"
    assert flags["flags"]["browser.live_view.transport"] == "cdp_screencast"


def test_no_model_identifier_appears_in_source(catalog):
    """D-018: model identifiers live in config/, never as source constants."""
    model_ids = {m["model_id"] for m in catalog["models"]}
    offenders = []
    for rel in inventory.tracked_files():
        if not inventory.is_product_code(rel) or rel.startswith(("config/", "tests/")):
            continue
        path = ROOT / rel
        if path.suffix not in {".rs", ".py", ".ts", ".tsx", ".swift"}:
            continue
        text = path.read_text(errors="replace")
        for model_id in model_ids:
            if model_id in text:
                offenders.append(f"{rel}: {model_id}")
    assert offenders == [], f"model ids must stay in config/: {offenders}"


def test_directories_required_by_dossier_exist():
    for rel in (
        "crates",
        "python/intelligence",
        "apps/desktop",
        "apps/web",
        "native/macos",
        "native/windows",
        "schemas",
        "migrations",
        "config",
        "packs/skills",
        "packs/capabilities",
        "sdk/typescript",
        "sdk/python",
        "infra/compose",
        "tests/contract",
        "tests/integration",
        "tests/e2e",
        "tests/recovery",
        "tests/security",
        "tests/performance",
        "tests/evaluation",
        "evidence",
        "scripts/ci",
        "scripts/dev",
    ):
        assert (ROOT / rel).is_dir(), f"DOSSIER.md §17 requires {rel}/"


def test_bootstrap_script_runs_every_gate():
    script = (ROOT / "scripts" / "dev" / "bootstrap.sh").read_text()
    for gate in (
        "validate_v81.py",
        "inventory.py --scan",
        "check_authority.py",
        "workspace_check.py",
        "cargo fmt",
        "cargo clippy",
        "cargo test",
        "ruff",
        "mypy",
        "pytest",
        "pnpm build",
        "pnpm typecheck",
        "pnpm test",
        "swift test",
    ):
        assert gate in script, f"bootstrap must run {gate}"
