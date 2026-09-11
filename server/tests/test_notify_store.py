# server/tests/test_notify_store.py
import tempfile
from pathlib import Path
import pytest

from youn_server import notify_store as ns


@pytest.fixture()
def store():
    tmp = tempfile.mkdtemp(prefix="notify_")
    s = ns.NotifyStore(Path(tmp) / "notifications.jsonl")
    yield s


def test_enqueue_and_fifo_order(store):
    a = store.enqueue("DEV-1", "t1", "b1", 300)
    b = store.enqueue("DEV-1", "t2", "b2", 300)
    c = store.enqueue("DEV-2", "t3", "b3", 300)
    assert store.next_for("DEV-1").id == a.id  # FIFO per device
    assert store.next_for("DEV-1").id == b.id
    assert store.next_for("DEV-1") is None     # drained
    assert store.next_for("DEV-2").id == c.id


def test_next_marks_shown(store):
    n = store.enqueue("DEV-1", "t", "b", 300)
    got = store.next_for("DEV-1")
    assert got.id == n.id
    assert got.status == "shown"
    assert store.next_for("DEV-1") is None  # not redelivered


def test_expired_skipped(store):
    store.enqueue("DEV-1", "t", "b", 0)  # ttl 0 → instantly expired
    assert store.next_for("DEV-1") is None


def test_shown_still_expires(store):
    """A shown-but-never-acked notification must expire like a pending one.

    Regression: the TTL check only ran for ``pending`` items, so anything
    marked ``shown`` (BOOT-dismissed, or ack lost on the wire) stayed ``shown``
    forever — ``_items`` and notifications.jsonl grew without bound.
    """
    n = store.enqueue("DEV-1", "t", "b", 300)
    assert store.next_for("DEV-1").status == "shown"

    n.ttl_sec = 0  # simulate the TTL window elapsing
    assert store.recent(device_id="DEV-1")[0].status == "expired"


def test_ack_sets_decision(store):
    n = store.enqueue("DEV-1", "t", "b", 300)
    store.next_for("DEV-1")
    got = store.ack(n.id, "agree")
    assert got.decision == "agree"
    assert got.status == "acked"


def test_ack_idempotent(store):
    n = store.enqueue("DEV-1", "t", "b", 300)
    store.next_for("DEV-1")
    store.ack(n.id, "agree")
    again = store.ack(n.id, "reject")
    assert again.decision == "agree"  # first decision wins


def test_recent_filters_device(store):
    a = store.enqueue("DEV-1", "t", "b", 300)
    store.next_for("DEV-1")
    store.ack(a.id, "agree")
    store.enqueue("DEV-2", "t2", "b2", 300)
    hist = store.recent(device_id="DEV-1")
    assert len(hist) == 1
    assert hist[0].device_id == "DEV-1"


def test_recent_limit(store):
    for i in range(25):
        store.enqueue("DEV-1", f"t{i}", "b", 300)
    assert len(store.recent(limit=20)) == 20
