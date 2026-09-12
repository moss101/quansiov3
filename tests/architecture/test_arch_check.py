"""GOV-008 tests: architecture conformance gate.

Every rule is exercised twice: the real repository must be clean for it, and a
fixture tree that violates it must be reported under the right rule id. The gate
must reuse the inventory rule engine (`scripts/ci/inventory.py`) rather than
duplicating it.
"""
from __future__ import annotations

import json
from pathlib import Path
from textwrap import dedent
from typing import Dict, List, Optional

import pytest

from scripts.ci import arch_check

RULES = [
    "renderer-to-provider",
    "python-authority-write",
    "worker-to-control-db",
    "gateway-effect-execution",
    "tool-registration-outside-owner",
    "second-scheduler-or-timer",
    "new-deployable-without-decision",
    "new-persistent-store",
]


def _tree(root: Path, files: Dict[str, str]) -> Path:
    for rel, content in files.items():
        target = root / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(dedent(content))
    return root


def _findings(root: Path, rule: Optional[str] = None) -> List[Dict[str, object]]:
    findings = arch_check.check(root)
    return [f for f in findings if rule is None or f["rule"] == rule]


# --------------------------------------------------------------------------- baseline
def test_real_repository_is_clean():
    assert arch_check.check() == []


@pytest.mark.parametrize("rule", RULES)
def test_real_repository_is_clean_for_rule(rule: str):
    assert _findings(arch_check.ROOT, rule) == []


def test_gate_reuses_the_inventory_rule_engine(monkeypatch):
    calls = []
    real_scan = arch_check.inventory.scan

    def recording_scan(*args, **kwargs):
        calls.append((args, kwargs))
        return real_scan(*args, **kwargs)

    monkeypatch.setattr(arch_check.inventory, "scan", recording_scan)
    findings = arch_check.check()

    assert calls, "arch_check must call inventory.scan, not reimplement the rule engine"
    assert any("files" in kwargs and "root" in kwargs for _, kwargs in calls)
    assert findings == []


# ------------------------------------------------------------- renderer-to-provider
def test_renderer_to_provider_flags_provider_sdk_import(tmp_path):
    root = _tree(
        tmp_path,
        {"apps/desktop/renderer/src/chat.ts": 'import OpenAI from "openai";\n'},
    )
    findings = _findings(root, "renderer-to-provider")
    assert len(findings) == 1
    assert findings[0]["path"] == "apps/desktop/renderer/src/chat.ts"
    assert findings[0]["line"] == 1
    assert "openai" in str(findings[0]["detail"])


def test_renderer_to_provider_flags_anthropic_sdk_under_apps(tmp_path):
    root = _tree(
        tmp_path,
        {"apps/web/src/client.ts": 'import Anthropic from "@anthropic-ai/sdk";\n'},
    )
    assert _findings(root, "renderer-to-provider")


def test_renderer_to_provider_flags_provider_base_url(tmp_path):
    root = _tree(
        tmp_path,
        {"apps/web/src/config.ts": 'export const BASE = "https://api.anthropic.com/v1";\n'},
    )
    findings = _findings(root, "renderer-to-provider")
    assert len(findings) == 1
    assert "api.anthropic.com" in str(findings[0]["detail"])


def test_renderer_to_provider_allows_server_client_code(tmp_path):
    root = _tree(
        tmp_path,
        {"apps/web/src/client.ts": 'export const base = publicApiPath("threads");\n'},
    )
    assert _findings(root, "renderer-to-provider") == []


# ----------------------------------------------------------- python-authority-write
def test_python_authority_write_flags_canonical_table_write(tmp_path):
    root = _tree(
        tmp_path,
        {
            "python/intelligence/context/store.py": """
            def commit(conn):
                conn.execute('UPDATE effect_ledger SET settled = true')
            """
        },
    )
    assert _findings(root, "python-authority-write")


def test_python_authority_write_flags_rust_authority_client_import(tmp_path):
    root = _tree(
        tmp_path,
        {"python/intelligence/context/client.py": "from quansio.v1.effects import effects_pb2\n"},
    )
    findings = _findings(root, "python-authority-write")
    assert len(findings) == 1
    assert findings[0]["line"] == 1
    assert "quansio.v1.effects" in str(findings[0]["detail"])


def test_python_authority_write_allows_intelligence_rpc_client(tmp_path):
    root = _tree(
        tmp_path,
        {
            "python/intelligence/model_gateway/rpc.py": (
                "from quansio.v1.intelligence import service_pb2_grpc\n"
            )
        },
    )
    assert _findings(root) == []


