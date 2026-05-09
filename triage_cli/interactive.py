"""Interactive evidence collection for the investigate command.

Three areas of responsibility:
1. Workspace dir + download manifest helpers (this file).
2. Attachment download orchestration (download_attachments — Task 13).
3. Drop-and-ready prompt + workspace summary (Tasks 14-15).
"""
from __future__ import annotations

import json
import sys
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Literal

_MANIFEST_NAME = ".download-manifest.json"


@dataclass(frozen=True)
class Workspace:
    """Per-ticket workspace under triage-notes/<id>/."""

    root: Path
    attachments_dir: Path
    local_dir: Path


@dataclass(frozen=True)
class DownloadDecision:
    """Result of resolve_destination: skip an existing file or download to a path."""

    action: Literal["skip", "download"]
    path: Path


def ensure_workspace(notes_root: Path, *, ticket_id: int) -> Workspace:
    """Create triage-notes/<id>/{attachments,local}/ and return paths.

    Idempotent: running multiple times on the same ticket is safe.
    """
    root = notes_root / str(ticket_id)
    attachments = root / "attachments"
    local = root / "local"
    attachments.mkdir(parents=True, exist_ok=True)
    local.mkdir(parents=True, exist_ok=True)
    return Workspace(root=root, attachments_dir=attachments, local_dir=local)


def read_manifest(attachments_dir: Path) -> dict[str, dict[str, object]]:
    """Read .download-manifest.json. Missing or corrupt returns {}."""
    path = attachments_dir / _MANIFEST_NAME
    if not path.exists():
        return {}
    try:
        with path.open(encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, json.JSONDecodeError) as e:
        # Print to stderr; let the run continue with a fresh manifest.
        print(
            f"warning: manifest at {path} is unreadable ({e}); treating as empty.",
            file=sys.stderr,
        )
        return {}
    if not isinstance(data, dict):
        return {}
    return data


def write_manifest_entry(
    attachments_dir: Path,
    *,
    filename: str,
    size: int,
    sha256: str,
) -> None:
    """Update the manifest entry for filename. Creates the manifest if missing."""
    manifest = read_manifest(attachments_dir)
    manifest[filename] = {
        "size": size,
        "sha256": sha256,
        "downloaded_at": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
    }
    path = attachments_dir / _MANIFEST_NAME
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    tmp.replace(path)


def resolve_destination(
    attachments_dir: Path,
    *,
    filename: str,
    remote_size: int | None,
) -> DownloadDecision:
    """Decide whether to download a remote file and to what path.

    Logic:
    - Manifest entry with matching size → skip; reuse existing file.
    - Manifest entry with mismatched size → download to <name>.2 (or .3, ...).
    - No manifest entry but file exists on disk → skip; treat existing as authoritative.
    - No manifest, no file → download to <name>.
    """
    manifest = read_manifest(attachments_dir)
    entry = manifest.get(filename)
    target = attachments_dir / filename

    if entry and remote_size is not None and entry.get("size") == remote_size:
        return DownloadDecision(action="skip", path=target)

    if entry and remote_size is not None and entry.get("size") != remote_size:
        # Find the next available .N suffix.
        n = 2
        while (attachments_dir / f"{filename}.{n}").exists():
            n += 1
        return DownloadDecision(
            action="download", path=attachments_dir / f"{filename}.{n}",
        )

    if target.exists():
        return DownloadDecision(action="skip", path=target)

    return DownloadDecision(action="download", path=target)
