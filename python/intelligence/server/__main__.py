"""Run the intelligence gateway: `python -m intelligence.server`.

Configuration comes from `QUANSIO_INTELLIGENCE_*` environment variables (see
`intelligence.server.app.ServerConfig`), overridable by the flags below. SIGTERM/SIGINT start
a graceful shutdown that drains in-flight calls before the process exits.
"""

from __future__ import annotations

import argparse
import logging
import signal
import sys
import threading
from collections.abc import Sequence

from intelligence.server.app import (
    CONTRACT_SERVICE_NAME,
    IntelligenceServer,
    ServerConfig,
    Transport,
    gateway_version,
)

LOG_FORMAT = "%(asctime)s %(levelname)s %(name)s %(message)s"


def _parse_args(argv: Sequence[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(prog="intelligence.server", description=__doc__)
    parser.add_argument("--transport", choices=[str(t) for t in Transport])
    parser.add_argument("--uds", dest="uds_path")
    parser.add_argument("--host")
    parser.add_argument("--port", type=int)
    parser.add_argument("--log-level", default="INFO")
    parser.add_argument(
        "--version",
        action="store_true",
        help="print the gateway version and the served contract, then exit",
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = _parse_args(argv)
    if args.version:
        print(f"quansio-intelligence {gateway_version()} (contract {CONTRACT_SERVICE_NAME})")
        return 0
    logging.basicConfig(level=args.log_level.upper(), format=LOG_FORMAT, stream=sys.stderr)
    base = ServerConfig.from_env()
    config = ServerConfig(
        transport=Transport(args.transport) if args.transport else base.transport,
        uds_path=args.uds_path or base.uds_path,
        host=args.host or base.host,
        port=args.port if args.port is not None else base.port,
        max_workers=base.max_workers,
        drain_grace_seconds=base.drain_grace_seconds,
    )
    server = IntelligenceServer(config)
    stop_requested = threading.Event()

    def _request_stop(signum: int, _frame: object) -> None:
        logging.getLogger("intelligence.server").info("received signal %s; draining", signum)
        stop_requested.set()

    signal.signal(signal.SIGTERM, _request_stop)
    signal.signal(signal.SIGINT, _request_stop)

    server.start()
    try:
        while not stop_requested.wait(timeout=1.0):
            pass
    finally:
        server.stop(config.drain_grace_seconds)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
