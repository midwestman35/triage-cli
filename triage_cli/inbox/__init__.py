"""Inbox TUI for triage-cli."""
from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from triage_cli.inbox.app import InboxApp

__all__ = ["InboxApp"]


def __getattr__(name: str) -> object:
    if name == "InboxApp":
        from triage_cli.inbox.app import InboxApp

        return InboxApp
    raise AttributeError(name)
