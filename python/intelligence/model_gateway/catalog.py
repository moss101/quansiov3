"""Model catalog: the only source of provider, model id, capability, context and cost data.

`config/models.yaml` (DOSSIER.md §7, D-018) is validated against
`schemas/json/models-config.schema.json` by the repository gates. The gateway reads it and
never carries a model identifier as a source constant: every wire model id the adapters send
comes from `ModelSpec.model_id`.

`ModelCatalog.from_mapping` exists so tests and the composition root can build a catalog from
data (for example pointing a provider at a local endpoint) without a hard-coded model id.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from enum import StrEnum
from pathlib import Path
from typing import Any

import yaml

from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode, ProviderConfigError

CATALOG_RELATIVE_PATH = Path("config") / "models.yaml"


class ProviderKind(StrEnum):
    """Provider wire families the gateway ships adapters for (OD-004)."""

    ANTHROPIC = "anthropic"
    OPENAI = "openai"
    OPENAI_COMPATIBLE = "openai_compatible"


@dataclass(frozen=True, slots=True)
class ProviderConfig:
    """One provider entry: kind, endpoint and the secret handle that unlocks it."""

    name: str
    kind: ProviderKind
    base_url: str | None
    base_url_env: str | None
    credential_handle: str | None
    credential_handle_env: str | None

    def resolve_base_url(self, environ: Mapping[str, str]) -> str:
        """Endpoint to call: the literal `base_url`, else the variable named by `base_url_env`."""
        if self.base_url:
            return self.base_url.rstrip("/")
        if self.base_url_env:
            value = environ.get(self.base_url_env, "").strip()
            if value:
                return value.rstrip("/")
        raise ProviderConfigError(
            f"provider '{self.name}' has no base_url and {self.base_url_env or 'no *_env variable'} is unset"
        )

    def resolve_credential_handle(self, environ: Mapping[str, str]) -> str:
        """Secret handle for this provider, following the `credential_handle_env` indirection."""
        if self.credential_handle:
            return self.credential_handle
        if self.credential_handle_env:
            value = environ.get(self.credential_handle_env, "").strip()
            if value:
                return value
            raise ProviderConfigError(
                f"provider '{self.name}' credential handle environment "
                f"variable {self.credential_handle_env} is unset"
            )
        raise ProviderConfigError(f"provider '{self.name}' declares no credential handle")


@dataclass(frozen=True, slots=True)
class ModelSpec:
    """One catalog model: the identity, capabilities and policy inputs for a call."""

    id: str
    provider: str
    model_id: str
    capabilities: frozenset[str]
    context_window: int
    cost_class: str
    dlp_eligible: bool
    #: Vector width an `embeddings` model produces; required for that capability, absent
    #: otherwise. The derived embedding index (INT-011) pins its own width and refuses a
    #: route that does not match it, so this is the declared side of that check.
    embedding_dimensions: int | None = None

    def supports(self, capability: str) -> bool:
        return capability in self.capabilities


@dataclass(frozen=True, slots=True)
class ModelCatalog:
    """Validated view of `config/models.yaml`."""

    version: int
    providers: Mapping[str, ProviderConfig]
    models: Mapping[str, ModelSpec]
    routing_primary: str
    fallback_order: tuple[str, ...]
    source: str

    # -- constructors --------------------------------------------------------------

    @classmethod
    def from_mapping(cls, data: Mapping[str, Any], *, source: str = "<mapping>") -> ModelCatalog:
        """Build and validate a catalog from decoded YAML/JSON data."""
        raw_providers = data.get("providers")
        raw_models = data.get("models")
        raw_routing = data.get("routing")
        version = data.get("version")
        if not isinstance(raw_providers, dict) or not raw_providers:
            raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, f"{source}: providers missing")
        if not isinstance(raw_models, list) or not raw_models:
            raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, f"{source}: models missing")
        if not isinstance(raw_routing, dict):
            raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, f"{source}: routing missing")
        if not isinstance(version, int):
            raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, f"{source}: version missing")

        providers: dict[str, ProviderConfig] = {}
        for name, entry in raw_providers.items():
            if not isinstance(entry, dict):
                raise GatewayError(
                    GatewayErrorCode.VALIDATION_SCHEMA,
                    f"{source}: provider '{name}' is not a mapping",
                )
            kind = entry.get("kind")
            try:
                parsed_kind = ProviderKind(str(kind))
            except ValueError as error:
                raise GatewayError(
                    GatewayErrorCode.VALIDATION_SCHEMA,
                    f"{source}: provider '{name}' has unknown kind {kind!r}",
                ) from error
            providers[str(name)] = ProviderConfig(
                name=str(name),
                kind=parsed_kind,
                base_url=_opt_str(entry.get("base_url")),
                base_url_env=_opt_str(entry.get("base_url_env")),
                credential_handle=_opt_str(entry.get("credential_handle")),
                credential_handle_env=_opt_str(entry.get("credential_handle_env")),
            )

        models: dict[str, ModelSpec] = {}
        for entry in raw_models:
            models.update(_parse_model(entry, providers, source))

        strategy = raw_routing.get("strategy")
        if strategy != "deterministic":
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA,
                f"{source}: routing.strategy must be 'deterministic' (no pre-call router model)",
            )
        primary = raw_routing.get("primary")
        if not isinstance(primary, str) or primary not in models:
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA,
                f"{source}: routing.primary {primary!r} is not a catalog model id",
            )
        fallback = raw_routing.get("fallback_order", [])
        if not isinstance(fallback, list) or not all(isinstance(item, str) for item in fallback):
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA,
                f"{source}: routing.fallback_order must be a list of model ids",
            )
        unknown = sorted(set(fallback) - set(models))
        if unknown:
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA,
                f"{source}: routing.fallback_order names unknown model id(s) {unknown}",
            )

        return cls(
            version=version,
            providers=providers,
            models=models,
            routing_primary=primary,
            fallback_order=tuple(fallback),
            source=source,
        )

    @classmethod
    def load(cls, path: Path | None = None) -> ModelCatalog:
        """Load the catalog from `path` or the repository's `config/models.yaml`."""
        resolved = path if path is not None else default_catalog_path()
        try:
            raw = yaml.safe_load(resolved.read_text(encoding="utf-8"))
        except OSError as error:
            raise GatewayError(
                GatewayErrorCode.INTERNAL, f"model catalog {resolved} is unreadable"
            ) from error
        except yaml.YAMLError as error:  # pragma: no cover - malformed operator config
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA, f"model catalog {resolved} is not valid YAML"
            ) from error
        if not isinstance(raw, dict):
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA, f"model catalog {resolved} is not a mapping"
            )
        return cls.from_mapping(raw, source=str(resolved))

    # -- lookups -------------------------------------------------------------------

    def model(self, model_id: str) -> ModelSpec:
        """Catalog model by id; a missing id is a route failure, never a silent default."""
        spec = self.models.get(model_id)
        if spec is None:
            raise GatewayError(
                GatewayErrorCode.ROUTE_UNAVAILABLE, f"model id {model_id!r} is not in the catalog"
            )
        return spec

    def provider_for(self, spec: ModelSpec) -> ProviderConfig:
        provider = self.providers.get(spec.provider)
        if provider is None:
            raise ProviderConfigError(
                f"model '{spec.id}' names provider '{spec.provider}' which is not in the catalog"
            )
        return provider

    def primary(self) -> ModelSpec:
        return self.model(self.routing_primary)


