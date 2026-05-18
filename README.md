# triage-cli

## What this is

A local Rust CLI for the Carbyne APEX NG911/E911 NOC. Give it a Zendesk
ticket URL or ID and it fetches the ticket body, customer history, attachment
metadata, optionally folds in local files or pasted logs and Datadog
enrichment, and produces a structured **five-markdown ticket folder**:
`INTAKE.md`, `EVIDENCE_PREFLIGHT.md`, `FORK_PACKET.md`, `DRAFTS.md`, and
`STATE.md`. The LLM commits a fork letter (A / B / C / D) against a
versioned rubric; the analyst reviews and acts on the drafts. Nothing is
posted back to Zendesk, Jira, or any audited surface.

This was originally a Python project (Typer + Textual + pydantic). It was
ported to Rust in May 2026 and reframed for the v1 contract; the binary is
now a single static executable with no Python runtime dependency. The frozen
Python source lives in `archive/python-source-2026-05-12.zip` for reference.

The full v1 contract — folder layout, file shapes, soft-lock semantics,
validator behavior — is documented in **`docs/spec/v1-reframe.md`**.

## Subcommands

- `triage-cli investigate <id-or-url>` — interactive guided session. Fetches
  the ticket, downloads attachments to a scratch workspace, prompts the
  analyst to drop in files / paste logs, then runs the structured pipeline
  and writes the ticket folder. Requires TTY.
- `triage-cli triage <id-or-url>` — headless single-shot. Same pipeline,
  no evidence prompts. Writes the ticket folder and prints
  `FORK_PACKET.md` to stdout (pipeable handoff).
- `triage-cli watch --view <id>` — long-running poll loop over a Zendesk
  view; runs the structured pipeline per new/updated ticket.
- `triage-cli inbox [--view ...]` — ratatui TUI over the produced ticket
  folders. Synth-summary right pane + tabbed per-file view. Requires TTY.
- `triage-cli doctor` — checks env vars, credentials, and scratch-dir
  writability; exits 0/1.
- `triage-cli build-map` — regenerates `data/cnc-map.json` from
  `apex-cnc-inventory.md`.
- `triage-cli setup` — interactive first-run that prompts for env vars and
  writes `.env`.

## Prerequisites

- **Rust toolchain 1.95+** (stable). Install via `rustup`:
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **Zendesk credentials** with read scope on tickets: an agent email plus an
  API token.
- **At least one LLM provider** configured (see *LLM providers* below). The
  default is the internal `unleash` gateway.
- **Datadog credentials** are optional enrichment for `triage`, `investigate`,
  and watcher mode.

## Install

### Windows (primary platform)

Open PowerShell and run:

```powershell
irm https://raw.githubusercontent.com/midwestman35/triage-cli/main/install.ps1 | iex
```

