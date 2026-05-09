"""Tests for the per-ticket workspace + download manifest."""
from __future__ import annotations

from pathlib import Path

import pytest


def test_workspace_paths_creates_subdirs(tmp_path: Path) -> None:
    """ensure_workspace creates {attachments,local} under triage-notes/<id>/."""
    from triage_cli.interactive import ensure_workspace

    ws = ensure_workspace(tmp_path, ticket_id=44496)
    assert ws.root == tmp_path / "44496"
    assert ws.attachments_dir == tmp_path / "44496" / "attachments"
    assert ws.local_dir == tmp_path / "44496" / "local"
    assert ws.attachments_dir.is_dir()
    assert ws.local_dir.is_dir()


def test_workspace_idempotent_on_existing_dirs(tmp_path: Path) -> None:
    """Running ensure_workspace twice succeeds; no error on existing dirs."""
    from triage_cli.interactive import ensure_workspace

    ensure_workspace(tmp_path, ticket_id=44496)
    ws = ensure_workspace(tmp_path, ticket_id=44496)  # second call
    assert ws.root.is_dir()


def test_manifest_read_missing_returns_empty(tmp_path: Path) -> None:
    """No manifest yet → empty dict, no error."""
    from triage_cli.interactive import read_manifest

    ws = tmp_path / "44496" / "attachments"
    ws.mkdir(parents=True)
    assert read_manifest(ws) == {}


def test_manifest_read_corrupt_returns_empty_with_warning(
    tmp_path: Path, capsys: pytest.CaptureFixture[str],
) -> None:
    """Corrupt manifest is treated as missing; a warning is printed to stderr."""
    from triage_cli.interactive import read_manifest

    ws = tmp_path / "44496" / "attachments"
    ws.mkdir(parents=True)
    (ws / ".download-manifest.json").write_text("not json {", encoding="utf-8")

    result = read_manifest(ws)
    assert result == {}
    captured = capsys.readouterr()
    assert "manifest" in captured.err.lower()


def test_manifest_write_roundtrip(tmp_path: Path) -> None:
    from triage_cli.interactive import read_manifest, write_manifest_entry

    ws = tmp_path / "44496" / "attachments"
    ws.mkdir(parents=True)

    write_manifest_entry(
        ws, filename="log.txt", size=1024, sha256="abc123",
    )
    manifest = read_manifest(ws)
    assert "log.txt" in manifest
    assert manifest["log.txt"]["size"] == 1024
    assert manifest["log.txt"]["sha256"] == "abc123"
    assert "downloaded_at" in manifest["log.txt"]


def test_resolve_destination_skip_when_size_matches(tmp_path: Path) -> None:
    """If manifest already has matching size, returns SKIP."""
    from triage_cli.interactive import resolve_destination, write_manifest_entry

    ws = tmp_path / "attachments"
    ws.mkdir(parents=True)
    (ws / "log.txt").write_text("existing", encoding="utf-8")
    write_manifest_entry(ws, filename="log.txt", size=1024, sha256="abc")

    decision = resolve_destination(ws, filename="log.txt", remote_size=1024)
    assert decision.action == "skip"
    assert decision.path == ws / "log.txt"


def test_resolve_destination_collision_creates_suffix(tmp_path: Path) -> None:
    """If size mismatches, return a .2 suffix path."""
    from triage_cli.interactive import resolve_destination, write_manifest_entry

    ws = tmp_path / "attachments"
    ws.mkdir(parents=True)
    (ws / "log.txt").write_text("existing", encoding="utf-8")
    write_manifest_entry(ws, filename="log.txt", size=1024, sha256="abc")

    decision = resolve_destination(ws, filename="log.txt", remote_size=2048)
    assert decision.action == "download"
    assert decision.path == ws / "log.txt.2"
