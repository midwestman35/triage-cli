# ROADMAP

Living index of accepted-but-deferred work. Items leave this file when they ship (move to a closed ADR or a changelog entry); they enter it from `docs/decisions/` or from a manual planning pass.

Each entry: **what / why / blocker / sketch of approach / estimated PR shape**.

---

## Now (in flight)

Tracked in the active branch / open PR — not duplicated here.

## Next up

### 1. Evidence-ID model v2

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

### 2. Fixture / demo mode

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

### 3. Golden output tests

**What.** A test runner that, for each fixture, runs the pipeline in `--no-llm` mode and asserts the produced five files are byte-identical to the `expected/` directory. Failure prints a unified diff.

**Why.** This is the safety net the project has been missing. With it, every refactor of `ticket_folder.rs`, `pipeline.rs`, or the report writers is provably non-breaking. Without it, "did this PR change the ticket-folder output?" is an open question on every change.

**Blocker.** #2 (fixtures must exist first).

**Sketch.** Inline `#[test]` in `ticket_folder.rs` (or a new `tests/golden.rs`). Walk `fixtures/`, run the pipeline, diff. Update-fixture mode behind `UPDATE_GOLDEN=1` env var.

**Estimated PR shape.** ~150 lines.

---

### 4. `--metrics-out` flag

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

**Blocker.** None strictly, but cleaner *after* the `Reporter` trait grows a structured `record_metric(key, value)` method so timings flow through the existing phase hooks instead of being bolted on next to `eprintln!`.

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

Once fixtures (#2) exist with known-good fork decisions, run the LLM against each fixture N times and report agreement / drift. This is what the assessment calls Module 5 / Phase 4. Needs #2 first.

---

## Will not do

- **Move prototypes to Python.** Rejected in `docs/decisions/2026-05-14-adversarial-review-of-career-notes.md`. Rust port stays the single source of truth.
- **Pause `watch` or inbox TUI.** Same source — both are load-bearing in the spec.
- **Autonomous Zendesk writes.** Not on the roadmap; the CONFIRM-gated drafts pattern is the contract.
- **More LLM providers.** Direction is the opposite — see `docs/adr/0002-prune-claude-openai-providers.md`.
