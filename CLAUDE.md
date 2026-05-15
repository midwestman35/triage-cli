# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

> **Kept in sync with `AGENTS.md`.** Edit both together — they have the same content and exist only because Claude Code and Codex / other agents each look for a different filename.

## What this is

A **Rust 1.95+** CLI that triages Zendesk tickets for the Carbyne APEX NG911/E911 platform. The crate lives in `triage-cli-rs/`. Seven subcommands:

- `triage-cli investigate <id-or-url>` — interactive guided session. Fetches the ticket, lets the analyst drop in attachments and pasted evidence, then runs the structured pipeline. Writes a five-markdown ticket folder under `${TRIAGE_TICKETS_ROOT:-./Tickets}/<id>/`. Requires a TTY.
- `triage-cli triage <id-or-url>` — headless single-shot pipeline (no evidence prompts). Same ticket-folder output; also prints `FORK_PACKET.md` to stdout (pipeable handoff).
- `triage-cli inbox [--view ...]` — ratatui TUI over the produced ticket folders. Synth-summary right pane plus tabbed per-file view. Requires TTY.
- `triage-cli watch --view <id>` — long-running headless poll loop; reuses the structured pipeline per ticket.
- `triage-cli doctor` — health-check env vars, credentials, output-dir writability; exits 0/1.
- `triage-cli build-map` — regenerates `data/cnc-map.json` from `apex-cnc-inventory.md`.
- `triage-cli setup` — interactive first-run; prompts for env vars and writes `.env`. Idempotent.

The reference for the v1 surface is the spec at **`docs/spec/v1-reframe.md`** — when behavior is ambiguous, that file wins. `README.md` is the user-facing reference. The frozen Python source lives in `archive/python-source-2026-05-12.zip` — do not edit it, do not port new features back to Python.

## LLM providers

Controlled by `LLM_PROVIDER` env var (default: `unleash`).

| Value | Mechanism | Required |
|---|---|---|
| `unleash` (default) | HTTP to internal Axon gateway | `UNLEASH_API_KEY`, `UNLEASH_ASSISTANT_ID` |
| `codex` | Subprocess to `codex exec`; inherits Codex OAuth | `codex` CLI on PATH |

The `claude` and `openai` providers were removed 2026-05-14 in favor of unleash + codex (see `docs/adr/0002-prune-claude-openai-providers.md`); if a future Anthropic API path is added, it must work with the enterprise OAuth seat the operator has — i.e. via the `claude` CLI subprocess, not the SDK. Codex reads `CODEX_MODEL` (default `gpt-5-codex`); Unleash ignores any model parameter — the model is selected server-side by `UNLEASH_ASSISTANT_ID`.

## Memory layer

After every investigation, `MEMORY.md` and `data/memory.db` (SQLite, FTS5-indexed) are updated with the ticket ID, customer, subject, symptom, assessment, resolution, **fork_letter**, **quoted_rubric_row**, and **rubric_version**. Before the LLM call, the top-3 similar prior investigations are retrieved via BM25 and injected as context.

### v1 schema (schema_version = 2)

The FTS5 `investigations` virtual table carries nine columns: the original six (`ticket_id, customer, subject, symptom, assessment, resolution`) plus `fork_letter, quoted_rubric_row, rubric_version` appended during the v1 reframe. FTS5 does not support `ALTER TABLE ADD COLUMN`, so the upgrade path is a one-shot rebuild inside `memory::ensure_db`: rows are spilled into a temp table, the FTS5 table is dropped and re-created with the v1 column list, then rows are re-inserted with empty strings for the new fields. The migration is wrapped in a single transaction (crash-safe — interrupted runs roll back to the legacy table) and is idempotent via `memory_meta.schema_version`.

MEMORY.md blocks gained `fork_letter`, `quoted_rubric_row`, and `rubric_version` keys. Legacy blocks lacking these keys still parse cleanly — missing keys default to the empty string.

To prune: edit `MEMORY.md` and delete entries. The FTS index rebuilds on the next investigation when `MEMORY.md` mtime > `last_indexed_at`.

## Common commands

```bash
# Build (release)
cd triage-cli-rs && cargo build --release
# Produces a ~7 MB static binary at target/release/triage-cli.

# Optional: symlink onto PATH
ln -s "$PWD/target/release/triage-cli" ~/.local/bin/triage-cli

# Run from the parent dir (the one with data/, MEMORY.md, apex-cnc-inventory.md, .env)
cd ..
triage-cli doctor

# Tests (inline #[cfg(test)] modules; no network calls)
cargo test

# Run one module's tests
cargo test --lib redact::

# Lint + format
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check

# Rebuild the site map after editing apex-cnc-inventory.md
triage-cli build-map
```

