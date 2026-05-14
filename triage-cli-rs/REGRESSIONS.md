# Rust Port Regressions

Tracks behavioral / visual deltas from the canonical Python `triage_cli` package.
Goal: keep this list short and explicit. Anything not listed here is asserted to
match Python behavior.

Status keys:
- **OPEN** — known divergence, no plan yet
- **WAIVED** — accepted divergence (with reason)
- **CLOSED** — fixed in the port, kept for history

---

## OPEN

### R1 — `claude` provider is a subprocess, not an in-process SDK call *(CLOSED)*
**Status:** CLOSED. Provider removed 2026-05-14 in favor of unleash+codex. See `docs/adr/0002-prune-claude-openai-providers.md`. Regression no longer applies.

**Python (historical):** `triage_cli/providers/claude.py` used the `claude_agent_sdk` Python
library, which streamed `AssistantMessage`/`TextBlock` blocks in-process. It
inherited Claude Code's OAuth session.

**Rust (historical):** No Claude Agent SDK existed for Rust. The port shelled out to the
`claude` CLI via `claude -p <prompt>` (subprocess, prompt-on-stdin). It still
inherited Claude Code OAuth, but the stream was replaced by a single buffered
stdout read.

### R2 — `codex` provider is new; no Python equivalent
**Python (historical):** Three providers (`unleash`, `claude`, `openai`).

**Rust:** Two providers: `unleash` (HTTP) and `codex` (subprocess). The `claude` and `openai` providers were removed 2026-05-14 (ADR-0002). Codex is invoked via
`codex exec <prompt>`; selected via `LLM_PROVIDER=codex`. Default model `gpt-5-codex` (env `CODEX_MODEL`).

**Impact:** Net-new functionality on the Rust side, not a regression. Documented for awareness.

### R3 — TUI uses ratatui+crossterm instead of Textual *(now feature-parity)*
**Python:** `triage_cli/inbox/` and `triage_cli/tui/` use Textual.

**Rust:** `ratatui`+`crossterm`, hand-rolled. Visual style differs but the
feature surface matches the Python implementation:
- Two-pane DataTable + Report layout (`tui/inbox.rs`)
- Status icons / colors per row: ✓ triaged (green), → triaging (yellow bg),
  ○ queued, ✗ failed (red bg)
- 6-column data table: selection icon · ticket# · site · when · confidence · summary
- Sorting: status priority (triaging > triaged > queued > failed), then
  newest report first
- Background polling on the configured interval; reentrancy-guarded
- 24-hour disk hydration of JSON sidecars on startup
- Site-input modal when site lookup fails (rendered as a centered overlay)
- 4-phase progress gauge shown during active triage
- Transient notification overlay (info/success/warning/error)
- Keybindings: ↑/k, ↓/j, enter, esc/ctrl+p, r (refresh), y (copy),
  o (open Zendesk URL), q/ctrl+c
- Cross-platform clipboard: tries `pbcopy` → `wl-copy` → `xclip`
- State file persisted on every poll + on quit

**Impact:** Visual presentation differs (borders, color palette, spinner
glyphs) but the workflow is preserved.

**Status:** R3 effectively CLOSED. Tracking continues only for visual
fidelity if a side-by-side comparison shows a meaningful regression.

### R4 — Datadog client uses the raw HTTP API, not the official SDK
**Python:** `datadog-api-client` Python SDK with its own connection pool,
retry, model serialization.

**Rust:** Direct `reqwest` calls against `https://api.<site>/api/v2/logs/events`.
The Rust port hand-codes the query construction and the response decoder.

**Impact:** Per goal text ("direct http calls to … datadog"). No retries on
transient failures, matching Python's stated behavior. Query string and JSON
response shape are validated against the documented v2 API.

### R5 — `unicode-animations` spinner replaced with `indicatif`
**Python:** `live_spinner("orbit", ...)` from `unicode-animations`. Used during
slow ops when stderr is a TTY.

**Rust:** `indicatif::ProgressBar::new_spinner` with the matching
`UnicodeBrailleSpinner`-style tick set ("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"). Stream is stderr.
"Orbit"-style frames (`◐◓◑◒`) are picked when available; otherwise the default
braille set is used.

**Impact:** Spinner glyph will look slightly different. Behavior (only shown
when stderr is a TTY) is preserved.