This downloads the latest release, verifies its SHA256 against the published `SHA256SUMS`, installs `triage-cli.exe` into `%LOCALAPPDATA%\Programs\triage-cli\bin\`, and seeds the data directory at `%LOCALAPPDATA%\triage-cli\`. Open a new PowerShell window after install for `$PATH` to refresh.

The script does not require admin privileges and never modifies machine-wide settings.

### macOS / Linux

```bash
curl -fsSL https://raw.githubusercontent.com/midwestman35/triage-cli/main/install.sh | bash
```

Installs the binary into `~/.local/bin/triage-cli` and seeds the data directory at `~/Library/Application Support/triage-cli/` (macOS) or `${XDG_DATA_HOME:-~/.local/share}/triage-cli/` (Linux).

### Install script flags

Both scripts accept these flags:

| Flag | Purpose |
|---|---|
| `-Version v0.2.0` / `--version v0.2.0` | Pin to a specific release tag. |
| `-Channel prerelease` / `--channel prerelease` | Pick the newest prerelease instead of the latest stable. |
| `-DryRun` / `--dry-run` | Print every action without executing. Useful for review. |

To pass flags through `iex` on Windows, download the script first:

```powershell
irm https://raw.githubusercontent.com/midwestman35/triage-cli/main/install.ps1 -OutFile install.ps1
.\install.ps1 -Version v0.2.0
```

### Upgrading

Re-run the same install one-liner. The script detects the newer release, verifies SHA256, and replaces the binary in place. Your `.env`, `MEMORY.md`, and other local state are not touched.

### Migrating from a repo-clone install

If you previously installed by cloning the repo and running `cargo build --release`, run once from inside that clone:

```bash
triage-cli migrate-home
```

This copies `.env`, `MEMORY.md`, `apex-cnc-inventory.md`, and the `data/` directory into `$TRIAGE_HOME` (or the platform default), after which the binary can be invoked from any directory.

### Uninstall

There is no uninstaller. Delete the binary directory (`%LOCALAPPDATA%\Programs\triage-cli\` on Windows, `~/.local/bin/triage-cli` elsewhere) and the data directory (`%LOCALAPPDATA%\triage-cli\` on Windows, the path printed by `triage-cli doctor` elsewhere).

### Build from source

The "clone and `cargo build --release`" path described in `CLAUDE.md` still works and is the supported developer setup. End users should prefer the install scripts.

## First-run setup

```bash
cd triage-cli                  # the dir with data/, MEMORY.md, apex-cnc-inventory.md
triage-cli setup               # interactive: writes .env
triage-cli build-map           # regenerates data/cnc-map.json
triage-cli doctor              # green/red health check; exits 1 if anything critical is missing
```

`setup` is idempotent — values from an existing `.env` become defaults on
re-run. You can also manually copy `.env.example`:
```bash
cp .env.example .env
```

## Configuration

### v1-specific environment variables

| Variable | Purpose |
| --- | --- |
| `TRIAGE_TICKETS_ROOT` | Where ticket folders are written. Default `./Tickets`. Point this at a Drive-synced folder (e.g. `~/Drive/CarbyneNOC/Tickets`) to share with the team. |
| `TRIAGE_RUBRIC_PATH` | Dev override: load the fork rubric from this file instead of the embedded copy. Release runs should leave this unset. |
| `TRIAGE_OWNER` | Identifier recorded in `STATE.md`'s `owner` field. Falls back to `$USER` then `"unknown"`. Used by the soft-lock conflict check. |
| `DIFF_VIEWER` | Command run on `--diff` soft-lock conflicts (e.g. `code --diff`). Falls back to `diff -u` printed to stderr. |

### Service credentials

| Variable | Purpose |
| --- | --- |
| `ZENDESK_SUBDOMAIN` | Your Zendesk subdomain (the `<sub>` in `<sub>.zendesk.com`). |
| `ZENDESK_EMAIL` | Agent email used for Basic auth. |
| `ZENDESK_API_TOKEN` | Zendesk API token. The client appends `/token` to the email automatically; do not append it yourself. |
| `DD_API_KEY` | Optional Datadog API key for `triage` / `investigate` / watch enrichment. |
| `DD_APP_KEY` | Optional Datadog application key. |
| `DD_SITE` | Datadog site host. Default `datadoghq.com`. |
| `DD_CALL_CENTER_TAG` | Datadog tag key for the call-center filter. Default `@log.machineData.callCenterName`. |
| `DD_STATION_TAG` | Reserved for future station-level filtering. Currently unused. |
| `LLM_PROVIDER` | `unleash` (default) or `codex`. |
| `UNLEASH_API_KEY` | Required when `LLM_PROVIDER=unleash`. |
| `UNLEASH_ASSISTANT_ID` | Required when `LLM_PROVIDER=unleash`. The model is selected server-side by the assistant; the CLI does not pass a model parameter. |
| `CODEX_MODEL` | Model identifier passed to `codex exec` when `LLM_PROVIDER=codex`. Default `gpt-5.5`. |

## LLM providers

| Value | Mechanism | Auth | Notes |
|---|---|---|---|
| `unleash` *(default)* | HTTP to `/chats` | `UNLEASH_API_KEY` + `UNLEASH_ASSISTANT_ID` | Internal Axon gateway. Model is chosen server-side by the assistant ID. |
| `codex` | Subprocess to `codex exec` | Inherits codex OAuth | `codex` must be on PATH. Default model `gpt-5.5`, override with `CODEX_MODEL`. |

`unleash` is the production path; `codex` is the dev escape hatch when the
gateway is unreachable. The `claude` and `openai` providers were removed
2026-05-14 — see `docs/adr/0002-prune-claude-openai-providers.md`.

## Building the site map

`data/cnc-map.json` is the lookup table from Zendesk requester orgs to APEX
`site_name` values (the Datadog filter key) and CNC UUIDs. It is generated
from the markdown inventory at `apex-cnc-inventory.md`.

To rebuild it:

```bash
triage-cli build-map
```

This rewrites `data/cnc-map.json` and `data/cnc-map-gaps.md` (the latter
records inventory rows missing a CNC UUID or `site_name`). When the upstream
Confluence inventory changes, refresh `apex-cnc-inventory.md` out-of-band
(e.g. via the operator's Confluence-connected chat client), then re-run
`build-map`. There is no Confluence module in this repo by design.

## Usage

### Check your setup first

```bash
triage-cli doctor
```

Prints green/red checks for Zendesk credentials, the selected LLM provider
key (or subprocess binary on PATH), and scratch-dir writability. Exits 0
when all critical checks pass.

### Guided investigation

```bash
triage-cli investigate 12345
triage-cli investigate https://<sub>.zendesk.com/agent/tickets/12345
```

The primary daily-use path. Fetches the ticket, downloads attachments to a
scratch workspace, prompts you to drop in additional local files or paste
logs, looks up similar prior investigations from the memory layer, runs the
structured LLM call, and writes the five-markdown ticket folder under
`${TRIAGE_TICKETS_ROOT:-./Tickets}/<id>/`.

Pre-supply evidence on the command line to skip the drop-and-wait step:

```bash
triage-cli investigate 12345 --file ./station.log --paste 'console=WARN audio dropped'
triage-cli investigate 12345 --no-llm     # skip the LLM; useful for dry runs
triage-cli investigate 12345 --verbose    # phase-by-phase stderr trace
```

`--file` and `--paste LABEL=TEXT` may be repeated.

### Fast one-shot triage

```bash
triage-cli triage 12345
triage-cli triage https://<sub>.zendesk.com/agent/tickets/12345
```

Headless; pipeable. The same ticket folder is written; `FORK_PACKET.md` is
streamed to stdout for piping into chat / ad-hoc tools. Status and verbose
output go to stderr.

### Common flags (shared by `triage` and `investigate`)

```bash
triage-cli triage 12345 --verbose              # phase-by-phase progress on stderr
triage-cli triage 12345 --at 2026-05-06T14:32:00Z   # anchor override
triage-cli triage 12345 --site us-nv-nvdps-apex     # bypass site lookup
triage-cli triage 12345 --cnc de9ee414-da5a-471d-bac2-10643190da0b
triage-cli triage 12345 --no-logs              # skip Datadog
triage-cli triage 12345 --no-llm               # skip LLM call
triage-cli triage 12345 --levels error,warn,info
triage-cli triage 12345 --window-minutes 60    # default 30
triage-cli triage 12345 --force                # overwrite another analyst's STATE.md
triage-cli triage 12345 --diff                 # on soft-lock conflict, open $DIFF_VIEWER
```

`--at` accepts ISO 8601 with offset, including trailing `Z`. Use it when the
ticket was filed well after the incident.

## Ticket folder output

A successful investigation writes exactly five files atomically under
`${TRIAGE_TICKETS_ROOT:-./Tickets}/<id>/`:

| File | Content |
| --- | --- |
| `INTAKE.md` | Housekeeping checklist, ticket facts, one-line fingerprint, LLM-emitted summary bullets, context-pulls table, initial fork hypothesis, intake decision. |
| `EVIDENCE_PREFLIGHT.md` | Gathered-evidence table, decisive evidence, missing / non-decisive evidence. |
| `FORK_PACKET.md` | Fork letter (A/B/C/D), confidence, reasoning, quoted rubric row, evidence summary, related work, handoff checklist. |
| `DRAFTS.md` | CONFIRM-gated drafts: customer-facing reply, internal Zendesk note, Jira draft (fork A only). |
| `STATE.md` | YAML frontmatter only: ticket_id, fork, confidence, quoted_rubric_row, rubric_version, owner, status, related, cluster, validator_warnings. |

Fork letters:

- **A** Engineering Jira
- **B** Vendor or Internal IT
- **C** NOC self-resolve
- **D** Cannot fork yet (rubric demands more evidence — the next step is gathering it, not routing)

See `docs/spec/v1-reframe.md` for the full file contract, including the
exact section list inside each file.

## Workflow

1. **Start** — `triage-cli investigate 12345` (or `triage 12345` for
   headless).
2. **Engine runs** — Zendesk fetch → customer history → memory lookup →
   evidence intake → site resolution → optional Datadog enrichment → PII
   redaction → structured LLM call.
3. **Ticket folder appears** — `Tickets/12345/` is written atomically; the
   CLI prints `FORK_PACKET.md` to stdout and "Ticket folder ready: ..." to
   stderr.
4. **Review** — open `Tickets/12345/` in your editor (or Claude Code).
   Inspect the fork commitment in `FORK_PACKET.md`, the evidence trail in
   `EVIDENCE_PREFLIGHT.md`, and the prepared drafts in `DRAFTS.md`.
5. **Act on drafts** — copy-paste the customer reply / internal note /
   Jira draft after review. The CLI does not post anything autonomously.
6. **Closure** — manually update `STATE.md` (`status: closed`) when the
   fork is acted on. v1 has no closure automation.

## Watching a Zendesk view

```bash
triage-cli watch --view 12345
```

Polls every 5 minutes (`--interval 300`). On first run, triages every ticket
whose `updated_at` is within the last 24 hours (`--backfill 24h`) and
silently marks older tickets as "seen". Emits one structured status line
per ticket to stderr. State persists across restarts at
`data/watcher-state-<view-id>.json`.

Common flags:

- `--backfill 0` — watermark mode; only future updates trigger triage.
- `--backfill inf` — triage every ticket in the view on first run.
- `--print-notes` — also stream the produced `FORK_PACKET.md` content to
  stdout per ticket.
- `--no-logs` — skip Datadog.

See `docs/runbooks/06-watching-a-view.md` for a full operator runbook.

## Inbox TUI

```bash
triage-cli inbox                       # your assigned tickets
triage-cli inbox --view 12345 --poll 60
```

A ratatui app over the produced ticket-folder corpus. Scans
`${TRIAGE_TICKETS_ROOT:-./Tickets}/` for subdirectories containing a
`STATE.md`. Each row is one ticket; the right pane defaults to a synth
single-line summary (fork letter, confidence, rubric row, status, owner,
related). `Tab` cycles into a tabbed view across `INTAKE.md`,
`EVIDENCE_PREFLIGHT.md`, `FORK_PACKET.md`, `DRAFTS.md`, `STATE.md`. A
non-blocking yellow banner appears when the selected ticket's
`rubric_version` does not match the shipped rubric.

**Keybindings**: `↑/k`, `↓/j` navigate · `Enter` triage a queued ticket /
focus the detail pane · `Tab` / `Shift+Tab` cycle file tabs · `Esc` return
to synth view (then to the list) · `r` refresh · `y` copy the synth summary
to clipboard · `o` open the ticket in Zendesk in the browser · `q` /
`Ctrl-C` quit.

## Project layout

```
triage-cli/
├── README.md                       # this file
├── CLAUDE.md / AGENTS.md           # agent-facing repo guidance (kept in sync)
├── .env / .env.example
├── apex-cnc-inventory.md           # source of truth for the CNC map
├── MEMORY.md                       # investigation memory (editable)
├── Tickets/                        # ticket folders (configurable via TRIAGE_TICKETS_ROOT)
│   └── <id>/
│       ├── INTAKE.md
│       ├── EVIDENCE_PREFLIGHT.md
│       ├── FORK_PACKET.md
│       ├── DRAFTS.md
│       ├── STATE.md
│       └── .debug/                 # raw LLM responses stashed after retry failure
├── triage-notes/                   # scratch dir used by interactive workspace + doctor
├── data/
│   ├── cnc-map.json                # generated by `triage-cli build-map`
│   ├── cnc-map-gaps.md             # generated; rows skipped during conversion
│   ├── memory.db                   # SQLite FTS5 index over MEMORY.md (schema v2)
│   └── watcher-state-<view>.json   # per-view watch/inbox state
├── docs/
│   ├── spec/v1-reframe.md          # the v1 contract; authoritative
│   ├── adr/                        # architecture decisions
│   ├── runbooks/                   # operator runbooks
│   └── CHEATSHEET.md
├── archive/                        # frozen pre-port Python source
└── triage-cli-rs/                  # Rust crate
    ├── Cargo.toml
    ├── REGRESSIONS.md
    ├── playbook/
    │   └── fork-rubric.md          # embedded at build time via include_str!
    └── src/
        ├── main.rs                 # entry point
        ├── cli.rs                  # clap subcommands
        ├── pipeline.rs             # investigate_one_structured
        ├── ticket_folder.rs        # five-markdown writer + soft-lock
        ├── playbook.rs             # Rubric loader (embedded + env override)
        ├── zendesk.rs              # Zendesk HTTP client
        ├── datadog.rs              # Datadog Logs v2 HTTP client
        ├── llm.rs                  # provider dispatch + structured output + retry
        ├── providers/              # mod, unleash, codex
        ├── tui/                    # inbox ratatui app
        ├── models.rs               # serde data models
        ├── memory.rs               # MEMORY.md + FTS5 (schema v2)
        ├── watcher.rs              # poll loop + state
        ├── interactive.rs          # prompts, attachments, drop-and-wait
        ├── investigation.rs        # session builder
        ├── extract.rs              # ticket ID parse + site lookup + anchor
        ├── build_map.rs            # cnc-map.json generation
        ├── setup.rs                # `triage-cli setup` + `doctor`
        └── redact.rs               # PII scrub at LLM boundary