# ------------------------------------------------------------- worker-to-control-db
def test_worker_to_control_db_flags_postgres_client(tmp_path):
    root = _tree(tmp_path, {"crates/qworkerd/src/db.rs": "use sqlx::PgPool;\n"})
    findings = _findings(root, "worker-to-control-db")
    assert findings
    assert findings[0]["path"] == "crates/qworkerd/src/db.rs"


def test_worker_to_control_db_flags_control_table_query(tmp_path):
    root = _tree(
        tmp_path,
        {
            "native/windows/src/query.rs": (
                'pub const Q: &str = "SELECT * FROM runtime_events";\n'
            )
        },
    )
    findings = _findings(root, "worker-to-control-db")
    assert len(findings) == 1
    assert "runtime_events" in str(findings[0]["detail"])


def test_worker_to_control_db_flags_control_database_url(tmp_path):
    root = _tree(
        tmp_path,
        {"crates/qworkerd/src/config.rs": 'pub const URL: &str = "DATABASE_URL";\n'},
    )
    assert _findings(root, "worker-to-control-db")


def test_worker_to_control_db_allows_fenced_worker_channel(tmp_path):
    root = _tree(
        tmp_path,
        {
            "crates/qworkerd/src/channel.rs": """
            //! Frame dispatch over the fenced worker channel.
            pub fn dispatch(frame: Frame) -> Result<(), Error> {
                handle(frame)
            }
            """
        },
    )
    assert _findings(root, "worker-to-control-db") == []


def test_worker_to_control_db_ignores_prose_about_approval(tmp_path):
    root = _tree(
        tmp_path,
        {
            "native/macos/Sources/QuansioMacBridge/MacBridge.swift": """
            /// It never decides capability, policy, approval or effect outcomes.
            public enum CapsuleSubstrate: String { case appleVirtualization }
            """
        },
    )
    assert _findings(root, "worker-to-control-db") == []


# --------------------------------------------------------- gateway-effect-execution
def test_gateway_effect_execution_flags_effect_import(tmp_path):
    root = _tree(
        tmp_path,
        {"python/intelligence/model_gateway/effects.py": "from quansio.v1.effects import reserve_effect\n"},
    )
    findings = _findings(root, "gateway-effect-execution")
    assert len(findings) == 1
    assert "reserve_effect" in str(findings[0]["detail"])


def test_gateway_effect_execution_flags_tool_registration(tmp_path):
    root = _tree(
        tmp_path,
        {
            "python/intelligence/model_gateway/tools.py": """
            def wire(registry):
                registry.register_tool("shell.exec", handler)
            """
        },
    )
    findings = _findings(root, "gateway-effect-execution")
    assert len(findings) == 1
    assert "register_tool" in str(findings[0]["detail"])


def test_gateway_effect_execution_allows_provider_adapter(tmp_path):
    root = _tree(
        tmp_path,
        {"python/intelligence/model_gateway/anthropic_adapter.py": "import anthropic\n"},
    )
    assert _findings(root) == []


# --------------------------------------------------- tool-registration-outside-owner
def test_tool_registration_flags_rust_attribute_outside_tools_crate(tmp_path):
    root = _tree(
        tmp_path,
        {"crates/graph/src/tools.rs": '#[tool(name = "graph.write")]\npub fn write() {}\n'},
    )
    findings = _findings(root, "tool-registration-outside-owner")
    assert len(findings) == 1
    assert "#[tool(...)]" in str(findings[0]["detail"])


def test_tool_registration_flags_register_call_outside_tools_crate(tmp_path):
    root = _tree(
        tmp_path,
        {"crates/server/src/runtime/wire.rs": 'registry.register_tool("shell.exec", handler);\n'},
    )
    assert _findings(root, "tool-registration-outside-owner")


def test_tool_registration_flags_typescript_registration(tmp_path):
    root = _tree(
        tmp_path,
        {"apps/web/src/tools.ts": "registerTool('shell.exec', handler);\n"},
    )
    assert _findings(root, "tool-registration-outside-owner")


def test_tool_registration_allows_the_tools_crate(tmp_path):
    root = _tree(
        tmp_path,
        {
            "crates/tools/src/registry.rs": """
            #[tool(name = "shell.exec")]
            fn shell_exec() {}

            pub fn wire(registry: &mut Registry) {
                registry.register_tool("shell.exec", handler);
            }
            """
        },
    )
    assert _findings(root, "tool-registration-outside-owner") == []


