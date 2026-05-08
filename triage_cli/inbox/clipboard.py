"""Best-effort clipboard copy via wl-copy / xclip / pbcopy."""
from __future__ import annotations

import subprocess

# Order matters: prefer Wayland-native, then X11, then macOS.
_TOOLS = (
    ("wl-copy",),
    ("xclip", "-selection", "clipboard"),
    ("pbcopy",),
)


def copy_to_clipboard(text: str) -> bool:
    """Copy ``text`` to the system clipboard. Return True on success.

    Tries wl-copy, xclip, then pbcopy. Returns False if none are
    available or all fail.
    """
    for cmd in _TOOLS:
        try:
            result = subprocess.run(
                list(cmd),
                input=text.encode("utf-8"),
                check=True,
                timeout=2,
            )
        except FileNotFoundError:
            continue
        except (subprocess.SubprocessError, OSError):
            continue

        if result.returncode == 0:
            return True

    return False