def default_catalog_path() -> Path:
    """Locate `config/models.yaml` by walking up from this module to the repository root."""
    for parent in Path(__file__).resolve().parents:
        candidate = parent / CATALOG_RELATIVE_PATH
        if candidate.is_file():
            return candidate
    raise GatewayError(
        GatewayErrorCode.INTERNAL,
        f"{CATALOG_RELATIVE_PATH.as_posix()} not found above {Path(__file__).resolve()}",
    )


def _parse_model(entry: object, providers: Mapping[str, ProviderConfig], source: str) -> dict[str, ModelSpec]:
    if not isinstance(entry, dict):
        raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, f"{source}: a model entry is not a mapping")
    model_id = entry.get("id")
    provider_name = entry.get("provider")
    wire_id = entry.get("model_id")
    capabilities = entry.get("capabilities")
    context_window = entry.get("context_window")
    cost_class = entry.get("cost_class")
    dlp_eligible = entry.get("dlp_eligible")
    embedding_dimensions = entry.get("embedding_dimensions")
    if not isinstance(model_id, str) or not model_id:
        raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, f"{source}: model id missing")
    if not isinstance(provider_name, str) or provider_name not in providers:
        raise GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA,
            f"{source}: model '{model_id}' names unknown provider {provider_name!r}",
        )
    if not isinstance(wire_id, str) or not wire_id:
        raise GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA, f"{source}: model '{model_id}' has no model_id"
        )
    if not isinstance(capabilities, list) or not all(isinstance(item, str) for item in capabilities):
        raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, f"{source}: model '{model_id}' capabilities")
    if not isinstance(context_window, int) or context_window <= 0:
        raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, f"{source}: model '{model_id}' context_window")
    if not isinstance(cost_class, str) or not cost_class:
        raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, f"{source}: model '{model_id}' cost_class")
    if not isinstance(dlp_eligible, bool):
        raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, f"{source}: model '{model_id}' dlp_eligible")
    if embedding_dimensions is not None and (
        not isinstance(embedding_dimensions, int) or embedding_dimensions <= 0
    ):
        raise GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA,
            f"{source}: model '{model_id}' embedding_dimensions must be a positive integer",
        )
    if "embeddings" in capabilities and embedding_dimensions is None:
        # An embedding route whose width is undeclared cannot be checked against the width the
        # derived index pins, so it is refused at load rather than discovered when a vector
        # cannot be written.
        raise GatewayError(
            GatewayErrorCode.VALIDATION_SCHEMA,
            f"{source}: model '{model_id}' declares the `embeddings` capability without embedding_dimensions",
        )
    return {
        model_id: ModelSpec(
            id=model_id,
            provider=provider_name,
            model_id=wire_id,
            capabilities=frozenset(capabilities),
            context_window=context_window,
            cost_class=cost_class,
            dlp_eligible=dlp_eligible,
            embedding_dimensions=embedding_dimensions,
        )
    }


def _opt_str(value: object) -> str | None:
    if value is None:
        return None
    text = str(value).strip()
    return text or None
