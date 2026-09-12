"""Intelligence gateway process: transport selection, loopback binding and lifecycle.

The service speaks the generated `quansio.v1.intelligence.IntelligenceGateway` gRPC contract
(DOSSIER.md §4.2). Two transports are supported:

  * `uds` — Unix domain socket, the co-located Rust ↔ Python default;
  * `tcp` — loopback TCP, used by tests and by local tooling.

Non-loopback TCP binding is refused: split deployments use mTLS (DOSSIER.md §4.2), which this
task does not implement. Startup and shutdown logging is structured (one JSON object per line)
and never contains credentials or request payloads.
"""

from __future__ import annotations

import importlib.metadata
import json
import logging
import os
import socket
import time
from collections.abc import Mapping
from concurrent import futures
from dataclasses import dataclass
from enum import StrEnum
from ipaddress import ip_address
from pathlib import Path

import grpc
from quansio.v1.intelligence import service_pb2, service_pb2_grpc

from intelligence.model_gateway import ModelGateway
from intelligence.server.servicer import (
    IMPLEMENTED_METHODS,
    SERVICER_LOGGER_NAME,
    IntelligenceGatewayServicer,
)

DEFAULT_UDS_PATH = "/tmp/quansio-intelligence.sock"
DEFAULT_TCP_PORT = 50065

# Service identity for operators: the version comes from the installed distribution, never a
# duplicated source constant, and the contract name from the generated descriptor.
CONTRACT_SERVICE_NAME = service_pb2.DESCRIPTOR.services_by_name["IntelligenceGateway"].full_name

_ENV_PREFIX = "QUANSIO_INTELLIGENCE_"


def gateway_version() -> str:
    """Installed `quansio-intelligence` version, or a marked fallback for source checkouts."""
    try:
        return importlib.metadata.version("quansio-intelligence")
    except importlib.metadata.PackageNotFoundError:
        return "0.0.0+unknown"


class Transport(StrEnum):
    """Supported bind transports."""

    UDS = "uds"
    TCP = "tcp"


def is_loopback_host(host: str) -> bool:
    """True for `localhost` and for any IPv4/IPv6 loopback literal."""
    if host.strip().lower() == "localhost":
        return True
    try:
        return ip_address(host.strip()).is_loopback
    except ValueError:
        return False


@dataclass(frozen=True, slots=True)
class ServerConfig:
    """Bind and lifecycle configuration; every field is overridable from the environment."""

    transport: Transport = Transport.UDS
    uds_path: str = DEFAULT_UDS_PATH
    host: str = "127.0.0.1"
    port: int = DEFAULT_TCP_PORT
    max_workers: int = 4
    drain_grace_seconds: float = 5.0

    def __post_init__(self) -> None:
        if self.transport is Transport.UDS:
            if not self.uds_path:
                raise ValueError("uds_path must not be empty for the uds transport")
            if not Path(self.uds_path).is_absolute():
                raise ValueError("uds_path must be absolute so both peers resolve the same socket")
        elif not is_loopback_host(self.host):
            raise ValueError(
                f"refusing to bind non-loopback host {self.host!r}: split deployments require "
                "mTLS, which is not implemented (DOSSIER.md §4.2)"
            )
        if not 0 <= self.port <= 65535:
            raise ValueError("port must be within 0..65535")
        if self.max_workers < 1:
            raise ValueError("max_workers must be at least 1")
        if self.drain_grace_seconds < 0:
            raise ValueError("drain_grace_seconds must not be negative")

    @classmethod
    def from_env(cls, environ: Mapping[str, str] | None = None) -> ServerConfig:
        """Build a config from `QUANSIO_INTELLIGENCE_*` variables (explicit args win)."""
        env = os.environ if environ is None else environ
        defaults = cls()
        transport = env.get(f"{_ENV_PREFIX}TRANSPORT", str(defaults.transport))
        try:
            parsed_transport = Transport(transport.strip().lower())
        except ValueError as error:
            raise ValueError(
                f"{_ENV_PREFIX}TRANSPORT must be one of {[str(t) for t in Transport]}"
            ) from error
        return cls(
            transport=parsed_transport,
            uds_path=env.get(f"{_ENV_PREFIX}UDS", defaults.uds_path),
            host=env.get(f"{_ENV_PREFIX}HOST", defaults.host),
            port=int(env.get(f"{_ENV_PREFIX}PORT", str(defaults.port))),
            max_workers=int(env.get(f"{_ENV_PREFIX}MAX_WORKERS", str(defaults.max_workers))),
            drain_grace_seconds=float(
                env.get(f"{_ENV_PREFIX}DRAIN_GRACE_SECONDS", str(defaults.drain_grace_seconds))
            ),
        )