```

## Common dev commands

```bash
cd triage-cli-rs
cargo build --release           # ~7 MB binary at target/release/triage-cli
cargo test --lib                # unit tests (mocked clients; no network)
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

## Troubleshooting

**`triage-cli: command not found`**
The binary isn't on PATH. Either run it as
`triage-cli-rs/target/release/triage-cli ...` or symlink it (see *Install*).

**`✗ <provider key> not set` from `doctor`**
The selected `LLM_PROVIDER` doesn't have its required credential. Either set
the env var or switch to a different provider via `LLM_PROVIDER=...`.

**`Zendesk auth failed - check ZENDESK_EMAIL and ZENDESK_API_TOKEN`**
The Zendesk client appends `/token` to the email when forming basic auth —
do not pre-append it. Also confirm the token has read scope on tickets.

**`site_name '<X>' contains characters that are unsafe`**
The Datadog client validates `site_name` before injecting it into the query
string. Either fix the offending entry in `apex-cnc-inventory.md` and re-run
`build-map`, or pass a clean value via `--site`. Site names should match the
lowercase-with-hyphens convention (e.g. `us-nv-nvdps-apex`).

**`STATE.md soft-lock conflict: owned by <other-analyst>` (exit code 2)**
Another analyst already wrote a ticket folder for this ticket. The CLI
preserves their work and refuses to overwrite without an explicit
acknowledgement. Options:

- Talk to the other analyst — they may be mid-investigation.
- Re-run with `--diff` to see the full STATE.md diff (in `$DIFF_VIEWER` if
  set; otherwise `diff -u` printed to stderr).
- Re-run with `--force` to overwrite. This is intentional — the soft-lock is
  a warning, not enforcement.

`TRIAGE_OWNER` sets the owner identifier recorded in `STATE.md`; the default
is `$USER`.

**Validator soft-warnings printed after a successful run**
The rubric-row validator is soft-warn (spec § 10, decision 1). If the LLM's
`quoted_rubric_row` is not a verbatim substring of the shipped rubric, the
warning is printed to stderr and stashed in `STATE.md`'s
`validator_warnings: [...]` field. The investigation still completes — the
warning surfaces drift rather than blocking it.

**`validation failed; raw response stashed at <path>`**
Two consecutive parse / shape-validation failures. The raw LLM response is
saved at `${TRIAGE_TICKETS_ROOT}/<id>/.debug/llm-response-<timestamp>.json`
for inspection. No ticket folder is written on this failure path; re-run
the command after fixing the underlying provider issue.

**Inbox shows a yellow `rubric_version` banner**
The selected ticket's `STATE.md` was written under a different rubric
version than the one shipped with your current binary. The on-disk artifact
is honored as-is; the banner just calls attention to the drift.

**Site cannot be resolved**
Pass `--site <site_name>` or `--cnc <uuid>` to bypass lookup, or fix the
mapping in `apex-cnc-inventory.md` and re-run `build-map`.
