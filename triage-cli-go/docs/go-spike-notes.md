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
- **Single binary; LLM is opt-out, not opt-in.** The default assessor
  shells out to the local `claude` CLI (preserving the no-API-key UX
  of the Python tool, which uses the Agent SDK). `--no-llm` falls
  back to the deterministic stub; if `claude` is missing on PATH,
  we warn and fall back automatically. Mock fixtures still let
  `investigate --mock --no-llm` run with zero external dependencies.
- **Cobra-first command surface** with a `doctor` command for env checks
  and a `version` command — patterns borrowed from polished operator
  CLIs (Printing Press Library mindset).

## What is deferred

Explicitly out of scope for this spike:

- Live Datadog client.
- Streaming-style assessor integration (`claude --output-format
  stream-json`); the TUI today renders phase-level events but the
  assessment phase is still a single blocking call.
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

1. Attachment download + text extraction.
2. Watcher: actual polling loop against Zendesk views.
3. Bubble Tea three-pane TUI for `investigate`, kept as a flag
   (`--tui`); the linear flow remains the default for piping.
4. Optional Datadog evidence source.
5. `build-map` command parity (parse `apex-cnc-inventory.md`).

## Iteration log

### 2026-05-08 — Iteration 1: live Zendesk HTTP client

Shipped:

- `internal/config/zendesk.go` — `LoadZendesk()` reads
  `ZENDESK_SUBDOMAIN/EMAIL/API_TOKEN`, normalizes pasted URLs, errors
  list every missing variable in one message.
- `internal/zendesk/types.go` — wire-format structs (`apiTicket`,
  `apiComment`, `apiUser`, `apiOrgResponse`, …) kept package-private,
  plus `mapTicket(...)` to project them onto `model.Ticket`.
- `internal/zendesk/client.go` — `HTTPFetcher` with HTTP Basic
  (`<email>/token:<token>`), 30 s default timeout, paginated
  `comments.json` walk capped at 500, best-effort `users` →
  `organizations` lookup that never fails the fetch, status-aware error
  hints (401/403/404/429), context cancellation propagation.
- `internal/zendesk/client_test.go` — `httptest.NewServer`-driven
  coverage: happy path, pagination, 401/404/5xx, org lookup failure,
  context cancellation, and a comment-cap test that would otherwise
  loop forever.
- `internal/cli/investigate.go` + `triage.go` — `--mock` still uses the
  fixture fetcher; without `--mock`, `config.LoadZendesk()` builds an
  `HTTPFetcher`. New `--timeout` duration flag overrides the client
  timeout.
- `internal/cli/doctor.go` — when all three env vars are set, doctor
  performs a 5 s `GET /api/v2/users/me.json` probe and prints one of
  reachable / authentication failed / HTTP <status> / network error.
  Reachability failures stay warnings, not critical.

Verified: `gofmt -l .` clean, `go vet ./...` clean,
`go test -race ./... -count=1` green, smoke runs match the success
target. The error printed when `investigate 12345` runs without env is:
`zendesk config: missing required environment variable(s):
ZENDESK_API_TOKEN, ZENDESK_EMAIL, ZENDESK_SUBDOMAIN`.

### 2026-05-08 — Iteration 2: claude CLI Assessor

Shipped:

