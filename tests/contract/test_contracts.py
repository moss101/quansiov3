"""GOV-004 contract tests: derivation, lint, regeneration diff and compatibility.

These tests fail if a generated contract drifts from DOMAIN.md, if a binding is stale
relative to its source, if a proto breaks the additive-only rule, or if a persisted
artifact no longer matches its JSON Schema.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest
import yaml

from scripts.ci import contract_compat, domain_catalog, gen_contracts

ROOT = gen_contracts.ROOT
CATALOG = ROOT / "schemas" / "catalog"
OPENAPI = ROOT / "schemas" / "openapi" / "public-api-v1.yaml"


@pytest.fixture(scope="module")
def catalog() -> dict:
    return domain_catalog.build_catalog()


@pytest.fixture(scope="module")
def openapi() -> dict:
    return yaml.safe_load(OPENAPI.read_text())


def _load_catalog(key: str):
    return yaml.safe_load((CATALOG / domain_catalog.CATALOG_FILES[key]).read_text())[key]


def test_catalog_is_derived_from_domain_and_current(catalog):
    assert gen_contracts.check(["catalog"]) == []
    assert _load_catalog("commands") == catalog["commands"]
    assert _load_catalog("errors") == catalog["errors"]


def test_domain_derivation_covers_commands_errors_effects_events_trust(catalog):
    assert sum(len(v) for v in catalog["commands"].values()) >= 70
    assert len(catalog["errors"]) == 38
    assert len(catalog["effect_classes"]) == 23
    assert len(catalog["event_families"]) == 34
    assert catalog["trust_levels"] == [
        "TRUSTED_SYSTEM",
        "TRUSTED_USER",
        "VERIFIED_KNOWLEDGE",
        "AGENT_GENERATED",
        "UNTRUSTED_EXTERNAL",
    ]
    assert len(catalog["ids"]) >= 45
    assert len(catalog["glossary"]) >= 50


def test_openapi_is_current_and_generated_from_the_catalog(catalog, openapi):
    assert gen_contracts.check(["openapi"]) == []
    assert openapi["openapi"] == "3.1.0"
    assert set(openapi["security"][0]) == {"bearerAuth"}


def test_every_domain_command_has_a_generated_contract(catalog, openapi):
    documented = {name for names in catalog["commands"].values() for name in names}
    generated = {
        path.split("/")[-1] for path in openapi["paths"] if path.startswith("/v1/commands/")
    }
    assert documented == generated
    # Each command references its params schema and the shared error schema.
    for name in documented:
        operation = openapi["paths"][f"/v1/commands/{name}"]["post"]
        assert operation["operationId"] == name
        assert "default" in operation["responses"]


def test_every_domain_error_code_is_in_the_generated_error_schema(catalog, openapi):
    documented = {error["code"] for error in catalog["errors"]}
    schema_codes = set(openapi["components"]["schemas"]["Error"]["properties"]["code"]["enum"])
    assert documented == schema_codes
    assert documented == set(openapi["x-quansio-error-http-map"])


def test_openapi_refs_resolve(openapi):
    schemas = set(openapi["components"]["schemas"])

    def walk(node):
        if isinstance(node, dict):
            ref = node.get("$ref")
            if ref:
                assert ref.startswith("#/components/schemas/")
                assert ref.split("/")[-1] in schemas, f"unresolved $ref {ref}"
            for value in node.values():
                walk(value)
        elif isinstance(node, list):
            for value in node:
                walk(value)

    walk(openapi["paths"])


def test_read_projections_are_present(catalog, openapi):
    for path in catalog["read_projections"]:
        if path.endswith("/stream"):
            continue
        assert path in openapi["paths"], f"missing projection path {path}"


def test_every_id_prefix_has_a_generated_proto_enum_value(catalog):
    proto = (ROOT / "schemas" / "proto" / "quansio" / "v1" / "core" / "identity.proto").read_text()
    enum_values = set(__import__("re").findall(r"ENTITY_PREFIX_[A-Z_]+", proto))
    for entity in catalog["ids"]:
        expected = "ENTITY_PREFIX_" + __import__("re").sub(r"(?<!^)(?=[A-Z])", "_", entity).upper().replace(" ", "_")
        assert expected in enum_values, f"{entity} has no {expected} in identity.proto"


def test_proto_lint_and_compat_are_clean():
    assert contract_compat.schema_lint() == []
    assert contract_compat.main(["--compat"]) == 0


def test_compat_detects_removed_field():
    from google.protobuf import descriptor_pb2

    def descriptor(fields):
        file = descriptor_pb2.FileDescriptorProto()
        file.name = "quansio/v1/core/test.proto"
        file.package = "quansio.v1.core"
        file.syntax = "proto3"
        message = file.message_type.add()
        message.name = "Thing"
        for number, name, type_name in fields:
            field = message.field.add()
            field.number = number
            field.name = name
            field.type = type_name
        return file

    baseline = descriptor_pb2.FileDescriptorSet()
    baseline.file.append(descriptor([(1, "id", 9), (2, "name", 9)]))
    current = descriptor_pb2.FileDescriptorSet()
    current.file.append(descriptor([(1, "id", 9)]))

    findings = contract_compat.compare(current, baseline)
    assert any("field #2 (name) removed" in f for f in findings)


def test_compat_allows_additive_field():
    from google.protobuf import descriptor_pb2

    def descriptor(fields):
        file = descriptor_pb2.FileDescriptorProto()
        file.name = "quansio/v1/core/test.proto"
        file.package = "quansio.v1.core"
        file.syntax = "proto3"
        message = file.message_type.add()
        message.name = "Thing"
        for number, name, type_name in fields:
            field = message.field.add()
            field.number = number
            field.name = name
            field.type = type_name
        return file

    baseline = descriptor_pb2.FileDescriptorSet()
    baseline.file.append(descriptor([(1, "id", 9)]))
    current = descriptor_pb2.FileDescriptorSet()
    current.file.append(descriptor([(1, "id", 9), (2, "added", 9)]))
    assert contract_compat.compare(current, baseline) == []


def test_lint_rejects_message_without_schema_version(tmp_path):
    proto = tmp_path / "schemas" / "proto" / "quansio" / "v1" / "core"
    proto.mkdir(parents=True)
    (proto / "bad.proto").write_text(
        'syntax = "proto3";\npackage quansio.v1.core;\n\nmessage Bad {\n  string id = 1;\n}\n'
    )
    findings = contract_compat.schema_lint(tmp_path, compile_protos=False)
    assert any("must carry schema_version" in f for f in findings)


def test_lint_rejects_unprefixed_enum(tmp_path):
    proto = tmp_path / "schemas" / "proto" / "quansio" / "v1" / "core"
    proto.mkdir(parents=True)
    (proto / "bad.proto").write_text(
        'syntax = "proto3";\npackage quansio.v1.core;\n\n'
        "enum Flavour {\n  VANILLA = 0;\n  CHOCOLATE = 1;\n}\n"
    )
    findings = contract_compat.schema_lint(tmp_path, compile_protos=False)
    assert any("must share one prefix" in f for f in findings)


def test_generated_bindings_are_current():
    """Rust (prost/tonic), Python (protoc) and TypeScript bindings must be regenerable."""
    assert gen_contracts.check(["rust", "python", "ts"]) == []
    assert (ROOT / "crates" / "contracts" / "src" / "generated" / "mod.rs").exists()
    assert (ROOT / "python" / "intelligence" / "contracts" / "generated" / "quansio").is_dir()
    assert (ROOT / "sdk" / "typescript" / "src" / "generated" / "public-api.d.ts").exists()


def test_generated_rust_bindings_expose_the_canonical_types():
    core = (ROOT / "crates" / "contracts" / "src" / "generated" / "quansio.v1.core.rs").read_text()
    for expected in ("pub struct RuntimeEvent", "pub enum ErrorCode", "pub enum EntityPrefix"):
        assert expected in core
    effects = (ROOT / "crates" / "contracts" / "src" / "generated" / "quansio.v1.effects.rs").read_text()
    assert "pub enum EffectClass" in effects
    grpc = (
        ROOT / "crates" / "contracts" / "src" / "generated" / "quansio.v1.intelligence.rs"
    ).read_text()
    assert "intelligence_gateway_server" in grpc or "IntelligenceGatewayServer" in grpc


def test_generated_python_bindings_use_the_canonical_import_path():
    """`quansio.v1.*` is the canonical import path; the nested alias must not be used."""
    import importlib

    module = importlib.import_module("quansio.v1.core.identity_pb2")
    assert module.Workspace(id="ws_1").id == "ws_1"
    offenders = []
    for path in sorted((ROOT / "python" / "intelligence").rglob("*.py")):
        if "contracts/generated" in str(path):
            continue
        if "intelligence.contracts.generated" in path.read_text():
            offenders.append(str(path.relative_to(ROOT)))
    assert offenders == [], f"use quansio.v1.* instead of the nested alias: {offenders}"


def test_json_schemas_validate_real_artifacts():
    from jsonschema import Draft202012Validator

    def validator(name: str):
        schema = json.loads((ROOT / "schemas" / "json" / f"{name}.schema.json").read_text())
        return Draft202012Validator(schema)

    models = yaml.safe_load((ROOT / "config" / "models.yaml").read_text())
    validator("models-config").validate(models)
    flags = yaml.safe_load((ROOT / "config" / "flags.yaml").read_text())
    validator("flags-config").validate(flags)

    summaries = sorted((ROOT / "evidence").glob("*/*/summary.json"))
    assert summaries, "at least one task evidence bundle must exist"
    for summary in summaries:
        validator("evidence-summary").validate(json.loads(summary.read_text()))


def test_json_schema_rejects_incomplete_evidence_summary():
    from jsonschema import Draft202012Validator, ValidationError

    schema = json.loads((ROOT / "schemas" / "json" / "evidence-summary.schema.json").read_text())
    validator = Draft202012Validator(schema)
    with pytest.raises(ValidationError):
        validator.validate({"task_id": "GOV-004", "commands": []})


def test_json_schema_validates_a_runtime_event_envelope():
    from jsonschema import Draft202012Validator, ValidationError

    schema = json.loads((ROOT / "schemas" / "json" / "runtime-event.schema.json").read_text())
    validator = Draft202012Validator(schema)
    event = {
        "event_id": "evt_01J8Z3K6F1N8VQ2X5W9Y0ABCDE",
        "tenant_id": "tn_01J8Z3K6F1N8VQ2X5W9Y0ABCDE",
        "sequence": 1,
        "aggregate_type": "run",
        "aggregate_id": "run_01J8Z3K6F1N8VQ2X5W9Y0ABCDE",
        "aggregate_version": 1,
        "type": "run.verification_passed",
        "schema_version": "v1",
        "occurred_at": "2026-09-12T00:00:00Z",
        "actor": {"kind": "system", "id": "runtime"},
    }
    validator.validate(event)
    invalid = dict(event, type="run.NotAThing")
    with pytest.raises(ValidationError):
        validator.validate(invalid)


def test_generated_catalog_files_declare_their_origin():
    for filename in domain_catalog.CATALOG_FILES.values():
        text = (CATALOG / filename).read_text()
        assert text.startswith("# GENERATED from DOMAIN.md"), filename
        assert "Do not hand edit" in text
