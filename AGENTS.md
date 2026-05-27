# AGENTS.md

This file gives Codex and AGENTS.md-compatible agents the working doctrine for this repo.

> **Kept in sync with `CLAUDE.md`.** Edit both together. The content intentionally stays short; narrow, evolving details belong in `.codex/skills/*/SKILL.md`.

## Doctrine

`triage-cli` is an evidence router for Carbyne APEX NG911/E911 support. Its job is to help a NOC analyst decide what to do with a Zendesk ticket, produce reviewable artifacts, and preserve the chain of evidence. It is **not** an autoposter, a hidden workflow engine, or a second source of truth.

Work by these principles:

1. **Operator agency first.** The CLI can draft, summarize, route, and explain. It must not post to Zendesk, create Jira issues, or mutate audited external systems without an explicit human action.
2. **Structured pipeline over parallel paths.** `pipeline::investigate_one_structured` is the main contract for `investigate`, `triage`, and `watch`. Do not reintroduce a prose-only or side-channel output path.
3. **Evidence beats confidence.** Every routing change should be traceable to ticket facts, fixture data, Datadog lines, memory hits, or explicit analyst-provided evidence.
4. **Local and reversible by default.** Ticket folders, drafts, session logs, and memory are local artifacts. Writes should be atomic, soft-lock aware, and easy for the analyst to inspect.
5. **Small context, sharp skills.** Load only the skills needed for the task. If a task appears to need more than 4-5 skills, split the task or tighten the plan instead of loading more context.

## First read on any task

- `README.md` for the user-facing surface.
- `docs/spec/v1-reframe.md` when behavior is ambiguous; the spec wins.
- `triage-cli-rs/Cargo.toml` for current crate metadata and MSRV.
- The specific skill(s) below that match the work. Do not read every skill “just in case.”

## Skill routing

Skills live under `.codex/skills/<name>/SKILL.md`. They are compact, domain-specific context packs and should evolve with the code. When you change behavior covered by a skill, update that skill in the same PR with the new invariant, command, or gotcha.

| Work area | Load this skill | Typical files |
| --- | --- | --- |
| Structured triage, fork packets, evidence IDs, memory, soft locks | `.codex/skills/pipeline-contract/SKILL.md` | `src/pipeline/**`, `src/models.rs`, `src/ticket_folder.rs`, `playbook/fork-rubric.md` |
| Codex provider, app-server, setup/doctor auth probes, transport fallback | `.codex/skills/codex-transport/SKILL.md` | `src/providers/**`, `src/llm.rs`, `src/setup.rs`, `docs/adr/0004-*` |
| Inbox TUI, chat turns, session manifests, attachments, progress banner | `.codex/skills/tui-chat/SKILL.md` | `src/tui/**`, `src/chat.rs`, `src/pipeline/followup*` |
| Fixtures, integration tests, golden/operator-visible evidence | `.codex/skills/fixture-evidence-tests/SKILL.md` | `fixtures/**`, `tests/**`, `src/fixture.rs`, metrics/reporting paths |

Do not create a new skill because a task is merely “important.” Add a skill only when the domain has stable invariants that would otherwise bloat this file.

## Hard contracts

- Five-file ticket folder: `INTAKE.md`, `EVIDENCE_PREFLIGHT.md`, `FORK_PACKET.md`, `DRAFTS.md`, `STATE.md`.
- `DRAFTS.md` content is CONFIRM-gated; generated drafts are never posted automatically.
- `triage` and `investigate` print `FORK_PACKET.md` to stdout. Most status, warnings, paths, progress, and validation chatter go to stderr.
- Tests must not hit live Zendesk, Datadog, or providers unless explicitly gated (`SANDBOX_INTEGRATION=1` or `CODEX_AVAILABLE=1`).
- Keep public APIs typed. Use `thiserror` at module boundaries and `anyhow` only in binary glue.
- Preserve PII redaction at provider boundaries. Operational identifiers such as ticket IDs, CNC/site names, station codes, and call IDs are generally useful evidence and should not be blindly stripped.

## Testing and proof

Prefer tests that show what changed for the analyst, not just that a function returned `Ok(())`.

For user-visible or operator-visible changes, include evidence in the assertion message or snapshot shape:

- **Before / trigger:** the fixture input, flag, prior state, or regression being exercised.
- **Now / surface:** what the analyst sees, receives, or can do now: stdout, one of the five markdown files, metrics JSON, base-evidence manifest, session log, or TUI-rendered state.
- **Proof:** exact strings, counts, IDs, fork letter, rubric row, file path, schema version, or manifest body that proves the change.

Default offline gate:

```bash
cd triage-cli-rs
cargo test --lib
cargo test --test integration
cargo test --test pipeline_integration
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

Use live gates only when the task is specifically about that integration.

## Subagent-driven development

Use `.codex/subagents.yaml` as the local worker contract. For non-trivial changes, run the plan through the relevant reviewers before coding:

- `plan_reviewer` for scope, missing tests, rollout risk, and whether the task is split correctly.
- `security_reviewer` for credentials, PII, network calls, file writes, shelling out, and audit boundaries.
- `architecture_reviewer` for pipeline ownership, duplication, module seams, and ADR needs.
- `product_evidence_reviewer` for “can the operator actually see the change?” proof.
- `duplication_reviewer` when adding new modules, prompts, renderers, or provider paths.

The reviewers should block cleverness that erodes the doctrine above. If a reviewer finds a gap, fix the plan before widening implementation.

## Project map

- Crate: `triage-cli-rs/`
- Entry point: `src/main.rs` → `triage_cli::run()` → `cli::run()`
- Structured pipeline: `src/pipeline/**`
- LLM dispatch and validation: `src/llm.rs`, `src/providers/**`
- Ticket folder writer: `src/ticket_folder.rs`
- Fixtures: `triage-cli-rs/fixtures/`
- Offline integration tests: `triage-cli-rs/tests/integration/**`
- Architecture decisions: `docs/adr/`
- Plans/specs: `docs/superpowers/`
