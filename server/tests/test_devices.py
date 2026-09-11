"""The registry must refuse to start against a database it cannot write.

SQLite opens a database file read-only *without raising* when it cannot write
it, so a server whose permissions are wrong starts happily, serves reads, and
fails every write — the schema statements in the constructor are all
``IF NOT EXISTS`` no-ops on an existing file, and the ALTER below swallowed the
one error that would have surfaced. We ran that way for about six hours before
noticing, with the device's pairing 500ing the whole time.
"""
import pytest

from youn_server.devices import DeviceRegistry


def test_a_read_only_database_is_rejected_loudly(tmp_path):
    db = tmp_path / "devices.db"
    DeviceRegistry(db)  # first run creates the file and the schema
    db.chmod(0o444)  # SQLite now opens it read-only, silently

    with pytest.raises(RuntimeError, match="not writable"):
        DeviceRegistry(db)


def test_the_error_names_the_path_and_the_fix(tmp_path):
    db = tmp_path / "devices.db"
    DeviceRegistry(db)
    db.chmod(0o444)

    with pytest.raises(RuntimeError) as excinfo:
        DeviceRegistry(db)
    message = str(excinfo.value)
    # The path is what an operator needs; blaming the permissions without
    # naming what could not be written is how the last six hours went.
    assert str(db) in message
    assert "permission" in message.lower()
