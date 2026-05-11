"""Bootstrap wrapper for triage-cli first-time setup."""

from __future__ import annotations

import sys
from pathlib import Path


def _main() -> int:
    root = Path(__file__).resolve().parents[1]
    sys.path.insert(0, str(root))

    from triage_cli.setup import main

    return main()


if __name__ == "__main__":
    raise SystemExit(_main())
