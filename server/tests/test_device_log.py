"""Device log upload: storage layer (devicelog.py) and HTTP endpoints.

Isolation: ``isolate_pages_dir`` swaps data_dir per test; this module points
``settings.device_log_dir`` at the same temp dir so nothing lands in the
running server's data/devicelogs.
"""
from __future__ import annotations

import tempfile
from pathlib import Path

import pytest

from youn_server.config import settings
from youn_server import devicelog


@pytest.fixture(autouse=True)
def _isolate_device_log_dir():
    """Point device_log_dir at a fresh temp dir and restore after."""
    tmp = tempfile.mkdtemp(prefix="devicelog_test_")
    orig = settings.device_log_dir
    settings.device_log_dir = Path(tmp)
    yield
    settings.device_log_dir = orig


def test_append_then_tail_round_trips():
    written = devicelog.append_lines(
        "NOTE4C-TEST", "2026-09-28T12:30:11+08:00",
        "I (27930) CustomLcdDisplay: EPD busy wait: 15000 ms\n"
        "W (27931) CustomLcdDisplay: EPD busy wait: 20000 ms\n",
        dropped=0,
    )
    assert written == 2
    lines, truncated = devicelog.tail_lines("NOTE4C-TEST", 10)
    assert truncated is False
    assert lines[0].endswith("CustomLcdDisplay: EPD busy wait: 15000 ms")
    assert lines[0].startswith("2026-09-28T12:30:11+08:00 ")
    assert lines[1].endswith("EPD busy wait: 20000 ms")


def test_dropped_marker_is_written():
    devicelog.append_lines("NOTE4C-TEST", "2026-09-28T12:30:11+08:00", "line a\n", dropped=7)
    text = (settings.device_log_dir / "NOTE4C-TEST.log").read_text()
    assert "dropped 7 lines" in text


def test_no_dropped_marker_when_zero():
    devicelog.append_lines("NOTE4C-TEST", "2026-09-28T12:30:11+08:00", "line a\n", dropped=0)
    text = (settings.device_log_dir / "NOTE4C-TEST.log").read_text()
    assert "dropped" not in text


def test_tail_reports_truncation():
    body = "".join(f"line {i}\n" for i in range(50))
    devicelog.append_lines("NOTE4C-TEST", "2026-09-28T12:30:11+08:00", body, dropped=0)
    lines, truncated = devicelog.tail_lines("NOTE4C-TEST", 10)
    assert len(lines) == 10
    assert truncated is True
    assert lines[-1].endswith("line 49")


def test_tail_unknown_device_is_empty():
    lines, truncated = devicelog.tail_lines("NO-SUCH-DEVICE", 10)
    assert lines == []
    assert truncated is False


def test_device_id_with_path_separator_is_rejected():
    """A device id is a filename component; traversal must not reach disk."""
    with pytest.raises(ValueError):
        devicelog.append_lines("../escape", "2026-09-28T12:30:11+08:00", "x\n", dropped=0)