def _remove_stale_socket(path: Path) -> None:
    """Remove a leftover UDS file; refuse when another gateway is still listening."""
    if not path.exists():
        return
    if not path.is_socket():
        raise RuntimeError(f"{path} exists and is not a unix socket")
    probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        probe.settimeout(0.5)
        probe.connect(str(path))
    except OSError:
        path.unlink()
        return
    finally:
        probe.close()
    raise RuntimeError(f"another intelligence gateway is already listening on {path}")


class IntelligenceServer:
    """Owns the gRPC server, its worker pool and graceful draining."""

    def __init__(
        self,
        config: ServerConfig,
        *,
        servicer: IntelligenceGatewayServicer | None = None,
        logger: logging.Logger | None = None,
    ) -> None:
        self._config = config
        # Composition root: the process builds the one gateway every model call goes through
        # (INT-002/D-006). Provider credentials are materialized by the gateway, never here.
        self._servicer = (
            servicer
            if servicer is not None
            else IntelligenceGatewayServicer(gateway=ModelGateway.from_environment())
        )
        self._logger = logger if logger is not None else logging.getLogger(SERVICER_LOGGER_NAME)
        self._server: grpc.Server | None = None
        self._address: str | None = None

    @property
    def config(self) -> ServerConfig:
        return self._config

    @property
    def address(self) -> str | None:
        """Bound address once started: the UDS path or `host:port`."""
        return self._address

    @property
    def servicer(self) -> IntelligenceGatewayServicer:
        return self._servicer

    def start(self) -> str:
        """Bind, register the servicer and start serving; returns the bound address."""
        if self._server is not None:
            raise RuntimeError("intelligence gateway is already started")
        grpc_server = grpc.server(
            futures.ThreadPoolExecutor(
                max_workers=self._config.max_workers,
                thread_name_prefix="intelligence-gateway",
            )
        )
        # Generated gRPC stub is unannotated; its handlers are the canonical contract surface.
        service_pb2_grpc.add_IntelligenceGatewayServicer_to_server(  # type: ignore[no-untyped-call]
            self._servicer, grpc_server
        )
        target = self._bind_target()
        bound = grpc_server.add_insecure_port(target)
        if bound == 0:
            raise RuntimeError(f"failed to bind intelligence gateway to {target}")
        # Port 0 means "any free port": report the port the OS actually assigned.
        address = (
            self._config.uds_path
            if self._config.transport is Transport.UDS
            else f"{self._config.host}:{bound}"
        )
        grpc_server.start()
        self._server = grpc_server
        self._address = address
        self._log(
            {
                "event": "intelligence.gateway.started",
                "version": gateway_version(),
                "contract": CONTRACT_SERVICE_NAME,
                "transport": str(self._config.transport),
                "address": address,
                "max_workers": self._config.max_workers,
                "drain_grace_seconds": self._config.drain_grace_seconds,
                "implemented_rpcs": sorted(IMPLEMENTED_METHODS),
            }
        )
        return address

    def _bind_target(self) -> str:
        if self._config.transport is Transport.UDS:
            path = Path(self._config.uds_path)
            _remove_stale_socket(path)
            return f"unix://{path}"
        return f"{self._config.host}:{self._config.port}"

    def stop(self, grace_seconds: float | None = None) -> None:
        """Drain in-flight calls, stop serving and release the socket."""
        grpc_server = self._server
        if grpc_server is None:
            return
        grace = self._config.drain_grace_seconds if grace_seconds is None else grace_seconds
        self._servicer.begin_drain()
        started_at = time.monotonic()
        idle = self._servicer.wait_for_idle(grace)
        remaining = max(grace - (time.monotonic() - started_at), 0.0)
        stopped = grpc_server.stop(remaining)
        stopped.wait(timeout=remaining + 5.0)
        self._server = None
        if self._config.transport is Transport.UDS:
            Path(self._config.uds_path).unlink(missing_ok=True)
        self._log(
            {
                "event": "intelligence.gateway.stopped",
                "transport": str(self._config.transport),
                "address": self._address,
                "drained": idle,
                "in_flight": self._servicer.in_flight,
            }
        )
        self._address = None

    def _log(self, fields: Mapping[str, object]) -> None:
        # Only the fields assembled here are logged: never environment values or payloads.
        self._logger.info(json.dumps(dict(fields), sort_keys=True))
