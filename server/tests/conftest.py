"""Shared pytest fixtures.

Isolate pages/ storage to a per-test temp dir. Uses pydantic-settings'
mutability to change data_dir on the fly; the settings object is the same
instance used by the server.
"""
import os
import shutil
import tempfile
from pathlib import Path

import pytest

from youn_server.config import settings

from .device_sig import TEST_MASTER_KEY

# Tests run without operator token (auth logic is exercised in test_pairing separately)
settings.operator_token = ""


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
    """Isolate pages/ storage to a per-test temp dir."""
    tmp = tempfile.mkdtemp(prefix="pages_test_")
    orig = settings.data_dir
    settings.data_dir = Path(tmp)
    # Ensure pages subdir exists for _all_page_sources glob
    (Path(tmp) / "pages").mkdir(parents=True, exist_ok=True)
    yield
    settings.data_dir = orig
    shutil.rmtree(tmp, ignore_errors=True)
