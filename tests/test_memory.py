"""Tests for the durable investigation memory layer."""
from __future__ import annotations

import pytest


@pytest.fixture()
def tmp_memory(tmp_path, monkeypatch):
    """Patch MEMORY_MD and MEMORY_DB to tmp_path for isolation."""
    import triage_cli.memory as mem
    monkeypatch.setattr(mem, "MEMORY_MD", tmp_path / "MEMORY.md")
    monkeypatch.setattr(mem, "MEMORY_DB", tmp_path / "memory.db")
    return tmp_path


def test_append_and_retrieve(tmp_memory):
    from triage_cli.memory import append_investigation, retrieve_similar

    append_investigation(
        ticket_id="ZD-100",
        customer="Acme Corp",
        subject="SBC jitter on PSAP-01",
        symptom="calls dropping after 30s, buffer overflow logs",
        assessment="CNC-7 under-provisioned",
        resolution="Increased buffer pool",
    )
    results = retrieve_similar("SBC jitter", "buffer overflow calls dropping")
    assert len(results) == 1
    assert results[0].ticket_id == "ZD-100"
    assert results[0].customer == "Acme Corp"


def test_retrieve_returns_empty_when_no_match(tmp_memory):
    from triage_cli.memory import retrieve_similar
    results = retrieve_similar("completely unrelated query", "nothing here")
    assert results == []


def test_find_duplicate_exact_match(tmp_memory):
    from triage_cli.memory import append_investigation, find_duplicate

    append_investigation(
        "ZD-200", "Corp B", "No audio", "one-way audio on outbound", "codec mismatch",
    )
    dup = find_duplicate("ZD-200")
    assert dup is not None
    assert dup.ticket_id == "ZD-200"


def test_find_duplicate_returns_none_when_absent(tmp_memory):
    from triage_cli.memory import find_duplicate
    assert find_duplicate("ZD-999") is None


def test_default_resolution_is_unknown(tmp_memory):
    from triage_cli.memory import append_investigation, find_duplicate

    append_investigation("ZD-300", "Corp C", "PSAP down", "no SIP registration", "TBD")
    entry = find_duplicate("ZD-300")
    assert entry is not None
    # "TBD" is the assessment arg (5th positional); resolution uses the default
    assert entry.resolution == "[unknown]"


def test_rebuild_from_memory_md(tmp_memory):
    from triage_cli.memory import append_investigation, rebuild_index, retrieve_similar

    append_investigation("ZD-400", "D Inc", "SBC crash", "repeated SBC restarts", "firmware bug")
    # Prune by rewriting MEMORY.md without the entry
    import triage_cli.memory as mem
    mem.MEMORY_MD.write_text("# Investigation Memory\n\n<!-- empty -->\n")

    count = rebuild_index()
    assert count == 0
    results = retrieve_similar("SBC crash", "firmware")
    assert results == []


def test_mtime_triggers_rebuild(tmp_memory, monkeypatch):
    """After MEMORY.md is modified, next retrieve_similar should auto-rebuild."""
    import time

    import triage_cli.memory as mem
    from triage_cli.memory import append_investigation, retrieve_similar

    append_investigation("ZD-500", "E Corp", "call drops", "drops at 30s", "routing fix")
    results1 = retrieve_similar("call drops", "drops")
    assert len(results1) == 1

    # Simulate human pruning: rewrite MEMORY.md empty, touch mtime
    time.sleep(0.01)
    mem.MEMORY_MD.write_text("# Investigation Memory\n\n<!-- empty -->\n")

    results2 = retrieve_similar("call drops", "drops")
    assert results2 == []


def test_memory_md_created_if_absent(tmp_memory):
    import triage_cli.memory as mem
    from triage_cli.memory import append_investigation
    assert not mem.MEMORY_MD.exists()
    append_investigation("ZD-600", "F Corp", "subject", "symptom", "assessment")
    assert mem.MEMORY_MD.exists()
    content = mem.MEMORY_MD.read_text()
    assert "ZD-600" in content


def test_retrieve_limit_respected(tmp_memory):
    from triage_cli.memory import append_investigation, retrieve_similar

    for i in range(5):
        append_investigation(f"ZD-{700+i}", "G Corp", "SBC issue", "SBC error logs", f"fix {i}")
    results = retrieve_similar("SBC issue", "SBC error", limit=2)
    assert len(results) <= 2
