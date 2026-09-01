#!/usr/bin/env python3
"""HTTP-only entrypoint for image push + OTA + UDP discovery on a single port.

Use this if you want to expose the image/OTA HTTP API on a separate port
from the WebSocket service, OR if you want to co-host with a Caddy/nginx
reverse proxy that only forwards HTTP.

The WS app (llmserve.py) starts its own UDP discovery on the same port as
this HTTP server — so you should NOT run both at once. Either run this
HTTP-only entrypoint and disable the UDP in llmserve, or run llmserve
and skip push_image.py.

Default port 8766 (matches README §39).
"""
from __future__ import annotations

import logging
import os
import signal
import sys
from pathlib import Path

import uvicorn

_HERE = Path(__file__).resolve().parent
os.chdir(_HERE)
sys.path.insert(0, str(_HERE))

from youn_server.config import settings  # noqa: E402
from youn_server.discovery import DiscoveryServer  # noqa: E402
from youn_server.app import app  # noqa: E402


async def _serve() -> None:
    cfg = uvicorn.Config(
        app,
        host=settings.push_image_host,
        port=settings.push_image_port,
        log_level=settings.log_level.lower(),
        access_log=True,
        lifespan="on",
    )
    server = uvicorn.Server(cfg)

    # NOTE: WS endpoint is still available on this app. If you want pure HTTP,
    # comment this out — but having WS + HTTP on the same port is convenient
    # when fronted by a reverse proxy.
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