Tests are inline (`#[cfg(test)]`) in the same `.rs` source files. Mocked clients; no live Zendesk / Datadog / provider calls. Don't add network-touching tests.

## Architecture

### Crate layout

`triage-cli-rs/` follows one-module-per-file. The binary entry point is `src/main.rs` → `triage_cli::run()` (in `src/lib.rs`) → `cli::run()`. Current modules under `src/`:

`build_map`, `cli`, `datadog`, `extract`, `interactive`, `investigation`, `llm`, `memory`, `models`, `pipeline`, `playbook`, `providers/` (`mod`, `unleash`, `codex`), `redact`, `setup`, `ticket_folder`, `tui/` (`mod`, `inbox`), `watcher`, `zendesk`.

The legacy `render` module and `tui/investigate.rs` were removed in the v1 reframe — there is no longer a prose-note renderer or a mid-investigation TUI.

### Pipeline ownership

`pipeline::investigate_one_structured` is the **only** end-to-end pipeline entry point in v1. It is shared by `cli::cmd_investigate`, `cli::cmd_triage`, and `watcher::run_iteration`. It drives customer-history fetch → memory lookup → site resolution → optional Datadog enrichment → PII redaction → structured LLM call → ticket-folder write. **No I/O outside the injected clients.** Do not reintroduce a parallel prose-output pipeline; the structured path is the contract.

Pipeline errors flow through the `pipeline::PipelineError` enum (`Zendesk`, `Datadog`, `Llm`, `Extract`, `Memory`, `TicketFolder`). Add new variants there rather than reaching for `anyhow::Error` when callers might match on the kind.

### Reporter trait

The `Reporter` trait (`pipeline::Reporter`) decouples progress output from pipeline logic. Three implementations:

- `StderrReporter` — default for `triage` and `investigate`. Emits phase lines to stderr (verbose-gated for `phase_started`, always for `phase_done` / `phase_failed`).
- `SilentReporter` — used by tests and the watcher.
- `ChannelReporter` — emits `TuiEvent`s into a tokio channel; used by the inbox TUI.

There is no terminal `Done` event: the caller of `investigate_one_structured` receives the `StructuredInvestigation` value (report + paths + validator warnings) synchronously. The inbox refreshes its row by re-reading `STATE.md` after the call returns.

### Five-markdown ticket folder

`ticket_folder.rs` is the canonical output writer (spec § 4). Given a `StructuredTriageReport`, it writes five files atomically (tempfile + rename) under `${TRIAGE_TICKETS_ROOT:-./Tickets}/<id>/`:

- **`INTAKE.md`** — engine-known facts: housekeeping checklist, ticket facts (ID, URL, status, priority, tags, requester, org, site/CNC, region, affected stations/agents, call ID, incident window, reported symptom), one-line fingerprint, LLM-emitted 3–6 bullet summary, context-pulls table, initial fork hypothesis with justification, intake decision (checklist of: ready / known issue / needs clarification / cannot proceed).
- **`EVIDENCE_PREFLIGHT.md`** — gathered-evidence table (type / source / time window / summary), decisive-evidence bullets, missing / non-decisive bullets.
- **`FORK_PACKET.md`** — committed routing decision: recommendation (fork letter, confidence, reasoning), decision signal (rubric class + quoted rubric row), evidence summary, missing evidence (restated), related work (Zendesk siblings, Jira, master ticket, cluster), handoff checklist.
- **`DRAFTS.md`** — CONFIRM-gated drafts. Customer-facing reply, internal Zendesk note, and (fork A only) a Jira draft. Each draft begins with `<!-- CONFIRM -->`. The CLI never posts these autonomously.
- **`STATE.md`** — YAML frontmatter only (no prose): `ticket_id`, `fork`, `confidence`, `quoted_rubric_row`, `rubric_version`, `owner`, `created_at`, `updated_at`, `status`, `related` (zendesk/jira/master), `cluster`, `validator_warnings`.

Fork letters are: **A** Engineering Jira · **B** Vendor or Internal IT · **C** NOC self-resolve · **D** Cannot fork yet (rubric demands more evidence; next step is gathering it).

Partial writes are impossible: the soft-lock pre-check runs before any file is touched, and each render-then-rename is atomic per file.

### Fork rubric (playbook module)

