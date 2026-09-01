"""UDP device discovery.

Wire format (matches `server/mock_client.py`):

    request  (device → server, UDP broadcast):
        {"type": "discover_host", "service": "<service_tag>",
         "deviceId": "...", "nonce": "..."}

    reply    (server → device, UDP unicast):
        {"wsUrl": "wss://...", "httpBase": "https://...",
         "serverId": "...", "nonce": "<echo>",
         "signature": "<hex hmac-sha256 of canonical message>"}

    canonical_message = "discover_reply|<server_id>|<server_name>|<ws_url>|<nonce>"

Signature lets a device verify the reply came from someone who holds the
shared secret, not an attacker on the LAN spoofing the server. The firmware
must have the same secret compiled in.

This module runs a background asyncio task that listens on PUSH_IMAGE_PORT/UDP.
"""
from __future__ import annotations

import asyncio
import hashlib
import hmac
import json
import logging
import socket
from typing import Optional

from .config import settings

log = logging.getLogger(__name__)

DISCOVERY_REQ_TYPE = "discover_host"
DISCOVERY_REPLY_TYPE = "discover_reply"

# How long a server_id / secret stays valid (cosmetic — devices don't cache state).
_NONCE_ECHO_LIMIT = 256


def sign_reply(host_id: str, host_name: str, ws_url: str, nonce: str, secret: str) -> str:
    """Build the HMAC-SHA256 signature used in `discover_reply`.

    Kept identical to `mock_client.sign_discovery_reply` so the firmware
    verifier (whatever it is) does not need a separate code path.
    """
    message = f"discover_reply|{host_id}|{host_name}|{ws_url}|{nonce}"
    return hmac.new(
        secret.encode("utf-8"),
        message.encode("utf-8"),
        hashlib.sha256,
    ).hexdigest()


def build_reply(host_id: str, host_name: str, ws_url: str, http_base: str, nonce: str) -> dict:
    return {
        "type": DISCOVERY_REPLY_TYPE,
        "wsUrl": ws_url,
        "httpBase": http_base,
        "serverId": host_id,
        "serverName": host_name,
        "nonce": nonce[:_NONCE_ECHO_LIMIT],
        "signature": sign_reply(host_id, host_name, ws_url, nonce, settings.discovery_shared_secret),
    }


class DiscoveryServer:
    """Asyncio UDP listener for `discover_host` broadcasts.

    Reuses the configured PUSH_IMAGE_PORT — the firmware already broadcasts to
    this port for image/OTA HTTP, so we co-host UDP discovery on the same number.
    A dedicated port would be cleaner but would require a firmware change.
    """

    def __init__(
        self,
        host_id: str = "youn-server-1",
        host_name: str = "youn-ink-server",
    ) -> None:
        self.host_id = host_id
        self.host_name = host_name
        self._transport: Optional[asyncio.DatagramTransport] = None
        self._stopped = asyncio.Event()

    async def start(self, port: Optional[int] = None) -> int:
        port = port or settings.push_image_port
        loop = asyncio.get_running_loop()

        # Allow address reuse so a quick restart doesn't have to wait for TIME_WAIT.
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        sock.bind(("0.0.0.0", port))

        transport, _ = loop.run_in_executor(None, lambda: None), None  # placeholder
        transport, _ = await loop.create_datagram_endpoint(
            lambda: DiscoveryProtocol(self),
            sock=sock,
        )
        self._transport = transport
        log.info("discovery listening on UDP/%d", port)
        return port

    async def stop(self) -> None:
        if self._transport is not None:
            self._transport.close()
            self._transport = None
        self._stopped.set()

    def handle_packet(self, data: bytes, addr: tuple[str, int]) -> None:
        try:
            req = json.loads(data.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as e:
            log.debug("discovery: bad json from %s: %s", addr, e)
            return

        if req.get("type") != DISCOVERY_REQ_TYPE:
            return
        if req.get("service") != settings.discovery_service_tag:
            log.debug("discovery: wrong service tag from %s: %r", addr, req.get("service"))
            return

        device_id = str(req.get("deviceId", "unknown"))[:128]
        nonce = str(req.get("nonce", ""))

        reply = build_reply(
            host_id=self.host_id,
            host_name=self.host_name,
            ws_url=settings.public_ws_url,
            http_base=settings.public_http_base,
            nonce=nonce,
        )
        body = json.dumps(reply, ensure_ascii=False).encode("utf-8")
        try:
            assert self._transport is not None
            self._transport.sendto(body, addr)
            log.info("discovery: replied to %s for device %s", addr, device_id)
        except OSError as e:
            log.warning("discovery: sendto %s failed: %s", addr, e)


class DiscoveryProtocol(asyncio.DatagramProtocol):
    def __init__(self, server: DiscoveryServer) -> None:
        self._server = server

    def datagram_received(self, data: bytes, addr: tuple[str, int]) -> None:
        # Defer to the event loop — no blocking work in this method.
        asyncio.get_event_loop().create_task(self._server._async_handle(data, addr))

    def error_received(self, exc: Exception) -> None:
        log.warning("discovery: socket error: %s", exc)


# Patch handle_packet into async helper for clean scheduling.
async def _async_handle(self: DiscoveryServer, data: bytes, addr: tuple[str, int]) -> None:
    self.handle_packet(data, addr)


DiscoveryServer._async_handle = _async_handle  # type: ignore[attr-defined]
