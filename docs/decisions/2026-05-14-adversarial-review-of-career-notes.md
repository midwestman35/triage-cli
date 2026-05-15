# Adversarial review — Career Project Notes assessment (2026-05-14)

Source: `claude-goal-5.11` (an external assessment of triage-cli's direction).

This document records, for each recommendation in the assessment, whether the project will adopt it, why, and where in the codebase it will land — or why it is being rejected or deferred.

## Verdict table

| # | Recommendation (paraphrased) | Verdict | Reason |
| - | --- | --- | --- |
| 1 | Project framing: AI-assisted incident triage workbench for NOC / reliability teams | **ACCEPT** | Matches the v1 spec (`docs/spec/v1-reframe.md`) and the README. No code change. |
| 2 | "Deterministic pipeline gathers facts; LLM may summarize, classify, draft — not invent" | **ACCEPT (already enforced)** | This principle is the spec. It is encoded in three places: `redact.rs` (PII redaction before the model sees the bundle), `playbook.rs` (rubric soft-validator that flags when the model quotes a row that does not exist), and the CONFIRM-gated drafts in `ticket_folder.rs`. No code change. Worth restating in the README's intro. |
| 3 | Five-file ticket folder is the right output unit | **ACCEPT (already shipped)** | `INTAKE.md`, `EVIDENCE_PREFLIGHT.md`, `FORK_PACKET.md`, `DRAFTS.md`, `STATE.md` are the contract of `ticket_folder::write_ticket_folder`. No change. |
| 4 | Port back to Python for prototypes | **REJECT** | Regression. The Rust port is shipped, the inline-test pattern (`#[cfg(test)]` in the same source file) already gives sub-second prototype loops, and a Python/Rust dual-language repo would double maintenance load for zero new capability. The career-narrative benefit ("I learned Python") does not require this project to be the vehicle — small standalone Python labs outside the repo do that better. |
| 5 | Pause `watch`, inbox/TUI polish, additional LLM providers, autonomous posting, large UI refinement | **PARTIAL** | Reject the "pause `watch`/inbox" half — both are shipped, used, and load-bearing in the spec (`watch` runs the same `investigate_one_structured` pipeline per ticket; the inbox is the operator's reading surface). Autonomous posting was never on the roadmap; restating the no-write boundary in the README is enough. The "additional LLM providers" half is **accepted** as the direction for this PR — see #15. |
| 6 | Narrow MVP: `triage-cli investigate <id> --file ... --paste ...` → five-file folder | **ACCEPT (already implemented)** | This is exactly `cli::cmd_investigate`. No change. |
| 7 | Demo / fixture mode: `triage-cli demo audio-drop`, `triage-cli investigate --fixture …` | **ACCEPT (deferred)** | High value but a meaningful design lift: needs a fixture schema, a stub Zendesk/Datadog client toggled on at the trait boundary, and a separate `cmd_demo` (or a `--fixture` flag on `investigate`). Tracked in `docs/ROADMAP.md`. Not in this PR. |
| 8 | Golden output tests on fixtures | **ACCEPT (deferred)** | Blocked on #7 — golden tests are how you verify fixture mode. Sequencing them together prevents writing a test harness that gets thrown away. Tracked in `docs/ROADMAP.md`. |
| 9 | Tighten the AI boundary: every output separates observed / inferred / missing / next-action / confirm | **ACCEPT (already shipped)** | This is the layout of `EVIDENCE_PREFLIGHT.md` (decisive vs. missing) and `FORK_PACKET.md` (recommendation + decision signal + missing evidence + handoff checklist). The `<!-- CONFIRM -->` markers on `DRAFTS.md` are the human-review gate. No change. |
| 10 | Make evidence first-class with IDs (`E-003`, `E-007`, …) cited in `FORK_PACKET.md` | **ACCEPT (deferred)** | A real product feature. Touches `models.rs` (`EvidenceItem.id` field), `pipeline.rs` (ID assignment is deterministic and stable across re-runs), the LLM prompt (the model has to cite IDs, not paraphrase), the validator (warn when a citation does not match an `E-N` ID), and the `EVIDENCE_PREFLIGHT.md` / `FORK_PACKET.md` writers. Wants its own ADR + spec amendment. Tracked in `docs/ROADMAP.md` as the next product PR. |
| 11 | CI workflow (`fmt --check`, `clippy -D warnings`, `cargo test`) | **ACCEPT (this PR)** | Phase 0 stabilization. The repo currently has only `.github/workflows/opencode.yml` (a comment-triggered AI workflow); no CI runs on push or PR today. Adding the workflow is pure config — zero design cost — and immediately catches regressions. |
| 12 | Justfile or Makefile | **ACCEPT (this PR)** | The "Common commands" block in `CLAUDE.md` is documentation that will drift the moment someone forgets to update it. A `justfile` is the executable form of the same list. Phase 0, no design cost. |
| 13 | `--metrics-out run-metrics.json` for phase timing, evidence count, LLM latency | **ACCEPT (deferred)** | A small but real change: needs a `RunMetrics` record, instrumentation at each `reporter.phase_*` call site, and a JSON writer at the end of the pipeline. Worth doing, but cleaner once the `Reporter` trait grows a structured `record_metric(...)` method rather than retrofitting timing onto today's signature. Tracked in `docs/ROADMAP.md`. |
| 14 | `--no-llm` baseline that still emits the five files | **ACCEPT (already shipped)** | `cli::TriageCmd` and `cli::InvestigateCmd` already have `--no-llm`; `pipeline::investigate_one_structured` dispatches to `stub_assess_structured` when set. What is missing is a runbook entry that names this flow as a supported baseline; that lands in this PR's doc sweep. |
| 15 | Prune to a tight set of LLM providers | **ACCEPT (this PR)** | Goal directive: keep `unleash` (HTTP, default) and `codex` (subprocess); remove `claude` (subprocess) and `openai` (HTTP). Rationale documented in `docs/adr/0002-prune-claude-openai-providers.md`. The Claude provider was already a subprocess shim — losing it removes 130+ lines and one CLI dependency. OpenAI was kept "for completeness"; with `unleash` as the production path and `codex` as the dev escape hatch, the third HTTP provider was carry-over weight. |
| 16 | ADR for each structural decision | **ACCEPT (this PR)** | The repo already has `docs/adr/0001-pr9-split-and-pruning.md`. This PR adds `0002-prune-claude-openai-providers.md`. |
| 17 | Career positioning as "reliability-minded operations engineer building internal tools / AI-native workflows" | **NOT ACTIONABLE (code-wise)** | Resume / portfolio framing; no repo change. |

## What this PR ships

1. Remove `claude` and `openai` providers (code + tests).
2. CI workflow (`.github/workflows/ci.yml`) + `justfile`.
3. Documentation sweep: README, `CLAUDE.md`, `AGENTS.md`, `.env.example`, `docs/CHEATSHEET.md`, `docs/runbooks/05-switching-models.md`, `triage-cli-rs/REGRESSIONS.md`.
4. `docs/adr/0002-prune-claude-openai-providers.md`.
5. `docs/ROADMAP.md` — deferred items (fixtures, golden tests, evidence v2, metrics-out).
6. This document.
7. Notion-wiki mirror of the above so the operator-facing surface stays in sync.

## What this PR explicitly does NOT ship

| Item | Reason |
| --- | --- |
| Fixture / demo mode | Bigger lift; deserves its own PR with a schema. |
| Golden output tests | Blocked on fixture mode. |
| Evidence-ID model v2 | Product change; needs spec amendment. |
| `--metrics-out` flag | Wants a `Reporter::record_metric` extension; not a single-flag drop-in. |
| Move to Python prototypes | Rejected outright — Rust port stays. |

## Why now

The repo is at a healthy decision point: the Rust port is shipped (`e28cc94`), the five-file ticket folder is the contract, and the next phase is *not feature expansion*. Phase 0 stabilization (CI, lint, fmt, test on every PR) is the cheapest move that protects everything already built. Pruning the provider surface in the same PR is on-theme — it is the same "reduce variability before adding more" principle.