The rubric lives at `triage-cli-rs/playbook/fork-rubric.md` and is **embedded into the binary at build time** via `include_str!` in `playbook.rs`. `Rubric::load()` returns the embedded copy unless `TRIAGE_RUBRIC_PATH` points at a disk file — that override is dev-mode only; release builds should rely on the embedded copy. `Rubric::version()` returns the parsed `rubric_version: ...` header.

`Rubric::contains_row(quoted)` is a **soft-warn** validator (spec § 10, decision 1): it returns whether `quoted` is a verbatim substring of the rubric text. The validator logs to stderr and stashes the warning in `STATE.md`'s `validator_warnings: [...]` field, but it does **not** reject the LLM's response. Tightening to strict matching is a backward-compatible change later.

### LLM access — structured output, provider trait

`llm.rs` dispatches to a provider via the `LlmProvider` trait in `providers/mod.rs` (uses native `async fn` in traits, no `async-trait` crate). Implementations: `unleash`, `codex`. Two single-turn async calls:

- `triage_structured(bundle, rubric, ...)` — main path. Asks the provider for a single JSON object deserializable into `StructuredTriageReport`, validates `ForkCommitment` against the rubric (soft-warn), and returns the report plus any validator warnings.
- `extract_anchor(ticket)` — best-effort timestamp extraction; returns `None` on any failure (invalid JSON, missing key, unparseable timestamp). Only transport errors propagate.

**Parse / validation retry semantics (spec § 6, decision 6):** on a JSON parse or shape-validation failure, the pipeline retries the LLM call once with a corrective system note. A second failure raises `LlmError::StructuredAfterRetry { raw_response, .. }`; the pipeline catches that variant and stashes the raw response under `${TRIAGE_TICKETS_ROOT}/<id>/.debug/llm-response-<timestamp>.json` (via `ticket_folder::stash_debug_response`) before propagating the error. No ticket folder is written on this failure path.

### STATE.md soft-lock semantics

`STATE.md` carries an `owner` field; the writer reads it before any of the five files are touched. Owner resolution falls through `TRIAGE_OWNER` → `$USER` → `"unknown"` (see `pipeline::current_owner`).

If an existing `STATE.md` claims a different owner and `--force` was not passed, `ticket_folder::write_ticket_folder` returns `TicketFolderError::SoftLockConflict` with: existing owner, current owner, a per-field summary diff (fork / confidence / status / owner / quoted_rubric_row / rubric_version), the path to the existing `STATE.md`, and the rendered new `STATE.md` content. **Nothing is written** — the existing folder is preserved byte-for-byte.

The CLI translates that error to a non-zero exit (code **2**, distinct from generic failures which exit 1), prints the summarized field diff to stderr, and — if `--diff` was passed — opens the full STATE.md diff in `$DIFF_VIEWER` (fallback `diff -u`). Same owner, no existing STATE.md, or `--force == true` all proceed.

### PII redaction at the LLM boundary

`redact.rs` is the redactor applied to the bundle before it reaches the provider. Scope is locked to **caller PII**: phones, addresses, GPS coords. Names are an explicit gap (regex unreliable). **Operational identifiers** (Call-IDs, ticket numbers, station codes, CNC UUIDs, site names) are preserved — they are what makes the LLM output useful for handoff.

Internal Zendesk comments **are** sent to the LLM, after passing through `redact`. v1 is terminal-only / local-files-only so residual exposure stays local; if any flow ever posts model output back to Zendesk or an audited system, the redaction scope must be re-evaluated.

### Interactive workspace and doctor (legacy `triage-notes/` scratch path)

The interactive `investigate` flow uses `./triage-notes/<ticket-id>/` as a **scratch workspace** for downloaded attachments and dropped local files (`cli.rs:cmd_investigate` calls `interactive::ensure_workspace(Path::new("./triage-notes"), ...)`). `doctor` also probes that path for writability. These are leftover code references from the legacy single-file output era; the ticket-folder output itself goes to `Tickets/<id>/`, not here. Treat `./triage-notes/` as an analyst scratch dir, not a deliverable surface.

### Site map flow

`apex-cnc-inventory.md` (committed markdown tables) → `build_map` (invoked via `triage-cli build-map`) → `data/cnc-map.json` + `data/cnc-map-gaps.md`. Conversion rules: per-site table is canonical, master table fills gaps, blank-CNC rows go to gaps file, dedupe by CNC UUID, all retained entries must have a non-null `site_name`. Preserve these when editing `build_map.rs`.

