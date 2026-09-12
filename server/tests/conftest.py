"""Shared pytest fixtures.

Isolate pages/ storage to a per-test temp dir. Uses pydantic-settings'
mutability to change data_dir on the fly; the settings object is the same
instance used by the server.
"""
import logging
import os
import shutil
import tempfile
from logging.handlers import RotatingFileHandler
from pathlib import Path

import pytest

from youn_server.config import settings

from .device_sig import TEST_MASTER_KEY

# Tests run without operator token (auth logic is exercised in test_pairing separately)
settings.operator_token = ""

# Isolate the device/pairing database from production ``data/devices.db``.
# ``youn_server.devices.registry`` and ``app._pairing_store`` are constructed at
# *import* time from ``settings.devices_db``, and the test modules that import
# them are collected after this module — so pointing the setting at a throwaway
# path here is enough to keep the suite off real pairing data.
_DEVICES_DB_DIR = tempfile.mkdtemp(prefix="devices_test_")
settings.devices_db = Path(_DEVICES_DB_DIR) / "devices.db"


@pytest.fixture(autouse=True)
def _device_signature_key():
    """Provide a MASTER_KEY so pair-start's key gate passes for the whole suite.

    Device-signature tests that need an empty key (test_master_key_required)
    override and restore it themselves.
    """
    settings.master_key = TEST_MASTER_KEY
    yield
    settings.master_key = ""

@pytest.fixture(autouse=True)
def isolate_pages_dir():
    """Isolate pages/, uploads/ and the log file to per-test temp dirs.

    The log file needs real work, not just a settings change: the rotating
    handler is created at import (config._configure_root_logging) and keeps its
    path, so a test run would append its own `page upserted device=...` lines to
    the running server's log where they read as live activity. Measured: one
    suite run added 286 lines to server/data/server.log before this; 27 after,
    the remainder being import/collection-time logging that happens before an
    autouse fixture can run.
    """
    tmp = tempfile.mkdtemp(prefix="pages_test_")
    orig = (settings.data_dir, settings.uploads_dir, settings.log_dir, settings.log_file)
    settings.data_dir = Path(tmp)
    settings.uploads_dir = Path(tmp) / "uploads"
    settings.log_dir = Path(tmp)
    settings.log_file = "test.log"
    (Path(tmp) / "pages").mkdir(parents=True, exist_ok=True)
    settings.uploads_dir.mkdir(parents=True, exist_ok=True)

    redirected: list[tuple[RotatingFileHandler, str]] = []
    for h in logging.getLogger().handlers:
        if isinstance(h, RotatingFileHandler):
            redirected.append((h, h.baseFilename))
            h.close()                      # drops the stream, so the next emit reopens
            h.baseFilename = str(Path(tmp) / "test.log")
    try:
        yield
    finally:
        for h, original in redirected:
            h.close()
            h.baseFilename = original
        (settings.data_dir, settings.uploads_dir,
         settings.log_dir, settings.log_file) = orig
        shutil.rmtree(tmp, ignore_errors=True)