### R6 — `rich.Panel` colored output replaced with `owo-colors` + plain ANSI
**Python:** `triage_cli/render.py` uses Rich's `Panel`/`Group`/`Text` to draw
bordered, colored panels for interactive `print_note` output.

**Rust:** When `stdout.is_terminal() && !NO_COLOR` we draw simpler box-drawing
panels with ANSI color via `owo-colors`. When stdout is not a TTY (piping)
we emit pure markdown, identical to Python's non-TTY behavior.

**Impact:** TTY output is visually less polished but readable. Pipe output is
byte-identical.

### R7 — Setup script is a Rust subcommand, not a separate script
**Python:** `python3.11 scripts/setup.py` is the first-run installer (creates
venv, runs pip install, prompts for .env, runs build-map).

**Rust:** `triage-cli setup` replaces it. It does not create a venv (irrelevant
for a static binary), but it does prompt for .env, validate, and run
`build-map`. Resumability matches Python (idempotent on rerun).

### R8 — Memory layer entry-parser tolerances may differ
**Python:** `memory.py::_parse_memory_md` walks `MEMORY.md`, splits on `---`,
and extracts `key: value` lines. Skips blocks that don't have both `id` and
`subject`.

**Rust:** Same logic, but the line scanner is byte-accurate to Python's
`str.partition(":")`. Edge case: if a value itself contains `:` (e.g., a URL),
both implementations keep everything after the first colon as the value.

**Eval target:** None expected; flagged for completeness.

## WAIVED

### W1 — `pyproject.toml` `dev` group not ported
Tests will be Rust `#[test]` functions next to source; no separate lint config
file. `ruff` rules don't translate; `cargo clippy` is the equivalent and runs
out-of-the-box.

### R9 — `cli investigate --save` is now always on
**Python:** `--save/--no-save` toggle; default save=on.

**Rust:** Always saves; the `--save` flag is accepted but ignored (no opposite
form). The previous "stdout-only" mode was rarely used and complicates the
ChannelReporter/TUI flow, which both rely on `save_note` always running so the
markdown/JSON pair is present after the pipeline returns.

**Eval target:** Restore the opposite flag if any caller depends on `--no-save`.

### R10 — `cli build-map` shells out to `python3 scripts/build_cnc_map.py` *(closed)*
**Python:** `build-map` is a subprocess to a sibling Python script that parses
the markdown inventory.

**Rust:** Ported natively in `src/build_map.rs`. The Rust binary no longer
depends on `python3` for any subcommand.

**Verification:** Run against the real `apex-cnc-inventory.md` (77 table rows).
Output `data/cnc-map.json` is **byte-identical** to the Python baseline
(`diff` returns empty). 35 entries written, 30 gaps logged — same numbers as
Python. 5 unit tests cover the parse/dedupe/normalize rules.

**Status:** CLOSED.

### R11 — `scripts/certify_readonly_my_queue.py` not ported
Read-only certification script remains Python-only. It exercises the Zendesk
client path against the authenticated user's assigned queue. The Rust client
mirrors the Python invariants (no PUT/POST/DELETE methods exposed); adding a
Rust equivalent of the cert script is straightforward but out of scope for
the core port.

## CLOSED

### C1 — Cargo `cargo check`, `cargo build --release`, and `cargo test` all pass
Initial port compiled cleanly on the first invocation with three trivial
unused-import / unused-variable warnings, all since fixed. All 15 unit tests
green. Release binary: 7.0 MB, aarch64-apple-darwin.

- `cargo check`: 0 errors, 0 warnings (after fixes)
- `cargo test --lib`: 15 passed, 0 failed
- `cargo build --release`: produced `target/release/triage-cli`
- `./target/release/triage-cli doctor`: matches Python doctor output shape
  (exits 1 when LLM key is unset, prints the same coloured ✓/✗ marks)

## Open questions for the user

Resolved in this session:
- ~~TUI depth: full feature parity with Textual inbox~~ (R3 closed)
- ~~build-map: port to Rust~~ (R10 closed)
- Memory paths: stay cwd-relative (matches Python)
- `--save`: keep always-on, no `--no-save`
- openai provider: removed 2026-05-14 per ADR-0002.

Still open:

1. **Codex subprocess command.** Assumed `codex exec --model <m> <prompt>`.
   What is the actual CLI surface — does it accept a system prompt flag, JSON
   output, take prompts on stdin? Will revisit when the codex CLI shape is
   nailed down (`REGRESSIONS.md` R2).