There is **no `confluence.rs`** by design. Refreshing the inventory from Confluence is a manual, out-of-band step. Do not add a Confluence module.

`extract::lookup_site` resolution priority: `--site` flag → `--cnc` flag → exact `friendly_name` match (case-insensitive) against `requester_org` → longest `site_name` substring in subject+description → longest `friendly_name` substring. The strategy name is returned for `--verbose` output. When the deterministic chain returns `no_match`, `pipeline::resolve_site` falls back to an LLM-driven `extract_site` call.

### Anchor resolution

`extract::resolve_anchor` priority: `--at` flag (`AnchorSource::Flag`) → LLM-extracted (`AnchorSource::Extracted`) → `ticket.created_at` (`AnchorSource::CreatedAt`). All datetimes are normalized to timezone-aware UTC inside the pipeline; naive inputs are treated as UTC. Don't silently drop tzinfo when adding date logic.

### Datadog query

Site-level only: `<DD_CALL_CENTER_TAG>:<site_name> status:(<levels>)`. Window is `anchor ± window_minutes`, capped at 200 lines (`log_truncated = true` when at the cap). `site_name` is regex-validated (`^[a-zA-Z0-9._-]+$`) before query interpolation — do not loosen this. `DD_STATION_TAG` is read by no code today but is reserved for station-level filtering; leave it in `.env.example`.

### Watcher state

`data/watcher-state-<view-key>.json` has shape `{"version": 1, "triaged": {"<ticket-id>": "<iso updated_at>"}}`. `watcher::should_triage` is the pure decider (re-triage when `updated_at` advances; first-run silent backfill marks pre-cutoff tickets as seen with no note). State writes are atomic (tempfile + rename) and pruned to 1000 entries at the end of each iteration. The same state file is shared between `watch` and `inbox` for a given view.

### Inbox TUI

`tui/inbox.rs` is a ratatui app over the produced ticket-folder corpus. It scans `tickets_root()` for subdirectories that contain a `STATE.md` — each such directory is one inbox row. `cli::cmd_inbox` requires a TTY.

- **Default detail view** is a synth single-pane summary parsed out of `STATE.md`: fork letter, confidence, quoted rubric row, status, owner, related tickets.
- **`Tab` / `Shift+Tab`** cycle into / through a per-file tabbed view across `INTAKE.md`, `EVIDENCE_PREFLIGHT.md`, `FORK_PACKET.md`, `DRAFTS.md`, `STATE.md`. `Esc` returns to the synth view.
- **Rubric-mismatch banner**: when the selected ticket's `STATE.md` carries a `rubric_version` that does not match the shipped rubric, a non-blocking yellow banner is shown above the content (spec § 10, decision 2).
- **Keybindings**: `↑/k`, `↓/j` navigate · `Enter` triages a queued row (and focuses the detail pane otherwise) · `Tab` / `Shift+Tab` cycle file tabs · `Esc` back to synth view / list · `r` refresh · `y` copy the synth summary to clipboard · `o` open the ticket in Zendesk in the browser · `q` / `Ctrl-C` quit.

Logging during the TUI run is redirected to a per-view file printed at startup so the TUI itself stays clean.

### Stdout vs stderr discipline

- **`triage`** prints `FORK_PACKET.md` (the file content) to stdout — the pipeable handoff surface. The other four ticket files are folder-only.
- **`investigate`** writes the folder and prints `FORK_PACKET.md` to stdout the same way; status text ("Ticket folder ready: ...") goes to stderr.
- **`watch --print-notes`** streams each ticket's `FORK_PACKET.md` content to stdout between `---` separators; without `--print-notes`, the watcher emits only stderr status lines.
- **`build-map`** prints its summary (entry count, gaps file path) to stdout — the conventional shape for a one-shot build tool.
- All other status: verbose phase traces, spinners, watcher status lines, save-path notices, validator soft-warnings, soft-lock summaries, inbox log path — go to **stderr** via `eprintln!` or the configured logger. Don't move status output to stdout.
- The TUI is the exception (it owns the terminal); its diagnostic logging is redirected to a file.

## Conventions worth knowing