# --------------------------------------------------------- second-scheduler-or-timer
def test_second_scheduler_flags_interval_outside_owner(tmp_path):
    root = _tree(
        tmp_path,
        {
            "crates/graph/src/tick.rs": """
            async fn tick() {
                let mut interval = tokio::time::interval(Duration::from_secs(1));
            }
            """
        },
    )
    findings = _findings(root, "second-scheduler-or-timer")
    assert len(findings) == 1
    assert "interval" in str(findings[0]["detail"])


def test_second_scheduler_flags_set_interval_outside_owner(tmp_path):
    root = _tree(
        tmp_path,
        {"apps/web/src/poll.ts": "setInterval(() => refresh(), 1000);\n"},
    )
    assert _findings(root, "second-scheduler-or-timer")


def test_second_scheduler_flags_croniter_outside_owner(tmp_path):
    root = _tree(
        tmp_path,
        {"crates/machine/src/routine.py": "from croniter import croniter\n"},
    )
    assert _findings(root, "second-scheduler-or-timer")


def test_second_scheduler_flags_spawned_loop(tmp_path):
    root = _tree(
        tmp_path,
        {
            "crates/machine/src/worker.rs": """
            pub fn start() {
                tokio::spawn(async move {
                    loop {
                        tick().await;
                    }
                });
            }
            """
        },
    )
    assert _findings(root, "second-scheduler-or-timer")


def test_second_scheduler_allows_canonical_owners(tmp_path):
    root = _tree(
        tmp_path,
        {
            "crates/server/src/scheduler/timers.rs": """
            pub fn arm() {
                let mut interval = tokio::time::interval(Duration::from_secs(1));
                tokio::spawn(async move {
                    loop {
                        interval.tick().await;
                    }
                });
            }
            """,
            "crates/events/src/retention.rs": "let schedule = croniter(\"0 * * * *\");\n",
        },
    )
    assert _findings(root, "second-scheduler-or-timer") == []


def test_second_scheduler_ignores_one_shot_spawn(tmp_path):
    root = _tree(
        tmp_path,
        {
            "crates/machine/src/worker.rs": """
            pub fn start() {
                tokio::spawn(async move { work().await });
            }
            """
        },
    )
    assert _findings(root, "second-scheduler-or-timer") == []


# --------------------------------------------------- new-deployable-without-decision
def test_new_deployable_allowlist_is_anchored_to_infra():
    assert arch_check.ALLOWLISTED_DEPLOYABLES
    assert all(path.startswith("infra/") for path in arch_check.ALLOWLISTED_DEPLOYABLES)
    assert set(arch_check.ALLOWLISTED_COMPOSE_SERVICES) <= set(arch_check.ALLOWLISTED_DEPLOYABLES)


def test_real_compose_services_are_allowlisted():
    text = (arch_check.ROOT / "infra/compose/compose.yaml").read_text()
    allowed = arch_check.ALLOWLISTED_COMPOSE_SERVICES["infra/compose/compose.yaml"]
    services = [name for name, _ in arch_check._compose_services(text)]
    assert services
    assert set(services) <= allowed


def test_new_deployable_flags_dockerfile_without_decision(tmp_path):
    root = _tree(tmp_path, {"crates/rogue/Dockerfile": "FROM scratch\n"})
    findings = _findings(root, "new-deployable-without-decision")
    assert len(findings) == 1
    assert findings[0]["path"] == "crates/rogue/Dockerfile"


def test_new_deployable_flags_helm_chart_without_decision(tmp_path):
    root = _tree(tmp_path, {"crates/rogue/Chart.yaml": "apiVersion: v2\nname: rogue\n"})
    assert _findings(root, "new-deployable-without-decision")


def test_new_deployable_flags_compose_service_without_decision(tmp_path):
    root = _tree(
        tmp_path,
        {
            "infra/compose/compose.yaml": """
            services:
              postgres:
                image: pgvector/pgvector:pg17
              second-store:
                image: redis:7
            """
        },
    )
    findings = _findings(root, "new-deployable-without-decision")
    assert len(findings) == 1
    assert "second-store" in str(findings[0]["detail"])
    assert findings[0]["line"] == 5


def test_new_deployable_with_decision_is_clean(tmp_path):
    root = _tree(
        tmp_path,
        {
            "crates/rogue/Dockerfile": "FROM scratch\n",
            "DOSSIER.md": (
                "- **D-019:** Ship the rogue worker deployable. *Rationale:* test. "
                "*Migration:* n/a. *Rollback:* n/a. Path: `crates/rogue/Dockerfile`.\n"
            ),
        },
    )
    assert _findings(root, "new-deployable-without-decision") == []


