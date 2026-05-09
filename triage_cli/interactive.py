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

import typer

from triage_cli.investigation import _detect_file_type, _read_text_if_supported
from triage_cli.models import AttachmentEvidence, LocalFileEvidence, Ticket

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


def confirm_download(ticket: Ticket) -> bool:
    """Prompt the user once for all-or-nothing attachment download.

    Side-effects: prints attachment list to stderr; reads y/n from stdin.
    Returns True on yes (default Y), False on no.
    """
    attachments = _flatten_attachments(ticket)
    if not attachments:
        return False

    print(
        f"Found {len(attachments)} attachment(s) on ticket #{ticket.id}:",
        file=sys.stderr,
    )
    for a in attachments:
        size = f"{a.size_bytes} bytes" if a.size_bytes is not None else "unknown size"
        ctype = a.content_type or "unknown"
        print(f"  - {a.filename} ({ctype}, {size})", file=sys.stderr)
    return typer.confirm("Download all to workspace?", default=True)


def download_attachments(
    ticket: Ticket,
    zd_client: object,  # protocol: download_attachment(url, dest, *, max_bytes)
    workspace: Workspace,
    *,
    max_bytes: int = 150 * 1024 * 1024,
) -> list[AttachmentEvidence]:
    """Orchestrate the all-or-nothing attachment download.

    Returns a list of AttachmentEvidence — one per attachment on the ticket.
    Items have local_path set if downloaded (or skipped because already present);
    None otherwise.
    """
    attachments = _flatten_attachments(ticket)
    if not attachments:
        return []

    if not confirm_download(ticket):
        # Return the metadata as-is (local_path stays None).
        return list(attachments)

    out: list[AttachmentEvidence] = []
    for a in attachments:
        if not a.content_url:
            print(
                f"warning: no download URL for {a.filename}; skipping.",
                file=sys.stderr,
            )
            out.append(a)
            continue

        decision = resolve_destination(
            workspace.attachments_dir,
            filename=a.filename,
            remote_size=a.size_bytes,
        )
        if decision.action == "skip":
            print(f"  reused {decision.path.name} (manifest match)", file=sys.stderr)
            out.append(a.model_copy(update={"local_path": decision.path}))
            continue

        try:
            print(f"  downloading {decision.path.name}...", file=sys.stderr, end=" ")
            bytes_written, sha = zd_client.download_attachment(
                a.content_url, decision.path, max_bytes=max_bytes,
            )
            print(f"done ({bytes_written} bytes)", file=sys.stderr)
            write_manifest_entry(
                workspace.attachments_dir,
                filename=decision.path.name,
                size=bytes_written,
                sha256=sha,
            )
            out.append(a.model_copy(update={"local_path": decision.path}))
        except RuntimeError as e:
            print(f"failed: {e}", file=sys.stderr)
            out.append(a)
        except Exception as e:  # AttachmentTooLargeError and friends
            print(f"skipped: {e}", file=sys.stderr)
            out.append(a)

    return out


def _flatten_attachments(ticket: Ticket) -> list[AttachmentEvidence]:
    """Return all attachments across the ticket's comments, in comment order."""
    out: list[AttachmentEvidence] = []
    for c in ticket.comments:
        out.extend(c.attachments)
    return out


_SKIP_TOKENS = {"skip", "quit", "q", "abort"}


def prompt_drop_and_wait(workspace: Workspace) -> list[LocalFileEvidence]:
    """Block until user types 'ready' (or empty enter); then scan local/.

    Returns LocalFileEvidence for every file in local/, classified by type.
    Text/log/json files have extracted_text populated; binaries do not.
    Empty input or 'ready' → ingest. 'skip'/'quit'/'q'/'abort' → return [].
    """
    print(
        "\nDrop supplemental logs in:\n"
        f"  {workspace.local_dir}\n"
        "Suggested types: zipped Apex station logs, Homer/Twilio SIP extracts,\n"
        "Datadog CSV. Press <enter> when ready (or 'skip' to continue with\n"
        "no local evidence).",
        file=sys.stderr,
    )

    while True:
        try:
            response = input("ready> ").strip().lower()
        except EOFError:
            response = ""
        if response in _SKIP_TOKENS:
            return []
        if response in ("", "ready"):
            return _ingest_local(workspace.local_dir)
        # Unknown token: re-prompt.
        print("(type 'ready' or press <enter> to ingest; 'skip' to skip)", file=sys.stderr)


def _ingest_local(local_dir: Path) -> list[LocalFileEvidence]:
    """Scan local_dir, classify each file, build LocalFileEvidence list."""
    if not local_dir.exists():
        return []
    out: list[LocalFileEvidence] = []
    for path in sorted(local_dir.iterdir()):
        if not path.is_file():
            continue
        try:
            stat = path.stat()
        except OSError:
            continue
        detected = _detect_file_type(path)
        text = _read_text_if_supported(path, detected)
        out.append(
            LocalFileEvidence(
                path=path,
                size_bytes=stat.st_size,
                detected_type=detected,
                extracted_text=text,
            ),
        )
    return out
