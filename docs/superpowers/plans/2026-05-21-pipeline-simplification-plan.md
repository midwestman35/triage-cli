# Pipeline Simplification Plan

Follow-up work on top of the `refactor/split-pipeline-module` branch (PR #40). This document captures the rationale and sequencing for the next round of edits identified by the thermo-nuclear code-quality review — starting at **item 2 (missed dramatic simplification opportunities)** because item 1 (file/module splits) is already in flight on this branch.

## Context

The v1 reframe got the contract right (structured pipeline, five-markdown folders, injected clients). Feature accumulation outpaced packaging: duplicated CLI orchestration, override-flag branching in every phase, nested reporter wrappers, and ad-hoc follow-up prompt assembly. The pipeline split (`pipeline/` directory, `PhaseCtx`, per-phase modules) is the prerequisite; this plan finishes the simplification layer on top.

## Locked decisions

| Question | Decision |
| --- | --- |
| Where does the shared runner live? | `pipeline/run.rs` — `run_investigation()` |
| Public flag name for stub LLM? | Keep `--no-llm`; map to `LlmMode::Stub` internally only |
| What does `revise` use? | Own opts builder + `SilentReporter`; no shared runner, no `CliReporter` |

## Scope

**In:**

- CLI dedupe via `pipeline/` runner
- Resolved inputs (`MemorySource`, `HistorySource`, `LlmMode`) at the pipeline boundary
- `CliReporter` for CLI paths only
- `chat::assemble_followup_system_prompt` extraction
- Wiring through `cli.rs`, `watcher.rs`, `tui/inbox/`
- `revise` opts-builder touch only (reporter unchanged)

**Out:**

- Item 1 file splits already on this branch (`pipeline/`, `tui/inbox/`, `models` boundary)
- Renaming `--no-llm` to `--stub`
- Giving `revise` the shared runner or `CliReporter`
- Golden-test fixture content changes

---

## PR 1 — Fixture bundle loader (foundation)

**Goal:** One canonical way to load fixture directories; delete copy-pasted loader blocks in CLI commands.

**Changes:**

- Add `fixture::load_bundle(path) -> Result<FixtureBundle, FixtureError>` in `fixture.rs` returning `{ ticket, datadog_logs, memory_hits }` (wrap existing `FixtureLoader` methods).
- Add `fixture::load_named(name) -> Result<FixtureBundle, FixtureError>` for `demo`.
- Replace inline loader blocks in `cli.rs` (`cmd_triage` fixture branch, `cmd_demo`).

**Verify:**

- `cargo test -p triage-cli fixture::`
- `triage-cli demo audio-drop` — byte-stable output unchanged

---

## PR 2 — Shared runner in `pipeline/`

**Goal:** Single pipeline entry for all non-`revise` callers; CLI keeps stdout/exit-code concerns.

**Changes:**

- Add `pipeline/run.rs` (re-export from `pipeline/mod.rs`):

  ```
  pub struct InvestigationRun { ticket, session, clients, opts, reporter }
  pub async fn run_investigation(run: InvestigationRun)
      -> Result<StructuredInvestigation, PipelineError>
  ```

- Post-run hooks (`print_fork_packet`, `metrics_path`, soft-lock diff) stay in `cli.rs` — `run_investigation` returns `StructuredInvestigation` only.
- Add `InvestigateOptions::from_common(...)` on `options.rs` to DRY repeated field sets (`cnc`, `site`, `anchor`, `window`, `levels`, `verbose`, `force`, `no_llm`, etc.).
- Refactor `cmd_triage` (live + fixture) and `cmd_investigate` tail to: build opts → build `Clients` → wrap reporter → `pipeline::run_investigation` → shared post-run block.
- Refactor `cmd_demo` to use `run_investigation` + plain `StderrReporter` (no metrics).

**Verify:**

- `cargo test`, `cargo clippy --all-targets -- -D warnings`
- Manual `triage --fixture …` and `investigate` smoke

---

## PR 3 — Watcher + inbox adopt `pipeline::run_investigation`

**Goal:** Delete duplicate client/rubric/session wiring outside CLI.

**Changes:**

- Add `InvestigateOptions::from_watcher(WatcherOptions)` on `options.rs` (replaces literal in `watcher.rs`).
- Refactor `watcher.rs` to call `pipeline::run_investigation` with `SilentReporter`.
- Refactor `tui/inbox/` `run_pipeline` to call `pipeline::run_investigation` with existing inbox reporter (`ChannelReporter` / phase events) — keep `opts_to_investigate` as a thin wrapper over `from_watcher` + site override.
- Do **not** route inbox through CLI post-run hooks.

**Verify:**

- `cargo test` (no TTY required)
- Inbox compile + existing inbox unit tests

---

## PR 4 — `CliReporter`

**Goal:** Collapse `MetricsReporter > SpinnerReporter > StderrReporter` nesting in CLI paths.

**Changes:**

- Add `pipeline/reporter.rs::CliReporter { verbose, spinner, metrics: MetricsCapture }` — single `Reporter` impl for CLI.
- Wire in `cli.rs` only:
  - `triage` → `CliReporter::new(verbose, spinner: false, metrics: true)`
  - `investigate` → `spinner: true`
- Remove `SpinnerReporter` and nested boxing from CLI call sites if no other uses remain.
- **Leave `revise` on `SilentReporter`** — no change to `pipeline/revise.rs` reporter wiring.

**Verify:**

- `--metrics-out` JSON shape unchanged (spot-check keys)
- Spinner visible on `investigate`, absent on `triage`

---

## PR 5 — Resolved inputs at pipeline boundary

**Goal:** Phases stop reading override flags; one mapping function at pipeline entry.

**Changes:**

- Add `pipeline/resolved.rs`:

  ```
  MemorySource::Live | Prefilled(Vec<MemoryEntry>)
  HistorySource::Live | Prefilled(CustomerHistoryEvidence) | Skip
  LlmMode::Live | Stub    // Stub ← opts.no_llm inside resolve_inputs only
  ```

- Add `resolve_inputs(opts, clients) -> ResolvedInputs`; call once in `run_investigation` and attach to `PhaseCtx`.
- Simplify `phases/history.rs`, `phases/memory.rs`, `phases/llm.rs` to read `ctx.resolved.*` only.
- **`--no-llm` stays the public CLI flag** — no rename.
- `revise`: add `InvestigateOptions::for_revise(...)` builder only; still uses `SilentReporter`.

**Verify:**

- Full `cargo test`
- Fixture / demo / `--no-llm` stub report byte-stable

---

## PR 6 — Override field cleanup

**Goal:** Remove override fields from the public options surface.

**Changes:**

- Add builders: `InvestigateOptions::for_live(...)`, `::for_fixture(&FixtureBundle, ...)`, `::for_revise(...)`.
- Migrate all call sites; remove `customer_history_override` / `memory_hits_override` from public struct (or `#[doc(hidden)]` during transition).
- Grep: zero override reads outside `resolve_inputs`.
- Update `AGENTS.md` / `CLAUDE.md`: document `ResolvedInputs` + `--no-llm → Stub` mapping.

---

## PR 7 — Follow-up system prompt assembly

**Goal:** Move prompt assembly out of `pipeline/followup.rs` into `chat` where the helpers already live.

**Changes:**

- Add `chat::assemble_followup_system_prompt(ticket_dir, caller_system, prior_turns, resume_session) -> String` — redact caller, preamble, conditional replay, combined cap via existing `truncate_on_boundary`.
- Replace inline block in `pipeline/followup.rs` (~77–105) with one helper call; keep lock/session-id logic in `followup.rs`.
- Unit tests in `chat.rs`: empty caller, preamble present, replay only when `resume_session`, cap truncation.

**Verify:**

- `cargo test chat::`

---

## PR 8 — Validation gate

- `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all -- --check`
- Smoke matrix: `demo audio-drop`, `triage --fixture … --no-llm`, soft-lock exit code 2, `--metrics-out` JSON shape
- Line check: `cli.rs` should drop ~150+ lines; no new file crosses 1k from these changes alone

---

## Branch sequence

```
refactor/split-pipeline-module   (this PR — merge first)
  └─ simplify/fixture-bundle
       └─ simplify/pipeline-run-investigation    ← runner in pipeline/
            └─ simplify/cli-reporter              ← cli.rs only
                 └─ simplify/resolved-inputs      ← --no-llm preserved
                      └─ simplify/followup-prompt
```

PRs 1–3 can stack quickly and merge independently. PR 5–6 is highest-risk — keep isolated and test-heavy. PR 7 is independent and can land anytime after the pipeline split merges.

---

## Rationale summary (thermo-nuclear review items 2–4)

### Item 2 — Missed dramatic simplification

| Problem | Remedy in this plan |
| --- | --- |
| `cmd_triage` / `cmd_investigate` / `cmd_demo` duplicate opts + pipeline + post-run | PR 2 shared runner; PR 1 fixture helper |
| `InvestigateOptions` god struct with override flags | PR 5–6 `ResolvedInputs` |
| `MetricsReporter > SpinnerReporter > StderrReporter` nesting | PR 4 `CliReporter` |
| Watcher/inbox duplicate wiring | PR 3 |

### Item 3 — Spaghetti / branching

| Problem | Remedy |
| --- | --- |
| Override checks in every phase (`history`, `memory`, `llm`) | PR 5 resolve once at entry |
| Ad-hoc system prompt assembly in `followup_turn` | PR 7 `chat::assemble_followup_system_prompt` |
| Enrichment nesting | Already addressed by `pipeline/phases/enrichment.rs` on this branch |

### Item 4 — Boundary leaks (partial; rest in item 1)

| Problem | Remedy |
| --- | --- |
| `TriageBundle::as_user_message` in `models.rs` | Addressed on this branch (`models/prompt.rs` split) |
| STATE.md parsing in TUI | Addressed on this branch (`tui/inbox/state.rs`) |
| Follow-up prompt logic in pipeline | PR 7 |

---

## Approval bar (for the simplification PRs)

Do not merge a simplification PR merely because behavior seems correct. Each PR should:

- Not introduce new override-flag reads in phase modules (after PR 5)
- Not add nested reporter boxing in CLI (after PR 4)
- Preserve `--no-llm` / demo byte-stable output
- Keep `revise` on `SilentReporter` with its own opts path
- Pass clippy + full test suite
