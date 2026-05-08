# HANDOFF.md — Go Spike Branch for `triage-cli`


## Printing Press Library inspiration

This Go spike should use the following repo as product/design inspiration:

https://github.com/mvanhorn/printing-press-library

Do not copy code blindly. Use it as a reference for the mindset and structure of polished CLI-first tools.

Borrow these ideas:

- self-contained CLI binaries
- small memorable commands
- human-readable and agent-readable output
- local artifact/state management
- focused docs and skill files per workflow
- installable operator tools that become muscle memory
- clean command help, examples, and `doctor` checks
- Markdown/JSON outputs for both users and LLM agents

When uncertain about CLI ergonomics, repo organization, docs, or agent-skill packaging, inspect Printing Press Library for patterns and adapt the spirit, not the exact implementation.

## Purpose

This branch is a Go spike/rewrite experiment for `triage-cli`.

The current Python implementation is the behavioral reference, not sacred architecture. The goal is to evaluate whether Go provides a better long-term foundation for a polished, CLI-first operator tool.

This branch should **begin rewriting `triage-cli` from scratch in Go** while preserving the core product direction:

> A guided Zendesk ticket investigation assistant that helps a NOC/support engineer collect evidence, correlate logs/comments/attachments, produce an assessment, and generate a clean internal note or handoff.

The tool has two first-class product pillars:

1. **Guided Investigation**
2. **Automated Watcher**

Datadog is optional enrichment, not the core spine.

---

## Current product direction

The winning workflow is:

```txt
Zendesk ticket number or URL
→ guided triage and investigation
→ collect ticket context
→ ingest available evidence
→ parse/correlate logs/comments/attachments
→ produce assessment, likely root cause, and next steps
→ generate a Zendesk-ready internal note / handoff
```

The app should not be centered on:

```txt
Zendesk ticket
→ CNC/site lookup
→ Datadog query
→ Claude report
→ pretty terminal output
```

That earlier Datadog-heavy direction is now demoted.

Datadog, CNC/site lookup, and station-level querying may still exist later, but they are optional evidence sources.

---

## Current Python repo summary

The current repo is a Python CLI built around:

```txt
triage_cli/
  cli.py          Typer CLI: triage, watch, build-map
  zendesk.py      Zendesk ticket/comment fetch
  datadog.py      Datadog Logs API client
  extract.py      ticket ID parsing, site lookup, anchor/window logic
  llm.py          Claude Agent SDK calls and prompts
  models.py       Pydantic models: Ticket, Comment, LogLine, SiteEntry, TriageBundle, TriageReport
  pipeline.py     triage_one orchestration
  render.py       markdown/Rich rendering and save behavior
  watcher.py      Zendesk view polling loop
```

Useful existing concepts to preserve behaviorally:

- Accept Zendesk ticket ID or URL.
- Fetch ticket metadata, description, requester org, comments.
- Generate structured assessment/report.
- Save Markdown and JSON artifacts.
- Maintain watcher mode for Zendesk views.
- Preserve local-first operation.
- Preserve pipe-friendly output modes.
- Keep Datadog optional.
- Keep CNC/site mapping optional.

Do not blindly port the Python file structure. Design a clean Go architecture.

---

## Desired Go architecture

Recommended structure:

```txt
triage-cli/
  cmd/
    triage-cli/
      main.go

  internal/
    cli/
      root.go
      investigate.go
      triage.go
      watch.go
      doctor.go

    config/
      config.go

    zendesk/
      client.go
      models.go

    investigation/
      session.go
      service.go
      workflow.go

    evidence/
      evidence.go
      local_file.go
      paste.go
      attachment.go
      parser.go

    timeline/
      timeline.go
      normalize.go

    assessment/
      assessment.go
      prompt.go
      llm.go

    render/
      markdown.go
      json.go
      terminal.go

    watcher/
      watcher.go
      state.go

    store/
      paths.go
      artifacts.go

    integrations/
      datadog/
        client.go
      sitemap/
        map.go

  docs/
    product-direction-review.md
    guided-investigation.md
    watcher-mode.md
    go-spike-notes.md

  skills/
    triage-investigate/
      SKILL.md
    triage-watch/
      SKILL.md

  testdata/
    tickets/
    logs/
```

This is a suggestion, not a prison. Prefer clarity.

---

## Preferred Go libraries

Use pragmatic Go libraries that fit polished CLI development:

```txt
CLI: cobra
TUI: bubbletea + bubbles + lipgloss
Config/env: viper or small custom env loader
Markdown rendering: glamour if helpful
JSON: stdlib encoding/json
HTTP: stdlib net/http
Local storage: start with files; SQLite later if needed
Testing: stdlib testing + testify if useful
```

