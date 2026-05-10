"""Render TriageReports to stdout and persist paired markdown/JSON files."""

from __future__ import annotations

import os
import sys
from datetime import UTC, datetime
from pathlib import Path

from rich.console import Console, ConsoleRenderable, Group
from rich.panel import Panel
from rich.text import Text

from triage_cli.models import TriageReport

DEFAULT_OUTPUT_DIR = Path("./triage-notes")
Paths = tuple[Path, Path]


def _utc(dt: datetime) -> datetime:
    if dt.tzinfo is None:
        return dt.replace(tzinfo=UTC)
    return dt.astimezone(UTC)


def _time(dt: datetime | None) -> str:
    return _utc(dt).strftime("%H:%M:%S") if dt is not None else "—"


def _sources(report: TriageReport) -> str:
    sources = ", ".join(report.sources)
    if "datadog" in report.sources:
        sources += f" ({report.log_event_count} events)"
    return sources


def _panel(body: str, title: str, border_style: str | None = None) -> Panel:
    kwargs = {"border_style": border_style} if border_style else {}
    return Panel(body, title=title, title_align="left", **kwargs)


def to_markdown(report: TriageReport) -> str:
    window_start = _utc(report.window.start)
    window_end = _utc(report.window.end)
    window = f"{window_start:%Y-%m-%d %H:%M}–{window_end:%H:%M} UTC"
    meta = (
        f"**Confidence:** {report.confidence} · **Sources:** {_sources(report)} · "
        f"**Window:** {window} · **Site:** {report.site_name}"
    )
    lines = [
        f"# Triage Report — ZD-{report.ticket_id}",
        "",
        meta,
        "",
        "## Finding",
        report.finding,
        "",
        "## Evidence",
    ]

    if report.evidence:
        for evidence in report.evidence:
            service = evidence.service or "—"
            lines.append(f"- {_time(evidence.timestamp)} · {service} · {evidence.message}")
    else:
        lines.append("_No evidence collected._")
    lines.append("")

    if report.next_checks:
        lines.append("## Next Checks")
        lines.extend(f"- {check}" for check in report.next_checks)
        lines.append("")

    if report.unknowns:
        lines.append("## Unknowns")
        lines.extend(f"- {unknown}" for unknown in report.unknowns)
        lines.append("")

    lines.extend(["## Suggested Internal Note", report.suggested_note])

    if (
        report.context_summary is not None
        and report.context_summary.kept < report.context_summary.candidates
    ):
        elided = report.context_summary.candidates - report.context_summary.kept
        total = report.context_summary.candidates
        lines.append(
            f"\n> Note: {elided} of {total} log lines elided by relevance scoring "
            "(severity, subject match, anchor proximity).",
        )

    return "\n".join(lines)


def rich_layout(report: TriageReport) -> ConsoleRenderable:
    styles = {"low": "yellow", "medium": "cyan", "high": "green"}
    header = Text.assemble(
        ("ZD-", "dim"),
        (str(report.ticket_id), "bold"),
        ("  ·  ", "dim"),
        (f"confidence: {report.confidence}", styles.get(report.confidence, "white")),
        ("  ·  ", "dim"),
        (f"sources: {_sources(report)}", "dim"),
        ("  ·  ", "dim"),
        (f"site: {report.site_name}", "dim"),
    )

    if report.evidence:
        evidence_text = "\n".join(
            f"[dim]{_time(e.timestamp)}[/]  [cyan]{(e.service or '—'):<15}[/]  {e.message}"
            for e in report.evidence
        )
    else:
        evidence_text = "[dim]No evidence collected.[/]"

    panels: list[ConsoleRenderable] = [
        header,
        _panel(report.finding, "Finding"),
        _panel(evidence_text, "Evidence"),
    ]
    if report.next_checks:
        panels.append(_panel("\n".join(f"• {c}" for c in report.next_checks), "Next Checks"))
    if report.unknowns:
        panels.append(
            _panel("\n".join(f"• {unknown}" for unknown in report.unknowns), "Unknowns", "yellow"),
        )

    panels.append(_panel(report.suggested_note, "Suggested Internal Note", "green"))

    if (
        report.context_summary is not None
        and report.context_summary.kept < report.context_summary.candidates
    ):
        elided = report.context_summary.candidates - report.context_summary.kept
        total = report.context_summary.candidates
        panels.append(
            Text(
                f"Note: {elided} of {total} log lines elided by relevance scoring "
                "(severity, subject match, anchor proximity).",
                style="dim",
            )
        )

    return Group(*panels)


def print_note(report: TriageReport, *, console: Console | None = None) -> None:
    if console is not None:
        if console.is_terminal:
            console.print(rich_layout(report))
        else:
            console.print(to_markdown(report), markup=False, highlight=False)
        return

    if sys.stdout.isatty() and not os.getenv("NO_COLOR"):
        Console().print(rich_layout(report))
    else:
        sys.stdout.write(to_markdown(report))
        sys.stdout.write("\n")


def save_note(report: TriageReport, ticket_id: int, output_dir: Path | None = None) -> Paths:
    """Save markdown + JSON. Strips content_url from any nested AttachmentEvidence."""
    import json as _json

    target_dir = output_dir if output_dir is not None else DEFAULT_OUTPUT_DIR
    target_dir.mkdir(parents=True, exist_ok=True)

    timestamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    base_stem = f"{ticket_id}-{timestamp}"
    stem = base_stem
    counter = 1
    while True:
        md_path = target_dir / f"{stem}.md"
        json_path = target_dir / f"{stem}.json"
        if not md_path.exists() and not json_path.exists():
            break
        counter += 1
        stem = f"{base_stem}-{counter}"

    md_text = to_markdown(report)
    md_path.write_text(md_text if md_text.endswith("\n") else f"{md_text}\n", encoding="utf-8")
    # exclude defensively: even if a future TriageReport gains attachments, no URL leaks.
    payload = report.model_dump(mode="json", exclude={"content_url"})
    json_path.write_text(
        _json.dumps(_strip_nested_key(payload, "content_url"), indent=2) + "\n",
        encoding="utf-8",
    )
    return md_path, json_path


def _strip_nested_key(obj: object, key: str) -> object:
    """Recursively remove a key from nested dicts/lists. Defensive."""
    if isinstance(obj, dict):
        return {k: _strip_nested_key(v, key) for k, v in obj.items() if k != key}
    if isinstance(obj, list):
        return [_strip_nested_key(v, key) for v in obj]
    return obj
