# triage-cli (Go spike)

A guided Zendesk ticket investigation assistant. Loads a ticket, reviews
comments and attachments, ingests local evidence files, builds a timeline,
and produces a structured triage report — paired Markdown and JSON
artifacts, with a deterministic stub assessment.

> **Status: spike.** Live Zendesk, Datadog, and LLM integrations are
> deferred behind interfaces. `--mock` mode runs end-to-end with zero
> external dependencies. See `docs/go-spike-notes.md` for the
> architectural narrative and what the next agent should pick up.

## Quickstart

```bash
go build ./...
go test ./...

# Walks the guided pipeline, prints Markdown to stdout, saves paired
# .md and .json under ./triage-notes/.
go run ./cmd/triage-cli investigate 12345 --mock

# Same pipeline, JSON to stdout instead of Markdown.
go run ./cmd/triage-cli investigate 12345 --mock --json

# Non-interactive variant (no phase headers, no timeline section).
go run ./cmd/triage-cli triage 12345 --mock

# Environment readiness check.
go run ./cmd/triage-cli doctor

# One-shot watcher tick (skeleton — does not poll Zendesk yet).
go run ./cmd/triage-cli watch --view 12345
```

Stdout is reserved for the rendered report so output is pipe-friendly.
Status, warnings, and progress headers go to stderr.

## Layout

- `cmd/triage-cli/` — CLI entry point.
- `internal/cli/` — cobra command tree.
- `internal/model/` — domain types (Ticket, Evidence, Assessment, Report).
- `internal/zendesk/` — ticket ID parsing + mock fetcher (live HTTP TODO).
- `internal/evidence/` — comment / attachment / file / paste ingestion + timeline.
- `internal/assessment/` — Assessor interface + deterministic stub.
- `internal/render/` — Markdown / JSON renderers + stderr helper.
- `internal/store/` — paired artifact writer.
- `internal/investigation/` — pipeline orchestration shared by both commands.
- `internal/watcher/` — state file + skeleton tick.

## Further reading

- `docs/go-spike-notes.md` — architectural notes and deferred work.
- `skills/triage-investigate/SKILL.md` — operator-level guide.
- `skills/triage-watch/SKILL.md` — watcher operator guide.