- `internal/assessment/prompt.go` — `BuildPrompt(session)` is a pure
  string-builder that emits the system instruction, the literal JSON
  schema (matching `model.Assessment`'s tags) with confidence
  calibration and a thin-evidence example, then `=== TICKET ===` /
  `=== EVIDENCE ===` / `=== TIMELINE ===` blocks and a closing
  "Output ONLY the JSON object" instruction.
- `internal/assessment/claudecli.go` — `ClaudeCLIAssessor` shells out
  to `claude -p <prompt> --output-format json [--model <m>]`. Parses
  the `{"type":"result","subtype":"success","result":"..."}` wrapper,
  strips ```json``` fences and surrounding prose with a balanced-brace
  scanner that respects strings/escapes, validates Confidence enum +
  required non-empty fields, and maps errors:
  - missing binary → `ErrClaudeNotFound` sentinel (caller can
    `errors.Is` against it)
  - non-zero exit → wraps stderr (truncated to 1KB)
  - JSON parse failure → wraps with truncated raw output
  - context cancellation propagates
  `Exec` is an injectable `ExecFunc` for tests; the default uses
  `exec.CommandContext` and (in `--llm-verbose`) mirrors stderr to
  `os.Stderr` via `io.MultiWriter`.
- `internal/assessment/claudecli_test.go` — table-driven coverage of
  happy path, fence stripping, prose-around-JSON, invalid Confidence,
  empty Summary, non-zero exit, `ErrClaudeNotFound` sentinel match,
  context cancellation, wrapper-reports-error, `extractJSONObject`
  edge cases, and `BuildPrompt` field presence.
- `internal/cli/investigate.go` — adds `--no-llm`, `--llm-model`,
  `--llm-verbose` flags via `addCommonInvestigateFlags`. New
  `selectAssessor` chooses between `StubAssessor` (when `--no-llm`)
  and a `fallbackAssessor` that delegates to the claude CLI but
  silently falls back to the stub on `ErrClaudeNotFound` (with a
  stderr warning). All other claude errors surface to the operator.
- `internal/cli/triage.go` — uses the same flag helper, no
  duplication.
- `internal/cli/doctor.go` — adds `probeClaudeCLI` which checks
  `exec.LookPath`, runs `claude --version` with a 5s timeout, and
  prints `✓ claude: <version>` / `✗ claude: ... failed: ...` /
  `− claude: not on PATH (...)`.
- `internal/investigation/flow.go` — phase 6 status string changed
  from `Running assessment (stub)...` to `Running assessment...`
  since the assessor is now operator-selected.

Confirmed wrapper shape from `claude --output-format json` (claude
2.1.123): `{"type":"result","subtype":"success","is_error":false,
"result":"<text>","duration_ms":...,"usage":{...},"session_id":...}`.

Verified: `gofmt -l .` clean, `go vet ./...` clean, `go test -race
./... -count=1` green. `investigate 12345 --mock` with claude
available produces a content-aware assessment (Confidence: likely;
ties SBC jitter windows to reported drop times); `--mock --no-llm`
produces the deterministic stub.

### 2026-05-08 — Iteration 3: Bubble Tea three-pane TUI

Shipped:

- `internal/investigation/flow.go` — pipeline now emits typed
  `Event`s through a `Reporter` interface. `Phase` is an enum of the
  six pipeline stages (`PhaseLoadTicket`…`PhaseAssess`); `TotalPhases`
  is the (constant) denominator. Three reporter implementations live
  here: `NopReporter` (default when `Deps.Reporter` is nil),
  `StderrReporter{Quiet}` (the previous behavior, lifted into a
  struct), and `ChanReporter{Ch}` (non-blocking forward to a channel
  for the TUI). `RunOpts.Guided`/`Quiet` were removed — the choice of
  reporter is the new lever.
- `internal/tui/` — opt-in three-pane bubbletea program. `model.go`
  defines the `tea.Model` state (phase, per-step status,
  ticket/report data, two `bubbles/viewport`s, focus). `view.go`
  composes the layout: header (ticket id · subject · status; sources
  line below), upper row split into Workflow rail (left, ⅓ width) and
  Active Step pane (right, ⅔ width), full-width Evidence/Timeline
  pane below, footer with key hints. `update.go` handles
  `WindowSizeMsg`, `KeyMsg` (`q`/`ctrl+c` quit, `tab`/`shift+tab`
  cycle focus, arrows/pgup/pgdn forward to the focused viewport,
  `enter` focuses the report viewer post-completion), and the custom
  message types (`PhaseEventMsg`, `TicketLoadedMsg`,
  `EvidenceAddedMsg`, `AssessmentDoneMsg`, `PipelineDoneMsg`,
  `PipelineErrorMsg`). `program.go` exposes
  `tui.Run(ctx, ticketID, noColor, runner)`: it spins up the alt-
  screen bubbletea program, pumps runner events into it via
  `prog.Send`, and returns the final `*model.TriageReport`,
  `tui.ErrUserCancelled`, or a pipeline error. `styles.go` defines
  the colour palette and respects `--no-color`.
- `internal/cli/investigate.go` — adds `--tui` flag. Mutually
  exclusive with `--json` and `--quiet` (typed errors). On `--tui`,
  `runPipelineTUI` builds the same `Deps` + `RunOpts`, wraps the
  fetcher in a `ticketTapFetcher` so the loaded ticket reaches the
  TUI, and saves paired artifacts after the program exits cleanly.
  User cancellation prints `→ cancelled` to stderr and exits 0; the
  artifact write is skipped. Non-TTY environments get a friendly
  hint instead of the raw bubbletea `/dev/tty` error.
- Tests: `internal/investigation/flow_test.go` adds
  `TestRun_ChanReporterEmitsAllPhasesInOrder` (asserts six phase
  events in declared order plus the final `Done` event) and
  `TestNopReporter_DoesNotPanicOnNilDeps`. `internal/tui/model_test.go`
  drives the model directly: `View()` snapshot at 80×24 confirms
  ZD-12345 / Workflow / each phase / Evidence-Timeline pane / footer
  appear; phase events flip `stepStatus` from pending → active →
  done; `q` returns `tea.Quit`; `WindowSizeMsg` updates dimensions;
  `PipelineDoneMsg` swaps the right pane to the report viewer.
- `go.mod` — added `github.com/charmbracelet/bubbletea v1.3.10`,
  `github.com/charmbracelet/lipgloss v1.1.0`,
  `github.com/charmbracelet/bubbles v1.0.0`. No other new direct
  deps.

Linear flow regression-checked: stderr output for
`investigate 12345 --mock --no-llm` is identical to pre-iteration
(`→ [N/6] …` plus the two `→ saved …` lines). The new `Done` event
is suppressed in `StderrReporter` so the trailing line is unchanged.

## TUI controls

When `investigate --tui` is active:

| Key | Action |
| --- | --- |
| `q`, `ctrl+c` | Quit (cancels pipeline if still running) |
| `tab` / `shift+tab` | Cycle focus between Active and Timeline panes (and Report once complete) |
| `↑` / `↓` / `pgup` / `pgdn` | Scroll the focused viewport |
| `enter` | (post-completion) Focus the report viewer |
