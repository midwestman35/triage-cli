---
name: pipeline-contract
description: Use when changing the structured triage pipeline, fork packets, evidence IDs, memory, ticket folders, or soft-lock behavior.
---

# Pipeline contract skill

Load this skill when a task touches the structured investigation path or any artifact the NOC analyst reviews.

## Core doctrine

The structured pipeline is the product contract. Keep `pipeline::investigate_one_structured` as the single route for `investigate`, `triage`, and `watch`; do not add a parallel prose renderer or bypass path.

## Stable invariants

- Five markdown files are the operator-facing deliverable: `INTAKE.md`, `EVIDENCE_PREFLIGHT.md`, `FORK_PACKET.md`, `DRAFTS.md`, `STATE.md`.
- `DRAFTS.md` is review-only and CONFIRM-gated. The CLI must not autonomously post to Zendesk, Jira, or another audited surface.
- `STATE.md` soft-lock checks happen before any file write. A conflict must preserve the existing folder byte-for-byte unless `--force` is explicit.
- Evidence IDs are deterministic `E-NNN` values assigned once from `TriageBundle::evidence_index` before the LLM call.
- Provider-bound bundles must pass through redaction. Preserve operational IDs that make routing explainable: ticket IDs, Call IDs, CNC/site names, station codes.
- Pipeline phases should use injected clients and typed errors. Avoid hidden network/file access inside business logic.

## Common files

- `triage-cli-rs/src/pipeline/**`
- `triage-cli-rs/src/models.rs`
- `triage-cli-rs/src/ticket_folder.rs`
- `triage-cli-rs/src/llm.rs`
- `triage-cli-rs/playbook/fork-rubric.md`
- `docs/spec/v1-reframe.md`

## Test evidence to require

For each behavior change, show what the operator sees now:

- exact markdown text in one of the five files;
- exact `STATE.md` fields for fork/confidence/rubric/owner/status;
- exact evidence ID, source path, body snapshot, or manifest schema;
- exact stdout/stderr behavior for CLI changes;
- exact soft-lock diff or non-write guarantee for ownership changes.

Default gates:

```bash
cd triage-cli-rs
cargo test --lib
cargo test --test integration
cargo test --test pipeline_integration
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

## Keep this skill current

Update this file when adding/removing a pipeline phase, changing artifact shape, changing evidence assignment, changing soft-lock semantics, or moving the canonical pipeline entry point.
