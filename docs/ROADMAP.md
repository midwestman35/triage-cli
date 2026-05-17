# ROADMAP

Living index of accepted-but-deferred work. Items leave this file when they ship (move to a closed ADR or a changelog entry); they enter it from `docs/decisions/` or from a manual planning pass.

Each entry: **what / why / blocker / sketch of approach / estimated PR shape**.

---

## Now (in flight)

Tracked in the active branch / open PR — not duplicated here.

## Next up

### 1. Interactive investigation — inbox TUI chat pane (V1 narrow loop)

**What.** A sixth tab in the inbox TUI (`CHAT`) that lets the analyst revisit
a ticket after the initial `investigate` run, ask follow-up questions to the
same LLM, attach new evidence, and optionally re-emit the five-markdown
folder via an explicit `/revise` slash-command. **State lives as JSONL**
(`Tickets/<id>/CONVERSATION.jsonl`, source of truth) with a derived markdown
renderer for human reading. The original investigation writes durable JSON
snapshots (`Tickets/<id>/.session/base-ticket.json`,
`.session/base-evidence-manifest.json`) so `/revise` rebuilds from machine-
readable state, not parsed markdown. Per-ticket advisory file lock at
`.session/lock` protects concurrent writers.

**Why.** The pipeline is single-shot today. Once `investigate <id>` writes
the ticket folder, the analyst's only options are to re-run the whole
pipeline (losing context) or open a separate `codex` terminal session in the
same directory (losing integration with the inbox, the soft-lock, and the
analyst's mental model of "the ticket lives in `triage-cli`"). Customers
bringing back new evidence days later is the dominant case this misses.
Brainstormed and approved 2026-05-17; revised 2026-05-17 after adversarial
design review.

**Blocker (gate #0).** Codex contract verification (`codex exec --json`
session-ID surface vs stderr regex vs fallback-to-replay). If the contract
gate fails, codex falls back to replay-context like unleash and the v1
ships without native session continuity — chat loop, schema, lock, and
/revise still work.

**V1 narrow loop ships:**
- Open chat tab, render existing CONVERSATION.jsonl
- Append analyst turn with plain-path file attach + `label=body` paste
- Ask follow-up via `provider.followup` (codex native or unleash replay)
- Retry on failure
- `/revise` re-enters the structured pipeline against the JSON snapshot
- Per-ticket advisory lock; evidence provenance (sha256, copied path,
  truncation status, sent_to_provider)
- Static braille throbber while a call is in flight
- Migration path for tickets predating the feature (reconstruct
  base-snapshots once, then JSON is authoritative)

**Deferred to V2 (tracked in #2 below):**
- Custom in-TUI file picker (replaces the plain path prompt)
- `$EDITOR` integration (`Ctrl-E` suspend-into-editor)
- Animated gradient spinner (`throbber-widgets-tui` dependency)
- Image attachments via `codex -i`
- Multi-line `Shift+Enter` input
- Automated turn writers (schema slot ships in v1; writers in v2)

**Sketch.**
- `tui/chat.rs` (new): ratatui chat pane (transcript view, input modal,
  command bar). Renders parsed `Turn` structs with color-coded headers
  per turn_kind.
- `chat.rs` (new logic module): `parse_conversation_jsonl` /
  `append_turn` / `render_conversation_md`; `ChatCommand` slash-command
  enum + parser; evidence intake with provenance; session manifest +
  snapshot read/write; per-ticket lock.
- `providers/mod.rs`: add a default-impl `LlmProvider::followup` method.
  `providers/codex.rs` overrides with `codex exec resume <id>` using the
  capture method selected by the contract gate. `providers/unleash.rs`
  uses the default.
- `pipeline.rs`: new `pipeline::followup_turn` entry point (does NOT
  touch the five-markdown folder; appends to JSONL only) and a
  `followup_mode: bool` flag on `investigate_one_structured` that gates
  the revise re-entry (load base snapshots, preserve CONVERSATION.jsonl,
  record revise entry in STATE.md.validator_warnings).
- Provider-mismatch degrades gracefully — replay under the active
  provider with a banner. Hard-refuse only under explicit
  `chat.require-native-resume` opt-in.

**Estimated PR shape (V1).** ~1500 lines total: `tui/chat.rs` (~450),
`chat.rs` (~350), provider trait + codex impl (~250), pipeline glue
(~200), tests (~250). Slightly larger than the initial estimate; the
JSONL + lock + provenance + migration path all add 25-30% over the
original markdown-as-state design.

Full design: `docs/superpowers/specs/2026-05-17-interactive-investigation-design.md`.

---

### 2. Automation hooks for chat pane (follow-on to #1)

**What.** A small set of automated producers that write `turn_kind: automated`
turns to `CONVERSATION.md` without analyst intervention, so the inbox chat
pane becomes a notification surface as well as a Q&A surface. Initial set:

- **Comment-arrival watcher.** Extend `watcher.rs` to detect new Zendesk
  comments on tickets that already have a `CONVERSATION.md`. On detection,
  call `pipeline::followup_turn` with a system-supplied prompt
  ("a new customer comment just arrived, summarize what changed and flag
  anything that affects the existing fork commitment") and append the
  resulting automated turn.
- **Datadog-drift watcher (optional, gated behind a flag).** Periodically
  re-pull Datadog logs for the original incident window; if new log lines
  appear (e.g. delayed ingestion), append an automated turn summarizing
  what's new.
- **Stale-investigation reminder.** A daily pass that finds tickets with
  `STATE.md.status == "open"` and no analyst turn in the last N days; posts
  an automated system turn asking "still waiting on the customer?" so the
  next analyst opening the inbox sees the prompt.

**Why.** v1 of #1 deliberately exposes three extension points
(`turn_kind: automated`, public `pipeline::followup_turn`, `ChatCommand`
enum) so this can be built additively. The NOC's goal over the next few
weeks (per user direction 2026-05-17) is to lean into automated triage and
ticket-fetching; the chat pane is the natural place to surface those
automated observations to the analyst.

**Blocker.** #1 (chat pane must exist; CONVERSATION.md schema and
`pipeline::followup_turn` must be public). Also: the watcher needs a
"which tickets have ongoing investigations" view; that's a 1-LOC change
since `tickets_root()` already scans for STATE.md.

**Sketch.**
- `watcher.rs`: extend `run_iteration` with a new pass that walks
  `tickets_root()` for tickets whose `Tickets/<id>/CONVERSATION.md` exists
  and `STATE.md.status == "open"`. For each, compare the existing latest
  comment timestamp with the live Zendesk view; if newer, fire an automated
  follow-up turn.
- `chat::append_automated_turn(ticket_id, source, body, attachments)` is
  the single helper the watcher calls.
- Add a `WatcherOptions::auto_followup_enabled` flag (default off) so the
  feature ships gated until the NOC has validated the prompt quality.
- Add a CLI flag `triage-cli watch --auto-followup` that flips the gate on
  per-run, plus an env var `TRIAGE_AUTO_FOLLOWUP=1` for daemonized usage.
- Prompt templates live in a new `chat/automation-prompts/` directory,
  loadable at build time via `include_str!` (same pattern as the embedded
  fork rubric in `playbook.rs`).

**Estimated PR shape.** ~600 lines for the comment-arrival watcher and the
`append_automated_turn` helper. Datadog-drift watcher and stale-investigation
reminder are separate PRs against the same hook.

---

### 3. Evidence-ID model v2

**What.** Every `EvidenceItem` (Zendesk comment, attachment, Datadog log, local file, pasted note, memory hit) gets a stable `id` of the form `E-001`, `E-002`, …, assigned deterministically by the pipeline. `FORK_PACKET.md` cites these IDs in its "Evidence used" section. The LLM is required to quote IDs, not paraphrase ("according to E-007 the console rebooted at …").

**Why.** Makes routing decisions auditable. A reviewer can trace the chain from `FORK_PACKET.md` → `EVIDENCE_PREFLIGHT.md` row → raw source. Today the chain is implicit.

**Blocker.** None.

**Sketch.**
- `models.rs`: add `id: String` to `EvidenceItem`. Assignment: zero-pad to three digits, sort key is `(type, source_time, source_path)` so the same inputs produce the same IDs.
- `pipeline.rs`: assign IDs in a single pass after bundle assembly, before the LLM call.
- LLM prompt: spec the citation format and the soft-warn validator pattern (`E-\d{3}` not found in bundle → warning).
- `playbook.rs`: extend the rubric validator with a second check (`Rubric::validate_citations(report, bundle)`).
- Writers: `EVIDENCE_PREFLIGHT.md` adds an `ID` column; `FORK_PACKET.md` "Evidence summary" bullets lead with the ID.

**Estimated PR shape.** Spec amendment + ~200 lines of code + tests for the assignment-stability and validator paths.

---

### 4. Fixture / demo mode

**What.** Ship a small set of canned investigation inputs that exercise the pipeline end-to-end without real Zendesk, Datadog, or LLM calls. Two surfaces:

```
triage-cli demo audio-drop
triage-cli investigate --fixture fixtures/audio-drop/
```

A fixture is a directory containing:
- `ticket.json` (full Zendesk ticket shape)
- `comments.json`
- `attachments/` (sample log files)
- `datadog-logs.json`
- `memory-hits.json`
- `expected/` (the five-file ticket folder the pipeline should produce, byte-identical when `--no-llm` is set)

Initial cases: `audio-drop`, `no-site-map`, `missing-evidence`, `vendor-fork`.

**Why.** Three wins at once: (a) the project is runnable without credentials, which is the difference between "demoable to a teammate in 30 seconds" and "requires an .env brief"; (b) it is the only sane way to run regression tests against real LLM behavior; (c) it makes the project safer to share publicly.

**Blocker.** None; sequencing matters — do this before `--metrics-out` so the demo cases are the test bed for the metrics shape.

**Sketch.**
- Introduce a `FixtureZendeskClient` and `FixtureDatadogClient` implementing the same surface as the real clients. Toggle via a `--fixture <path>` flag that swaps the trait objects at `cmd_investigate` / `cmd_triage` construction time.
- A `cmd_demo` subcommand is a thin wrapper that resolves a friendly name (`audio-drop`) to a path in `fixtures/`.
- Fixtures live under `triage-cli-rs/fixtures/`. The crate ships them; they are not embedded.

**Estimated PR shape.** ~400 lines including fixture files. The first fixture is the work; subsequent ones are cheap.

---

### 5. Golden output tests

**What.** A test runner that, for each fixture, runs the pipeline in `--no-llm` mode and asserts the produced five files are byte-identical to the `expected/` directory. Failure prints a unified diff.

**Why.** This is the safety net the project has been missing. With it, every refactor of `ticket_folder.rs`, `pipeline.rs`, or the report writers is provably non-breaking. Without it, "did this PR change the ticket-folder output?" is an open question on every change.

**Blocker.** #4 (fixtures must exist first).

**Sketch.** Inline `#[test]` in `ticket_folder.rs` (or a new `tests/golden.rs`). Walk `fixtures/`, run the pipeline, diff. Update-fixture mode behind `UPDATE_GOLDEN=1` env var.

**Estimated PR shape.** ~150 lines.

---

### 6. `--metrics-out` flag

**What.** Every run optionally writes a JSON record to a caller-supplied path:

```json
{
  "ticket_id": 12345,
  "phases": {"zendesk_fetch": 0.42, "enrichment": 1.10, "llm_call": 3.85, "save": 0.05},
  "evidence_counts": {"comments": 7, "attachments": 2, "datadog_lines": 41, "memory_hits": 3},
  "llm": {"provider": "unleash", "model": "...", "tokens_in": 4200, "tokens_out": 980, "retried": false},
  "validator_warnings": ["..."],
  "fork": "B",
  "confidence": "high",
  "exit_code": 0
}
```

**Why.** Module 4 of the assessment ("observability"). Lets the operator collect run-level telemetry without instrumenting at the call site.

**Blocker.** None strictly, but cleaner *after* the `Reporter` trait grows a structured `record_metric(key, value)` method so timings flow through the existing phase hooks instead of being bolted on next to `eprintln!`. (Reporter::record_metric was already added in May 2026 — this item may be partially shipped; verify before scoping a PR.)

**Sketch.**
- `pipeline.rs`: `Reporter::record_metric(&self, key: &str, value: MetricValue)`; default impl is a no-op so existing `StderrReporter` / `SilentReporter` / `ChannelReporter` keep working.
- New `MetricsReporter` (wraps another reporter, captures records into a `Vec<RunMetric>`, returns them at end).
- `cli.rs`: `--metrics-out <PATH>` on `triage` and `investigate`. Pipeline wires `MetricsReporter` over the chosen base reporter.
- Provider trait: extend `complete` to return token usage when the provider knows it (unleash, openai do; codex subprocess does not).

**Estimated PR shape.** ~300 lines including reporter refactor.

---

## Maybe / unscheduled

### `--no-llm` baseline runbook entry

Already shipped as a flag (`cli::TriageCmd.no_llm`, dispatched via `pipeline::stub_assess_structured`). Worth a paragraph in `docs/runbooks/02-triaging-a-ticket.md` once the use case for it ("CI smoke check; demo without a model") matures. Low priority.

### Confidence calibration

Track, over time, how often the LLM-reported confidence matches operator outcome. Needs the metrics record (#4) and a small `calibration.jsonl` append log. Real product work, not infrastructure.

### Eval harness for fork accuracy

Once fixtures (#4) exist with known-good fork decisions, run the LLM against each fixture N times and report agreement / drift. This is what the assessment calls Module 5 / Phase 4. Needs #4 first.

---

## Will not do

- **Move prototypes to Python.** Rejected in `docs/decisions/2026-05-14-adversarial-review-of-career-notes.md`. Rust port stays the single source of truth.
- **Pause `watch` or inbox TUI.** Same source — both are load-bearing in the spec.
- **Autonomous Zendesk writes.** Not on the roadmap; the CONFIRM-gated drafts pattern is the contract.
- **More LLM providers.** Direction is the opposite — see `docs/adr/0002-prune-claude-openai-providers.md`.
