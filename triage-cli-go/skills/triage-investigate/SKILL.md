# triage-investigate

Guided investigation of a single Zendesk ticket. Walks the linear
pipeline (load → review → ingest → timeline → assess → render) and
saves paired `.md` and `.json` artifacts.

## When to use

- A ticket landed and you want a structured triage note before
  responding internally.
- You need a quick correlated timeline of comments + local evidence.
- You want a starting draft of an internal note that does NOT
  fabricate a root cause when evidence is thin.

## Basic usage

```bash
# Mock mode — works without any environment variables.
triage-cli investigate 12345 --mock
```

Output:
- Markdown report on stdout (pipe-friendly).
- Paired files in `./triage-notes/12345-<UTC timestamp>.{md,json}`.
- Phase headers (`→ [1/6] Loading ticket...`) on stderr.

## Common flags

| Flag | Purpose |
| --- | --- |
| `--mock` | Use the mock Zendesk fetcher (required while the live client is unimplemented). |
| `--json` | Emit JSON to stdout instead of Markdown. |
| `--evidence <path>` | Ingest a local evidence file. Repeatable. |
| `--output-dir <dir>` | Where to save paired artifacts (default `./triage-notes`). |
| `--quiet` | Suppress stderr phase headers and `saved` notices. |

## Example: ingest a local log

```bash
triage-cli investigate 12345 --mock \
  --evidence ~/Downloads/ws3-2026-05-08.log \
  --evidence ~/Downloads/sbc-jitter.csv
```

Each `--evidence` file becomes a `local_file` evidence entry,
contributes a `## Timeline` row, and is summarized in the
`## Correlation` and `## Suggested Internal Note` sections.

## Reading the report

- **Confidence: unknown** means evidence was thin. The assessment
  refuses to claim a root cause; treat the report as a
  "request more evidence" template.
- **Confidence: possible** means correlation surfaced patterns
  worth corroborating. The stub never claims `likely` or
  `confirmed` — that is reserved for future LLM-backed assessors.

## What this is not (yet)

- No live Zendesk fetch. Use `--mock`.
- No LLM call. Assessment is deterministic.
- No Datadog evidence source. Hand it logs via `--evidence`.
