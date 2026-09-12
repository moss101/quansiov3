"""Credential custody for the model gateway (DOSSIER.md §16, D-006).

Provider credentials live in the trusted server/local-service secret custody. The gateway is
the only component that reads them, the only component that puts them on the wire, and the
only place they exist as a value: adapters receive a `SecretValue` whose `repr`/`str` are
redacted, so a request object, log record or exception text can be captured without carrying
key material.

Handle convention: a catalog handle such as `provider/anthropic` materializes from the
environment variable `QUANSIO_SECRET_PROVIDER_ANTHROPIC`. The Rust secret broker owns
materialization in production; during V8.1 development the environment is the local custody
boundary, and no credential is ever written to config or the repository.
"""

from __future__ import annotations

import os
from collections.abc import Mapping

from intelligence.model_gateway.catalog import ProviderConfig
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode

SECRET_ENV_PREFIX = "QUANSIO_SECRET_"


def env_var_for_handle(handle: str) -> str:
    """Environment variable that materializes `handle` (for example `provider/anthropic`)."""
    normalized = "".join(character if character.isalnum() else "_" for character in handle).upper()
    return f"{SECRET_ENV_PREFIX}{normalized}"


class SecretValue:
    """A materialized credential that refuses to describe itself.

    `reveal()` is the only way to obtain the value and is called exactly once per provider
    request, at the moment the authentication header is built.
    """

    __slots__ = ("_handle", "_value")

    def __init__(self, handle: str, value: str) -> None:
        self._handle = handle
        self._value = value

    @property
    def handle(self) -> str:
        return self._handle

    def reveal(self) -> str:
        return self._value

    def with_scheme(self, scheme: str) -> SecretValue:
        """A new `SecretValue` carrying `<scheme> <value>` (for `Authorization: Bearer ...`)."""
        return SecretValue(self._handle, f"{scheme} {self._value}")

    def __repr__(self) -> str:
        return f"SecretValue(handle={self._handle!r}, value=***redacted***)"

    __str__ = __repr__


class CredentialResolver:
    """Reads provider credentials from secret custody by handle; never writes them anywhere."""

    __slots__ = ("_environ",)

    def __init__(self, environ: Mapping[str, str] | None = None) -> None:
        self._environ = os.environ if environ is None else environ

    def resolve(self, provider: ProviderConfig) -> SecretValue:
        """Materialize the provider credential, or fail closed with a typed error."""
        handle = provider.resolve_credential_handle(self._environ)
        variable = env_var_for_handle(handle)
        value = self._environ.get(variable, "")
        if not value.strip():
            raise GatewayError(
                GatewayErrorCode.PROVIDER_UNAVAILABLE,
                f"credential handle '{handle}' for provider '{provider.name}' is not materialized "
                f"(expected environment variable {variable})",
                retryable=False,
            )
        return SecretValue(handle, value)
