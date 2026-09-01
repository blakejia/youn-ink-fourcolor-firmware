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