Do not introduce heavy architecture unless it directly serves the weekend spike.

---

## Commands to implement or stub

### `triage-cli investigate <ticket>`

Primary guided investigation mode.

For the spike, this may start as a terminal-guided flow before the full Bubble Tea TUI is complete.

Expected flow:

```txt
1. Parse ticket ID/URL.
2. Fetch Zendesk ticket, or use mock fixture if --mock.
3. Show initial ticket summary.
4. Review comments.
5. Show attachment metadata if available.
6. Prompt user for evidence:
   - local file
   - local directory
   - pasted text
   - skip
7. Normalize evidence into timeline events.
8. Generate or stub assessment.
9. Save Markdown and JSON handoff artifacts.
```

### `triage-cli triage <ticket>`

Fast one-shot non-interactive mode.

This should produce a report without launching the guided workspace.

### `triage-cli watch --view <id>`

Automated watcher mode.

This is a mainstay feature. For the first Go spike, it can be stubbed or minimally implemented, but the architecture should support it.

Expected behavior eventually:

```txt
poll Zendesk view
→ find new/updated tickets
→ run non-interactive triage
→ save artifacts
→ persist local state
→ repeat
```

### `triage-cli doctor`

Useful for polished CLI quality.

Should check:

```txt
Zendesk env/config present
Claude/LLM access configured if applicable
output directories writable
watcher state directory writable
optional Datadog config present or absent
```

### `triage-cli version`

Simple version/build info.

---

## Core domain model

Implement Go structs around the actual product workflow.

Suggested types:

```go
type InvestigationSession struct {
    Ticket     Ticket
    Evidence   InvestigationEvidence
    Timeline   []TimelineEvent
    Assessment *Assessment
    Report     *TriageReport
}

type InvestigationEvidence struct {
    TicketID        int64
    Comments        []Comment
    Attachments     []AttachmentEvidence
    LocalFiles      []LocalFileEvidence
    PastedLogs      []PastedEvidence
    OptionalSources []string
}

type AttachmentEvidence struct {
    Filename      string
    ContentType   string
    SizeBytes     int64
    Source        string
    LocalPath     string
    ExtractedText string
}

type LocalFileEvidence struct {
    Path          string
    SizeBytes     int64
    DetectedType  string
    ExtractedText string
}

type PastedEvidence struct {
    Label string
    Text  string
}

type TimelineEvent struct {
    Timestamp *time.Time
    Source    string
    Kind      string
    Message   string
    RawRef    string
}

type Assessment struct {
    Summary               string
    LikelyRootCause        string
    Confidence             string
    Correlation            []string
    Unknowns               []string
    NextSteps              []string
    SuggestedInternalNote  string
}

type TriageReport struct {
    TicketID      int64
    GeneratedAt   time.Time
    Sources       []string
    Assessment    Assessment
    EvidenceCount int
    Timeline      []TimelineEvent
}
```

Adjust as needed.

---

## Desired report sections

The final Markdown output should use this structure:

```md
# Triage Report — ZD-12345

## Initial Summary
What the ticket appears to report.

## Evidence Reviewed
Ticket comments, attachments, local logs, pasted logs, optional Datadog.

## Correlation
How evidence lines up across symptoms, timestamps, logs, and comments.

## Likely Root Cause
Clearly mark as confirmed, likely, possible, or unknown.

## Unknowns / Gaps
What evidence is missing or uncertain.

## Suggested Next Steps
Concrete next actions.

## Suggested Internal Note
Zendesk-ready internal note.
```

The output should be human-readable and agent-readable.

Add flags over time:

```txt
--json
--markdown
--no-color
--quiet
--output-dir
```

---

## TUI direction

Do not build an inbox-first TUI.

The eventual TUI should be a three-pane guided investigation workspace:

```txt
┌─ triage-cli · ZD-12345 · Guided Investigation ─────────────────────────────┐
│ Status: Evidence gathering · Sources: Zendesk, attachments, local logs      │
├──────────────────────────┬────────────────────────────────────────────────┤
│ Workflow                 │ Active Step                                    │
│                          │                                                │
│ ✓ Ticket loaded          │ Initial Assessment                             │
│ ✓ Comments reviewed      │                                                │
│ → Evidence intake        │ User reports intermittent audio loss.          │
│   Log parsing            │ Ticket contains 2 attachments and 4 comments.  │
│   Correlation            │                                                │
│   Assessment             │ Missing: workstation/station logs              │
│   Suggested next steps   │                                                │
│   Export note            │ [A] ingest attachments  [L] add local logs     │
├──────────────────────────┴────────────────────────────────────────────────┤
│ Evidence / Timeline                                                        │
│ 09:12  Ticket created: "Audio dropping from workstation"                   │
│ 09:18  Internal note: customer says issue began after reboot               │
│ 09:23  Attachment found: station_logs.zip                                  │
└────────────────────────────────────────────────────────────────────────────┘
```