- All data models in `models.rs` derive `Serialize` and `Deserialize` from `serde`. Use `#[serde(tag = "...", rename_all = "snake_case")]` for sum types so the wire format stays predictable.
- Library code returns `Result`. Use `thiserror` for typed errors at module boundaries (`PipelineError`, `ProviderError`, `TicketFolderError`, `PlaybookError`, `MemoryError`); reach for `anyhow::Result` only in the binary glue (`cli.rs`, `main.rs`).
- Stdout writers are limited to three places: `cli::print_fork_packet_to_stdout` (via `print!`), `watcher::run_iteration` when `--print-notes` is set, and `build_map`'s summary lines. Library modules use `eprintln!` or return; do not introduce more stdout writers.
- Module size guidance: aim for ~250 lines per `.rs`; when one grows past that, the split usually wants a sibling file, not a sub-module. `pipeline.rs`, `cli.rs`, `ticket_folder.rs`, and `tui/inbox.rs` are deliberate exceptions.
- TUI deps (`ratatui`, `crossterm`) are runtime-required; don't feature-gate them.
- Edition `2021`, MSRV `1.95`. Lint with `cargo clippy --all-targets -- -D warnings`; format with `cargo fmt --all`.
- Architecture decisions are recorded in `docs/adr/`. Look there before proposing structural changes; if you make one, add an ADR.

## Environment variables (v1-specific)

| Variable | Purpose |
| --- | --- |
| `TRIAGE_TICKETS_ROOT` | Where ticket folders are written. Default `./Tickets`. Set to a Drive-synced path on operator laptops. |
| `TRIAGE_RUBRIC_PATH` | Dev override: load the fork rubric from this file instead of the embedded copy. Release builds should leave this unset. |
| `TRIAGE_OWNER` | Identifier recorded in `STATE.md`'s `owner` field; overrides `$USER`. |
| `TRIAGE_MEMORY_MD` / `TRIAGE_MEMORY_DB` | Test-only overrides for the MEMORY.md / SQLite paths. |
| `DIFF_VIEWER` | Command run on `--diff` soft-lock conflicts (e.g. `code --diff`). Falls back to `diff -u`. |
| `LLM_PROVIDER` and provider creds | See the *LLM providers* table above. |

## Where things live

| What | Where |
| --- | --- |
| Triage system prompt | `triage-cli-rs/src/llm.rs` |
| Anchor extraction prompt | `triage-cli-rs/src/llm.rs` |
| Structured pipeline | `triage-cli-rs/src/pipeline.rs` (`investigate_one_structured`) |
| Reporter trait + implementations | `triage-cli-rs/src/pipeline.rs` (`Reporter`, `StderrReporter`, `SilentReporter`, `ChannelReporter`) |
| Five-markdown ticket folder writer | `triage-cli-rs/src/ticket_folder.rs` (`write_ticket_folder`, `stash_debug_response`, `tickets_root`) |
| Fork rubric loader (embedded + `TRIAGE_RUBRIC_PATH` override) | `triage-cli-rs/src/playbook.rs` |
| Embedded rubric file | `triage-cli-rs/playbook/fork-rubric.md` |
| Investigation session + evidence | `triage-cli-rs/src/investigation.rs` |
| Memory layer (MEMORY.md + SQLite FTS5, schema v2) | `triage-cli-rs/src/memory.rs`, `MEMORY.md`, `data/memory.db` |
| LLM provider trait + impls | `triage-cli-rs/src/providers/` (`mod.rs`, `unleash.rs`, `codex.rs`) |
| LLM structured-output dispatch + validator + retry | `triage-cli-rs/src/llm.rs` (`triage_structured`) |
| PII redaction | `triage-cli-rs/src/redact.rs` |
| Inbox TUI | `triage-cli-rs/src/tui/inbox.rs` |
| Site lookup logic | `triage-cli-rs/src/extract.rs` (`lookup_site`) |
| Anchor priority | `triage-cli-rs/src/extract.rs` (`resolve_anchor`) |
| Datadog query construction | `triage-cli-rs/src/datadog.rs` |
| Watcher loop + state | `triage-cli-rs/src/watcher.rs` |
| Doctor + setup commands | `triage-cli-rs/src/setup.rs` |
| Site map builder | `triage-cli-rs/src/build_map.rs` |
| Interactive workspace (`./triage-notes/<id>/` scratch dir) | `triage-cli-rs/src/interactive.rs` |
| CLI surface (clap-derive) | `triage-cli-rs/src/cli.rs` |
| v1 reframe spec | `docs/spec/v1-reframe.md` |
| Frozen Python source (reference only) | `archive/python-source-2026-05-12.zip` |
| Architecture decisions | `docs/adr/` |
| Operator runbooks | `docs/runbooks/` |
| Quick reference | `docs/CHEATSHEET.md` |
| Known regressions / open issues | `triage-cli-rs/REGRESSIONS.md` |
