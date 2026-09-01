#!/usr/bin/env python3
"""WebSocket + HTTP entrypoint. Listens on settings.listen_port (default 9001).

Use this for the primary service port (device WebSocket + operator API).
The image push port is exposed by `push_image.py`.
"""
from __future__ import annotations

import logging
import os
import signal
import sys
from pathlib import Path

import uvicorn

# Run from server/ so .env resolution works regardless of cwd.
_HERE = Path(__file__).resolve().parent
os.chdir(_HERE)
sys.path.insert(0, str(_HERE))

from youn_server.config import settings  # noqa: E402
from youn_server.discovery import DiscoveryServer  # noqa: E402
from youn_server.app import app  # noqa: E402


async def _serve() -> None:
    cfg = uvicorn.Config(
        app,
        host=settings.listen_host,
        port=settings.listen_port,
        log_level=settings.log_level.lower(),
        access_log=True,
        lifespan="on",
    )
    server = uvicorn.Server(cfg)

    disc = DiscoveryServer()
    await disc.start(port=settings.push_image_port)

    def _stop(*_: object) -> None:
        logging.getLogger(__name__).info("shutdown signal received")
        server.should_exit = True

    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, _stop)

    await server.serve()
    await disc.stop()


def main() -> None:
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    )
    import asyncio
    asyncio.run(_serve())


if __name__ == "__main__":
    main()
