# Go Spike Notes — `triage-cli`

This branch begins a from-scratch Go rewrite of `triage-cli`. The Python repo
(at `../`) is treated as a *behavioral reference*, not as architecture to port.

## North star

A guided Zendesk ticket investigation assistant that helps a NOC/support
engineer collect evidence, correlate logs/comments/attachments, produce an
assessment, and generate a clean internal note or handoff.

Two product pillars:

1. `triage-cli investigate <ticket>` — guided investigation flow.
2. `triage-cli watch --view <id>` — automated watcher for Zendesk views.

Datadog and CNC/site mapping are optional evidence enrichment, not the spine.

## What we are preserving from Python

Behavioral concepts, not implementation:

- Ticket ID and ticket-URL parsing (raw integer or `/tickets/<id>` URL).
- Zendesk ticket fetch (subject, description, requester org, comments).
- Internal-vs-public comment awareness.
- Markdown + JSON paired artifact output, saved per ticket per timestamp.
- Watcher state model: `{version, triaged: {ticket_id: updated_at}}`, atomic
  writes, prune to N entries, first-run silent backfill.
- "Stdout is for the rendered report; everything else goes to stderr" rule
  so output stays pipe-friendly.
- `--mock` / fixture-driven local development without hitting live APIs.
- Optional CNC/site map with priority resolution (flag → org → subject
  bracket → substring).

## What we are intentionally redesigning

- **Datadog is demoted.** It was the spine of the Python flow; here it is one
  optional `EvidenceSource` among many (local files, paste, attachments,
  Datadog, future MCP).
- **Investigation is the central abstraction**, not a one-shot pipeline.
  The core type is `InvestigationSession` with explicit phases (load,
  review, evidence, parse, timeline, correlate, assess, export). The
  guided flow walks these phases; `triage` runs them non-interactively.
- **Evidence is polymorphic**, not log-shaped. Comments, attachments,
  pasted text, local files, and log lines all normalize into
  `TimelineEvent`s that the assessment consumes.
- **Assessment is honest about confidence.** When evidence is thin, the
  report says "unknown" rather than fabricating a root cause. The Python
  prompt allowed this; the Go report enforces it as a struct field.
- **Single binary, no Python/Claude-CLI dependency at the edges.** Mock
  mode and deterministic stub assessment let `investigate --mock` run
  with zero external dependencies. LLM integration plugs in behind an
  interface later.
- **Cobra-first command surface** with a `doctor` command for env checks
  and a `version` command — patterns borrowed from polished operator
  CLIs (Printing Press Library mindset).

## What is deferred

Explicitly out of scope for this spike:

- Live Zendesk HTTP client (mock-only for now; real client behind an
  interface, with TODO).
- Live Datadog client.
- Live LLM call (deterministic stub assessment for now).
- Bubble Tea three-pane TUI. The spike uses a clean linear terminal flow.
- SQLite history.
- Slack notifications.
- Posting back to Zendesk.
- `build-map` command (CNC inventory parser).
- Attachment download / extraction (we surface metadata only).

## Proposed Go package structure

```
triage-cli-go/
  cmd/triage-cli/main.go            # entry point
  internal/
    cli/                            # cobra commands
      root.go
      investigate.go
      triage.go
      watch.go
      doctor.go
      version.go
    config/                         # env + flags
      config.go
    model/                          # core domain types
      ticket.go
      evidence.go
      timeline.go
      assessment.go
      report.go
      session.go
    zendesk/                        # ticket fetch (mock + interface)
      client.go
      mock.go
      parse.go
    investigation/                  # guided flow orchestration
      session.go
      flow.go
    evidence/                       # ingestion + normalization
      local_file.go
      paste.go
      attachment.go
      timeline.go
    assessment/                     # stub assessor + interface
      stub.go
    render/                         # markdown + json output
      markdown.go
      json.go
      stderr.go
    store/                          # artifact paths + writes
      artifacts.go
    watcher/                        # poll loop + state (skeleton)
      state.go
      watcher.go
    integrations/
      datadog/                      # placeholder
        client.go
      sitemap/                      # placeholder
        map.go
  testdata/tickets/                 # JSON fixtures
  docs/
    go-spike-notes.md
  skills/
    triage-investigate/SKILL.md
    triage-watch/SKILL.md
  go.mod
  README.md
```

## Overnight implementation plan

Priority order (each item should be runnable + tested before moving on):

1. **Module skeleton** — `go.mod`, dir tree, blank `main.go` that compiles.
2. **Cobra root + version + doctor** — `triage-cli version` works.
3. **Domain models** — `Ticket`, `Comment`, `Evidence*`, `TimelineEvent`,
   `Assessment`, `TriageReport`, `InvestigationSession`, with JSON tags
   and table-driven tests.
4. **Ticket ID/URL parser** — port `parse_ticket_id` semantics (raw int
   or `/tickets/<id>` URL), with table tests including edge cases.
5. **Mock Zendesk client** — `Fetcher` interface, `MockFetcher`
   reading `testdata/tickets/<id>.json`, with built-in fallback fixture
   so `investigate 12345 --mock` works without a fixture file present.
6. **Evidence + timeline** — comment-to-timeline conversion; `Local
   File` and `Paste` evidence types with normalization stubs.
7. **Stub assessment** — deterministic `Assessor` impl that builds a
   plausible `Assessment` from the timeline (no LLM); marks confidence
   honestly based on evidence count.
8. **Render** — Markdown and JSON renderers matching the spec sections;
   Markdown to stdout, JSON to a paired artifact.
9. **Store** — write paired `.md` and `.json` to `triage-notes/<id>-<ts>`.
10. **Investigate command** — wires it all together; `--mock`, `--json`,
    `--output-dir`, `--quiet` flags.
11. **Triage command** — non-interactive variant of investigate (same
    pipeline, no prompts, no guided phase headers).
12. **Watch command** — skeleton with state file shape only; logs
    "would triage" per tick and persists state.
13. **Doctor command** — checks `ZENDESK_*` env presence, output dir
    writable, watcher state dir writable, optional Datadog config.
14. **Tests** — `go test ./...` passes; smoke test on the success target.

Success target:

```bash
go run ./cmd/triage-cli investigate 12345 --mock
```

prints a guided Markdown triage flow to stdout and saves
`./triage-notes/12345-<ts>.md` and `.json`.

## How to run

```bash
cd triage-cli-go
go build ./...
go test ./...
go run ./cmd/triage-cli version
go run ./cmd/triage-cli investigate 12345 --mock
go run ./cmd/triage-cli triage 12345 --mock --json
go run ./cmd/triage-cli doctor
```

## What the next agent should pick up

1. Real Zendesk HTTP client behind the `Fetcher` interface.
2. Real LLM-backed `Assessor` (Anthropic or local) behind the
   `Assessor` interface — keep the stub as a `--no-llm` option.
3. Attachment download + text extraction.
4. Watcher: actual polling loop against Zendesk views.
5. Bubble Tea three-pane TUI for `investigate`, kept as a flag
   (`--tui`); the linear flow remains the default for piping.
6. Optional Datadog evidence source.
7. `build-map` command parity (parse `apex-cnc-inventory.md`).