def test_new_deployable_compose_service_with_decision_is_clean(tmp_path):
    root = _tree(
        tmp_path,
        {
            "infra/compose/compose.yaml": """
            services:
              postgres:
                image: pgvector/pgvector:pg17
              second-store:
                image: redis:7
            """,
            "DOSSIER.md": (
                "- **D-020:** Add the derived cache service. *Rationale:* test. "
                "*Migration:* n/a. *Rollback:* n/a. "
                "Path: `infra/compose/compose.yaml` service `second-store`.\n"
            ),
        },
    )
    assert _findings(root, "new-deployable-without-decision") == []


# ------------------------------------------------------------- new-persistent-store
def test_new_persistent_store_flags_rust_client_outside_owner(tmp_path):
    root = _tree(
        tmp_path,
        {
            "crates/capability/Cargo.toml": """
            [package]
            name = "quansio-capability"

            [dependencies]
            sqlx = { version = "0.8", features = ["postgres"] }
            """
        },
    )
    findings = _findings(root, "new-persistent-store")
    assert len(findings) == 1
    assert findings[0]["path"] == "crates/capability/Cargo.toml"
    assert "sqlx" in str(findings[0]["detail"])


def test_new_persistent_store_flags_typescript_client_outside_owner(tmp_path):
    root = _tree(
        tmp_path,
        {"apps/web/package.json": '{"name": "@quansio/web", "dependencies": {"pg": "^8.13.1"}}\n'},
    )
    findings = _findings(root, "new-persistent-store")
    assert len(findings) == 1
    assert "pg" in str(findings[0]["detail"])


def test_new_persistent_store_flags_python_client_outside_owner(tmp_path):
    root = _tree(
        tmp_path,
        {
            "python/services/worker/pyproject.toml": """
            [project]
            name = "quansio-worker"
            dependencies = ["psycopg[binary]>=3.2"]
            """
        },
    )
    findings = _findings(root, "new-persistent-store")
    assert len(findings) == 1
    assert findings[0]["path"] == "python/services/worker/pyproject.toml"
    assert "psycopg" in str(findings[0]["detail"])


def test_new_persistent_store_allows_canonical_owner(tmp_path):
    root = _tree(
        tmp_path,
        {
            "crates/server/Cargo.toml": """
            [package]
            name = "quansio-server"

            [dependencies]
            sqlx = { version = "0.8", features = ["postgres"] }
            """
        },
    )
    assert _findings(root, "new-persistent-store") == []


def test_new_persistent_store_allows_python_embeddings_distribution(tmp_path):
    root = _tree(
        tmp_path,
        {
            "python/pyproject.toml": """
            [project]
            name = "quansio-intelligence"
            dependencies = ["psycopg[binary]>=3.2"]

            [tool.hatch.build.targets.wheel]
            packages = ["intelligence"]
            """
        },
    )
    assert _findings(root, "new-persistent-store") == []


def test_new_persistent_store_flags_requirements_client_outside_owner(tmp_path):
    root = _tree(
        tmp_path,
        {"python/services/worker/requirements.txt": "redis>=5.0\n"},
    )
    findings = _findings(root, "new-persistent-store")
    assert len(findings) == 1
    assert findings[0]["path"] == "python/services/worker/requirements.txt"
    assert "redis" in str(findings[0]["detail"])


def test_new_persistent_store_ignores_dev_dependencies(tmp_path):
    root = _tree(
        tmp_path,
        {
            "crates/capability/Cargo.toml": """
            [package]
            name = "quansio-capability"

            [dev-dependencies]
            sqlx = "0.8"
            """,
            "python/services/worker/pyproject.toml": """
            [project]
            name = "quansio-worker"
            dependencies = []

            [dependency-groups]
            dev = ["psycopg[binary]>=3.2"]
            """,
        },
    )
    assert _findings(root, "new-persistent-store") == []


# ------------------------------------------------------------------------------ CLI
def test_cli_exit_codes_and_output(tmp_path, capsys):
    assert arch_check.main(["--json"]) == 0
    assert json.loads(capsys.readouterr().out) == []

    clean = tmp_path / "clean"
    clean.mkdir()
    assert arch_check.main(["--root", str(clean)]) == 0
    assert "CLEAN" in capsys.readouterr().out

    bad = _tree(
        tmp_path / "bad",
        {"apps/web/src/urls.ts": 'export const base = "https://api.openai.com/v1";\n'},
    )
    assert arch_check.main(["--root", str(bad)]) == 1
    out = capsys.readouterr().out
    assert out.startswith("renderer-to-provider: apps/web/src/urls.ts:1: ")
