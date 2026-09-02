"""Tests for the FastMCP notification server mounted on /mcp.

Streamable-HTTP transport requires:
  * Request must include ``Accept: application/json, text/event-stream``.
  * Client must first ``initialize`` then send ``notifications/initialized``
    before any ``tools/call``. A bare POST without session yields 400/202;
    a GET without Accept yields 406.

These tests exercise the real protocol — not a private shortcut — so they
will catch regressions in the mount/lifespan wiring.
"""
from __future__ import annotations

import pytest
from fastapi.testclient import TestClient

from youn_server.app import create_app
from youn_server.devices import registry


@pytest.fixture()
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


def _mcp_headers(session_id):
    h = {
        'Accept': 'application/json, text/event-stream',
        'Content-Type': 'application/json',
    }
    if session_id:
        h['mcp-session-id'] = session_id
    return h


def _initialize(client):
    r = client.post(
        '/mcp',
        headers=_mcp_headers(None),
        json={
            'jsonrpc': '2.0',
            'id': 1,
            'method': 'initialize',
            'params': {
                'protocolVersion': '2025-03-26',
                'capabilities': {},
                'clientInfo': {'name': 'test-mcp', 'version': '1.0'},
            },
        },
    )
    return r.status_code, r.headers.get('mcp-session-id')


def test_mcp_probe(client):
    r = client.get('/mcp')
    assert r.status_code != 404, f'/mcp not mounted (got {r.status_code})'


def test_mcp_tool_call_push_notification(client):
    status, sid = _initialize(client)
    assert status == 200, f'initialize failed: {status} {sid}'
    assert sid, 'initialize did not return session id'

    r = client.post(
        '/mcp',
        headers=_mcp_headers(sid),
        json={'jsonrpc': '2.0', 'method': 'notifications/initialized'},
    )
    assert r.status_code in (200, 202), (
        f'initialized notification got {r.status_code}: {r.text}'
    )

    registry.upsert('DEV-MCP', 'NOTE4C')
    registry.approve('DEV-MCP')

    r = client.post(
        '/mcp',
        headers=_mcp_headers(sid),
        json={
            'jsonrpc': '2.0',
            'id': 4,
            'method': 'tools/call',
            'params': {
                'name': 'push_notification',
                'arguments': {
                    'device_id': 'DEV-MCP',
                    'title': 'hello',
                    'body': 'world',
                },
            },
        },
    )
    assert r.status_code == 200, (
        f'tools/call push_notification returned {r.status_code}: {r.text}'
    )
    body = r.text
    assert '"ok":true' in body or '"ok": true' in body, (
        f'push_notification did not return ok=true: {body}'
    )
    assert 'DEV-MCP' in body