Core workflow rail:

```txt
✓ Load ticket
✓ Review comments
→ Gather evidence
  Parse logs
  Build timeline
  Correlate signals
  Assess likely cause
  Suggest next steps
  Export handoff
```

---

## Subagent-driven development plan

Use subagents/worktrees logically if available. Suggested roles:

### 1. Architecture Agent

Tasks:

- Review current Python repo.
- Write `docs/go-spike-notes.md`.
- Define Go package boundaries.
- Confirm command structure.
- Identify must-preserve Python behaviors.
- Avoid overbuilding.

### 2. CLI Agent

Tasks:

- Set up Go module.
- Add Cobra root command.
- Implement/stub:
  - `investigate`
  - `triage`
  - `watch`
  - `doctor`
  - `version`
- Add global flags:
  - `--json`
  - `--no-color`
  - `--output-dir`
  - `--config`

### 3. Domain Model Agent

Tasks:

- Implement core structs.
- Add JSON serialization.
- Add tests for report/session structures.
- Keep models simple and readable.

### 4. Evidence Agent

Tasks:

- Implement local file ingestion.
- Implement pasted evidence ingestion.
- Implement basic log line normalization.
- Stub attachment metadata support if Zendesk attachments are not available yet.
- Build timeline events from ticket/comments/evidence.

### 5. Zendesk Agent

Tasks:

- Implement ticket ID/URL parsing.
- Implement Zendesk client using env vars.
- Fetch ticket metadata/comments.
- If attachments are complex, expose metadata first and defer download.
- Include `--mock` mode for local development.

### 6. Assessment Agent

Tasks:

- Implement deterministic assessment stub first.
- Add LLM integration later if straightforward.
- Render assessment into `TriageReport`.
- Ensure the report does not pretend certainty when evidence is thin.

### 7. Render/Artifact Agent

Tasks:

- Render Markdown.
- Render JSON.
- Save paired artifacts.
- Keep output pipe-friendly.
- Add terminal-friendly output but avoid making polish the core product.

### 8. Watcher Agent

Tasks:

- Port watcher state model concept.
- Minimal first pass is acceptable.
- Preserve direction:
  - poll Zendesk view
  - detect new/updated tickets
  - run non-interactive triage
  - save artifacts
  - persist state

### 9. QA Agent

Tasks:

- Add unit tests.
- Add fixtures.
- Run `go test ./...`.
- Run basic command smoke tests.
- Document what is incomplete.

---

## Overnight priorities

Do not attempt everything.

Prioritize in this order:

```txt
1. Go module skeleton
2. Cobra CLI commands
3. Core domain models
4. Mock `investigate` flow
5. Markdown/JSON artifact rendering
6. Zendesk ticket ID/URL parsing
7. Basic Zendesk client or stub with clear TODOs
8. Evidence ingestion from local file/paste
9. Timeline creation
10. Doctor command
11. Watcher skeleton
12. Bubble Tea TUI skeleton only if time remains
```

The spike is successful if by morning we can run something like:

```bash
go run ./cmd/triage-cli investigate 12345 --mock
```

and get:

```txt
ticket summary
evidence intake prompt or mock evidence
timeline
assessment
saved markdown/json artifacts
```

---

## Non-goals for this overnight spike

Do not prioritize:

```txt
Datadog integration
station-level Datadog querying
Slack notifications
posting back to Zendesk
multi-user backend
SQLite history
full MCP integration
production-grade Bubble Tea TUI
perfect parity with Python watcher
```

Those can come later.

---

## Product principles

The app should answer:

```txt
What is the ticket reporting?
What evidence do we have?
What evidence is missing?
What patterns correlate?
What is the likely cause?
What should we do next?
What should the internal note say?
```

It should feel:

```txt
calm
operational
evidence-first
fast
local-first
agent-readable
human-readable
friendly without being gimmicky
```

Avoid turning this into a flashy dashboard that hides the workflow.

---

## Final instruction

Work from the current repo, but do not be trapped by the Python architecture.

Use the Python version as a reference for behavior and edge cases.

Create a Go implementation that proves whether this tool wants to become a polished single-binary CLI.

Before making large changes, write a short `docs/go-spike-notes.md` explaining:

1. What was preserved from Python.
2. What was intentionally redesigned.
3. What was deferred.
4. How to run the Go spike.
5. What the next agent should work on.
