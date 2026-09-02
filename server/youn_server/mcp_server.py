"""FastMCP server exposing notification tools on /mcp (streamable-http)."""
from __future__ import annotations

from dataclasses import asdict

from fastmcp import FastMCP

from .config import settings
from . import notify_store as ns
from .devices import registry

# fastmcp 4.x: no `transport=` kwarg on FastMCP(). Streamable HTTP
# transport is selected when mounting via http_app(transport=...).
mcp = FastMCP("youn-notify")


@mcp.tool
def push_notification(device_id: str, title: str, body: str,
                      ttl_sec: int = 300) -> dict:
    """Create a pending notification for a device."""
    if not any(d.device_id == device_id
               for d in registry.list_all(only_trusted=True)):
        raise ValueError("device not trusted")
    n = ns.get_store().enqueue(device_id, title, body, ttl_sec)
    return {"ok": True, "notification": asdict(n)}


@mcp.tool
def list_notifications(device_id: str = "", limit: int = 20) -> dict:
    """List recent notifications (all devices or filtered by device_id)."""
    items = ns.get_store().recent(device_id=device_id, limit=limit)
    return {"notifications": [asdict(n) for n in items]}


@mcp.tool
def ack_notification(notification_id: str, decision: str) -> dict:
    """Mark a notification as agreed or rejected."""
    if decision not in ("agree", "reject"):
        raise ValueError("decision must be agree|reject")
    n = ns.get_store().ack(notification_id, decision)
    if n is None:
        raise ValueError("notification not found")
    return {"ok": True, "status": n.status, "decision": n.decision}
