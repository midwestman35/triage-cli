"""Build data/cnc-map.json and data/cnc-map-gaps.md from apex-cnc-inventory.md."""

from __future__ import annotations

from pathlib import Path
from datetime import datetime, timezone
import json
import re

REPO_ROOT = Path(__file__).resolve().parent.parent
INVENTORY = REPO_ROOT / "apex-cnc-inventory.md"
MAP_OUT = REPO_ROOT / "data" / "cnc-map.json"
GAPS_OUT = REPO_ROOT / "data" / "cnc-map-gaps.md"

PER_SITE_HEADING = "Confirmed via per-site Overview pages"
MASTER_HEADING = 'From master "APEX Sites Description" page (display label only)'

UUID_RE = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
SITE_NAME_RE = re.compile(r"^[a-z]{2}-[a-z0-9-]+$")


def _split_row(line: str) -> list[str]:
    """Split a markdown table row into trimmed cells."""
    return [c.strip() for c in line.strip().strip("|").split("|")]


def _is_separator(line: str) -> bool:
    """True if the row is a `|---|---|` style separator under the header."""
    inner = line.strip().strip("|")
    return bool(inner) and all(set(c.strip()) <= set("-:") for c in inner.split("|"))


def parse_table(text: str, heading: str) -> list[list[str]]:
    """Return data rows (cells) under the H2 whose title matches `heading`."""
    rows: list[list[str]] = []
    in_section = False
    seen_separator = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("## "):
            if in_section:
                break
            in_section = stripped[3:].strip() == heading
            continue
        if not in_section or not stripped.startswith("|"):
            continue
        if _is_separator(stripped):
            seen_separator = True
            continue
        if seen_separator:
            rows.append(_split_row(stripped))
    return rows


def _clean_friendly(name: str) -> str:
    """Strip a trailing parenthetical (Fairfax Pine Ridge correction)."""
    return re.sub(r"\s*\([^)]*\)\s*$", "", name).strip()


def _is_uuid(value: str) -> bool:
    return bool(UUID_RE.match(value.strip().lower()))


def _normalize_label_to_site_name(label: str) -> str | None:
    """Return a site_name if `label` already looks like one, else None.

    Conservative: reject labels containing spaces. Fabricating a site_name
    by squashing spaces (`MX-Sales CCS` -> `mx-salesccs`) would produce a
    Datadog query key that does not actually exist. Only accept labels
    that, after lowercasing, match `^[a-z]{2}-[a-z0-9-]+$` directly
    (e.g. `US-LA-Orleans-Apex`, `CO-UNP`).
    """
    if " " in label.strip():
        return None
    candidate = label.strip().lower()
    return candidate if SITE_NAME_RE.match(candidate) else None


def build_entries(
    per_site_rows: list[list[str]], master_rows: list[list[str]]
) -> tuple[list[dict], dict[str, list[dict]]]:
    """Return (entries sorted by site_name, gaps_by_reason)."""
    entries: dict[str, dict] = {}  # cnc -> entry
    gaps: dict[str, list[dict]] = {"blank": [], "unparseable_label": []}

    for row in per_site_rows:
        if len(row) < 3:
            continue
        site_name, friendly, cnc = row[0], row[1], row[2]
        if not _is_uuid(cnc):
            gaps["blank"].append(
                {"site_name": site_name, "friendly_name": friendly, "notes": cnc}
            )
            continue
        entries[cnc.lower()] = {
            "friendly_name": _clean_friendly(friendly),
            "site_name": site_name,
            "cnc": cnc.lower(),
        }

    for row in master_rows:
        if len(row) < 3:
            continue
        label, cnc, region = row[0], row[1], row[2]
        if not _is_uuid(cnc):
            gaps["blank"].append(
                {"site_name": "", "friendly_name": label, "notes": cnc}
            )
            continue
        cnc_key = cnc.lower()
        if cnc_key in entries:
            continue  # per-site wins; silent dedupe
        site_name = _normalize_label_to_site_name(label)
        if site_name is None:
            gaps["unparseable_label"].append(
                {"display_label": label, "cnc": cnc_key, "region": region}
            )
            continue
        entries[cnc_key] = {
            "friendly_name": label,
            "site_name": site_name,
            "cnc": cnc_key,
        }

    return sorted(entries.values(), key=lambda e: e["site_name"]), gaps


def _render_section(heading: str, header: str, rows: list[str]) -> list[str]:
    out = [f"## {heading}", ""]
    if not rows:
        out.append("_None._")
    else:
        out.append(header)
        out.append("|" + "---|" * (header.count("|") - 1))
        out.extend(rows)
    out.append("")
    return out


def render_gaps_markdown(gaps: dict[str, list[dict]]) -> str:
    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    lines = [
        "# CNC map gaps",
        "",
        "Rows from `apex-cnc-inventory.md` that were skipped during conversion. Fill in",
        "the missing data and re-run `triage-cli build-map` (or",
        "`python scripts/build_cnc_map.py`) to regenerate `data/cnc-map.json`.",
        "",
        f"Generated: {now}",
        "Source: `apex-cnc-inventory.md`",
        "",
    ]
    blank_rows = [
        f"| {r.get('site_name', '')} | {r.get('friendly_name', '')} | {r.get('notes', '')} |"
        for r in gaps.get("blank", [])
    ]
    lines += _render_section(
        "Missing or unparseable CNC UUID in source",
        "| Site Name | Friendly Name | Notes |",
        blank_rows,
    )
    label_rows = [
        f"| {r['display_label']} | {r['cnc']} | {r['region']} |"
        for r in gaps.get("unparseable_label", [])
    ]
    lines += _render_section(
        "Master-table label is not a parseable site_name",
        "| Display Label | CNC UUID | Region |",
        label_rows,
    )
    return "\n".join(lines)


def main() -> None:
    inventory_text = INVENTORY.read_text(encoding="utf-8-sig")
    per_site_rows = parse_table(inventory_text, PER_SITE_HEADING)
    master_rows = parse_table(inventory_text, MASTER_HEADING)
    if not per_site_rows and not master_rows:
        raise SystemExit(
            f"No table rows found under either '{PER_SITE_HEADING}' or "
            f"'{MASTER_HEADING}' in {INVENTORY}. Headings may have changed; "
            f"refusing to overwrite {MAP_OUT.relative_to(REPO_ROOT)}."
        )
    entries, gaps = build_entries(per_site_rows, master_rows)
    MAP_OUT.parent.mkdir(parents=True, exist_ok=True)
    MAP_OUT.write_text(json.dumps(entries, indent=2) + "\n", encoding="utf-8")
    GAPS_OUT.write_text(render_gaps_markdown(gaps), encoding="utf-8")
    print(f"Wrote {len(entries)} entries to {MAP_OUT.relative_to(REPO_ROOT)}")
    print(f"Logged {sum(len(v) for v in gaps.values())} gaps to {GAPS_OUT.relative_to(REPO_ROOT)}")


if __name__ == "__main__":
    main()
