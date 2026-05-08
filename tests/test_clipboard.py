"""Tests for inbox.clipboard.copy_to_clipboard fallback chain."""
from __future__ import annotations

import subprocess
from unittest.mock import MagicMock, patch

from triage_cli.inbox import clipboard


def test_copy_uses_first_available_tool():
    """If wl-copy works, xclip and pbcopy are not tried."""
    with patch("triage_cli.inbox.clipboard.subprocess.run") as mock_run:
        mock_run.return_value = MagicMock(returncode=0)
        ok = clipboard.copy_to_clipboard("hello")

    assert ok is True
    args = mock_run.call_args_list[0][0][0]
    assert args[0] == "wl-copy"
    assert mock_run.call_count == 1


def test_copy_falls_back_when_first_tool_missing():
    """FileNotFoundError on wl-copy tries xclip."""

    def side_effect(cmd, **_):
        if cmd[0] == "wl-copy":
            raise FileNotFoundError
        return MagicMock(returncode=0)

    with patch(
        "triage_cli.inbox.clipboard.subprocess.run",
        side_effect=side_effect,
    ) as mock_run:
        ok = clipboard.copy_to_clipboard("hello")

    assert ok is True
    cmds_tried = [c[0][0][0] for c in mock_run.call_args_list]
    assert cmds_tried[:2] == ["wl-copy", "xclip"]


def test_copy_falls_back_to_pbcopy_when_linux_tools_fail():
    """wl-copy and xclip failures fall through to pbcopy."""

    def side_effect(cmd, **_):
        if cmd[0] in {"wl-copy", "xclip"}:
            raise subprocess.CalledProcessError(1, cmd)
        return MagicMock(returncode=0)

    with patch(
        "triage_cli.inbox.clipboard.subprocess.run",
        side_effect=side_effect,
    ) as mock_run:
        ok = clipboard.copy_to_clipboard("hello")

    assert ok is True
    cmds_tried = [c[0][0][0] for c in mock_run.call_args_list]
    assert cmds_tried == ["wl-copy", "xclip", "pbcopy"]


def test_copy_returns_false_when_no_tool_available():
    """All three FileNotFoundError returns False."""
    with patch(
        "triage_cli.inbox.clipboard.subprocess.run",
        side_effect=FileNotFoundError,
    ) as mock_run:
        ok = clipboard.copy_to_clipboard("hello")

    assert ok is False
    assert mock_run.call_count == 3


def test_copy_returns_false_on_subprocess_error():
    """A tool exists but errors out; keep trying, then return False."""
    with patch(
        "triage_cli.inbox.clipboard.subprocess.run",
        side_effect=subprocess.CalledProcessError(1, "x"),
    ) as mock_run:
        ok = clipboard.copy_to_clipboard("hello")

    assert ok is False
    assert mock_run.call_count == 3
