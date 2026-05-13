# triage-cli v1 reframe spec

Status: draft, 2026-05-13
Supersedes: `docs/05.11.26-spec-revision.md` (critique-note) and the original "guided investigation" framing in `docs/product-direction-review.md`.

This spec defines the v1 reframe of `triage-cli` for the Carbyne APEX NOC. It absorbs the workflow learned in `~/Documents/DailyNOC/_triage_pipeline/` and reshapes the CLI around it. The Rust crate at `triage-cli-rs/` stays — its data plumbing (Zendesk, Datadog, memory, watcher, inbox, site map) is the engine. The output, the skills, and the team-facing surface change.

## 1. Purpose & audience

### Who this is for

- **NOC analysts at Carbyne**, including newcomers who have never used the tool and may be skeptical that an LLM-assisted triage step belongs in their workflow.
- **The on-call shift owner**, who needs to inherit a ticket mid-investigation without losing context.
- **Reviewers / leads**, who need a uniform record of how a ticket was routed and why.

### What success looks like

- **Consistency over speed.** Two analysts triaging the same ticket produce structurally identical artifacts and quote the same rubric row. We accept slower investigations if the trade buys consistency.
- **No mis-routed tickets.** Every fork commitment cites a rubric row from a versioned `fork-rubric.md` shipped with the binary. If the LLM cannot quote a row, it cannot commit a fork.
- **No pipeline breaks.** The investigation produces a fixed-shape ticket folder. Downstream consumers (Jira draft, customer note, internal note, future automations) read from this folder, not from prose.

### Non-goals

- Speed-optimized triage. v1 is not chasing "investigate in under N seconds."
- Replacing analyst judgment. The LLM commits a fork; the analyst confirms before any external action (Jira creation, customer-facing note).
- Eliminating Claude Code / conversational refinement. The ticket folder is designed to be opened in Claude Code for follow-up; the CLI is the entry point, not the only surface.

## 2. Anchors

Every design decision in the rest of this spec traces back to one of these three commitments. Challenge an anchor here, not downstream sections.

### Anchor A — Engine / playbook split

The crate is the **engine**: Zendesk client, Datadog client, memory layer, redactor, pipeline orchestration, LLM provider trait, TUI, watcher. None of these encode Carbyne-specific knowledge.

The **playbook** is the rubric (`fork-rubric.md`), the site inventory (`apex-cnc-inventory.md`), the system prompt, and any domain-specific evidence-gathering recipes. These live alongside the engine in `triage-cli-rs/playbook/` and are loaded as data, not compiled in as logic.

The OSS carve-out (`triage-cli-oss`) is **post-v1**. We name the boundary here so that v1 code does not embed playbook strings into engine modules. No `if customer == "carbyne"` branches in the engine.

### Anchor B — Five-markdown ticket folder is the canonical output

A successful investigation produces `Tickets/<id>/` containing five files (section 4). This replaces the current single-file `triage-notes/<id>-<ts>.md` output.

Prose summaries are still emitted, but they live **inside** structured files (`INTAKE.md`, `FORK_PACKET.md`). Downstream automation reads the structured frames; humans read the prose.

### Anchor C — Drive-as-filesystem for the shared corpus

The shared `Tickets/` corpus is a folder on Google Drive synced via the user's Drive desktop client. The CLI writes local files; the Drive client syncs them under the user's OAuth session. There is **no Drive API integration in the binary**, no service account, no autonomous writes to Drive's REST endpoints.

This is a hard constraint, not a preference. Autonomous API writes to audited Atlassian / Drive surfaces have caused real incidents for the operator. The CLI never opens a network socket to Drive, Confluence, or any audited document store.

## 3. Workflow

The analyst experience for a routine triage:

1. **Start** — `triage-cli investigate 44671`
2. **Engine runs** — Zendesk fetch → customer history → memory lookup → optional evidence intake (`--file`, `--paste`) → site resolution → optional Datadog enrichment → PII redaction → LLM call.
3. **Ticket folder appears** — the CLI writes `Tickets/44671/{INTAKE.md, EVIDENCE_PREFLIGHT.md, FORK_PACKET.md, DRAFTS.md, STATE.md}` to the configured shared root.
4. **Fork is committed in the artifact, not as a side effect.** The LLM's structured response includes a `ForkCommitment` with a `quoted_rubric_row`. `FORK_PACKET.md` renders that commitment for human reading; `STATE.md` records it for machine reading.
5. **Analyst reviews.** If the fork is wrong, the analyst opens `Tickets/44671/` in Claude Code and corrects via conversation — the folder is the substrate.
6. **Analyst acts on drafts.** `DRAFTS.md` contains a Jira draft (if Fork A), a customer-facing reply (if needed), and an internal Zendesk note. Each draft is behind an explicit CONFIRM gate; the CLI never posts them autonomously.
7. **Closure** — when the fork is acted on, the analyst updates `STATE.md` (`status: closed`, `closed_at`, `closed_by`). v1 has no closure automation; this is manual.

Watcher mode (`triage-cli watch`) runs steps 1–4 on each new ticket and stops before step 5. Inbox mode (`triage-cli inbox`) is a viewer over the produced ticket folders.

## 4. Ticket folder contract

`Tickets/<id>/` contains exactly the five files below. The CLI writes them atomically (tempfile + rename, per the existing watcher-state pattern). Missing files mean the investigation aborted; partial files are never written.

### `INTAKE.md`

What the engine knew before the LLM was called. Sections:

- **Housekeeping** — ticket directory created, artifacts grouped, no loose files.
- **Ticket** — Zendesk ID, URL, status, priority, tags, requester, organization, site / CNC, region, affected stations, affected agents, call ID (if applicable), incident window, reported symptom.
- **One-line fingerprint** — `<customer> / <site> / <symptom-class> / <window> / <prior pattern if any>`. This is the cluster key.
- **Ticket summary** — 3–6 bullets, prose, what the customer reported.
- **Context pulls** — table of (pull, result, source) for: current Zendesk ticket, related Zendesk tickets, open master ticket, open Jira / known REP, Confluence / site docs, CNC / friendly-name mapping, current deployment version, known cluster match. Pulls that were not attempted in this session say `unavailable` in the source column. **The table never invents data.**
- **Initial route** — preliminary fork hypothesis with one-line justification. This is the engine's pre-LLM guess; the LLM can override.
- **Intake decision** — one of: ready for evidence preflight / known issue / needs clarification / cannot proceed.

### `EVIDENCE_PREFLIGHT.md`

What evidence was gathered, and what is missing. Sections:

- **Gathered** — table of (evidence type, source, time window, summary). Sources include: Zendesk thread, customer-history pull, memory hits, Datadog window (if enriched), local files (`--file`), pasted blobs (`--paste`).
- **Decisive evidence** — bullets, the items that move the fork.
- **Missing / non-decisive** — bullets, what would have helped but was not available. This is load-bearing: the LLM uses this section to decide whether to commit a fork or return "cannot fork yet."

### `FORK_PACKET.md`

The committed routing decision. Sections:

- **Recommendation** — fork letter (A / B / C / D), confidence (high / medium-high / medium / low), owner (engineering Jira team / vendor / internal IT / NOC self-resolve / cannot fork yet), next action.
- **Decision signal** — rubric class, rubric row (quoted verbatim from `fork-rubric.md`), why this commits the fork.
- **Evidence summary** — bullets, the strongest evidence behind the call.
- **Missing / non-decisive evidence** — mirror of the preflight section, restated in the context of the chosen fork.
- **Related work** — Zendesk siblings, Jira tickets (read-only), master tickets, cluster identifier if any.
- **Handoff** — engineering Jira needed? Vendor / internal IT request needed? Customer note needed? Internal Zendesk note needed? Each is yes / no with a one-line reason.

Forks are: **A. Engineering Jira**, **B. Vendor or Internal IT**, **C. NOC self-resolve**, **D. Cannot fork yet** (the rubric demands more evidence; the analyst's next step is gathering it, not routing).

### `DRAFTS.md`

Prepared text the analyst may send after review. Sections:

- **Customer-facing reply** — drafted in plain language, no jargon, no rubric references. Behind a `<!-- CONFIRM -->` marker.
- **Internal Zendesk note** — full triage context for the next NOC shift. References fork letter, rubric row, key evidence.
- **Jira draft** (Fork A only) — title, description, affected component, suspected area, repro steps if known. Always tagged for the REP project. **Never posted autonomously.** The analyst copy-pastes after review.

Each draft begins with `<!-- CONFIRM -->`; no external system reads from `DRAFTS.md` directly.

### `STATE.md`

Machine-readable state for the shared corpus. Frontmatter only — no prose. Fields:

```yaml
---
ticket_id: 44671
fork: B
confidence: medium-high
quoted_rubric_row: "Multiple stations at same site flip ERROR within seconds of each other → B. Vendor / IT — customer LAN, switch, or SDWAN. Link to site master ticket."
rubric_version: "2026-05-12"
owner: enrique.velazquez@axon.com
created_at: 2026-05-13T07:32:11Z
updated_at: 2026-05-13T07:32:11Z
status: open
related:
  zendesk: [43874, 42708]
  jira: []
  master: null
cluster: jeffcom-all-console-network-error
validator_warnings: []
---
```

`STATE.md` is the soft-lock surface (section 7). The CLI reads `owner` before writing to detect concurrent investigation.

## 5. Fork rubric as a versioned asset

### Location

`triage-cli-rs/playbook/fork-rubric.md` — committed alongside the source. Bundled into the binary at build time via `include_str!` and exposed through a `playbook::rubric()` accessor. Loading from disk is allowed in dev mode (`TRIAGE_RUBRIC_PATH` env var); in release builds the embedded copy is authoritative.

### Versioning

The rubric carries a `rubric_version` header (date-stamped). Every `STATE.md` records the version that was active at investigation time. A bumped rubric does not retroactively invalidate older tickets.

### LLM contract

The rubric is loaded once per process and injected into the system prompt as a non-negotiable block. The LLM is instructed: **the `quoted_rubric_row` field in the structured response must be a verbatim substring of the rubric file.** The validator (section 6) enforces this server-side; the prompt is a hint, not a guarantee.

### Editing

The rubric is a team artifact. PRs to `fork-rubric.md` are reviewed like code. The rubric file has its own change log section at the bottom. Changes ship via a normal `triage-cli` release.

## 6. LLM output contract

### Structured response

The LLM emits a single JSON document deserialized via `serde_json::from_str::<TriageReport>` in `pipeline::investigate_one`. The struct lives in `triage-cli-rs/src/models.rs`:

```rust
pub struct TriageReport {
    pub intake: IntakeBlock,
    pub evidence_preflight: PreflightBlock,
    pub fork_packet: ForkPacket,
    pub drafts: DraftsBlock,
}

pub struct ForkPacket {
    pub commitment: ForkCommitment,
    pub evidence_summary: Vec<String>,
    pub missing_evidence: Vec<String>,
    pub related: RelatedWork,
    pub handoff: HandoffBlock,
}

pub struct ForkCommitment {
    pub fork_letter: ForkLetter, // A | B | C | D
    pub confidence: Confidence,
    pub quoted_rubric_row: String,
    pub rubric_class: String,
    pub reasoning: String,
}
```

The renderer (`render.rs`) turns each block into one of the five ticket-folder files. Renderer logic is purely structural — no LLM calls, no enrichment.

### Validation

`ForkCommitment` is validated before any file is written:

- `quoted_rubric_row` must match against the loaded rubric (matching strictness is an open question — section 10).
- `fork_letter == D` is allowed only when `missing_evidence` is non-empty.
- `confidence == high` is allowed only for forks A, B, C (a high-confidence "cannot fork yet" is incoherent).

Validation failures cause the investigation to abort with a clear error and **no ticket folder is written**. The analyst sees the raw LLM response in stderr for debugging.

### Why structured, not prose

Prose triage notes drifted: every analyst (and every model) wrote them slightly differently, and downstream tooling could not rely on any field being present. Structured output makes the contract explicit. The cost is one schema migration the first time we change `ForkCommitment` — that cost is paid once and amortizes forever.

## 7. Shared corpus & soft-lock

### Drive-as-filesystem

The shared root (e.g., `~/Drive/CarbyneNOC/Tickets/`) is a Drive-synced folder on each analyst's laptop. The CLI writes local files; the Drive client uploads them. From Drive's audit log this is a normal user file write, indistinguishable from the analyst saving a Word doc — exactly the property we want.

No Drive API client, no service account, no OAuth dance inside the CLI. The CLI does not even know it is writing to a synced folder; it writes to a configured local path.

### Soft-lock semantics

`STATE.md` carries an `owner` field. The CLI, before writing into an existing `Tickets/<id>/` directory:

1. Reads `STATE.md` if it exists.
2. If `owner` is set and is not the current user, prints a warning naming the existing owner and refuses to write unless `--force` is passed.
3. If `owner` is the current user, proceeds.
4. If no `STATE.md`, treats the directory as unclaimed.

This is a soft lock — Drive does not surface file-level locks across machines, and we are not adding a coordination service. Conflicts (two analysts running `investigate` simultaneously) are possible and resolved by `--force` after a conversation. The warning is the safety, not enforcement.

### Closure

`STATE.md` updates to `status: closed` are manual. There is no nightly job, no closure detection. The lifecycle is owned by the analyst.

## 8. CLI surface

### Subcommands that change behavior

- `investigate <id>` — now produces the five-markdown ticket folder under `Tickets/<id>/`. The old single-file `triage-notes/` write path is removed (no dual-output mode). The `-v` flag still surfaces the engine trace on stderr.
- `triage <id>` — same orchestration, runs end-to-end without evidence prompts. Writes the ticket folder. The previous "pipe to stdout for chaining" use case is preserved by also printing `FORK_PACKET.md` to stdout when stdout is not a TTY; the other four files are folder-only.

### Subcommands that absorb skills

The DailyNOC skills (`triage-intake`, `triage-fork`, `jira-creation`, `resolution-note`, `test-script`) are not separate `.skill` files in v1. They are **stages** of `investigate`:

| Skill | Becomes |
|---|---|
| `triage-intake` | `INTAKE.md` generation (engine + LLM) |
| `triage-fork` | `FORK_PACKET.md` generation (LLM with rubric) |
| `jira-creation` | `DRAFTS.md` Jira section (LLM, CONFIRM-gated) |
| `resolution-note` | `DRAFTS.md` customer + internal sections (LLM, CONFIRM-gated) |
| `test-script` | Out of scope for v1 (section 9) |

### Subcommands unchanged

- `inbox`, `watch`, `doctor`, `build-map`, `setup` keep their current contracts. Inbox is updated to recognize the new ticket-folder shape as its viewable artifact.

## 9. Out of scope for v1

The following are deliberately deferred. Naming them here prevents scope creep.

- **`triage-cli-oss` carve-out.** Engine separation is honored in code structure (Anchor A), but no separate crate or repo ships in v1.
- **Test-script generation.** The DailyNOC `test-script` skill is not absorbed in v1. Reintroduce after the five-markdown contract has stabilized in production.
- **Drive API integration.** Hard prohibition, not a deferral.
- **Confluence module.** Hard prohibition (`CLAUDE.md` already enshrines this).
- **Closure automation, auto-archiving, Jira auto-post.** All actions on external systems remain manual / CONFIRM-gated.
- **Multi-tenant playbook loading.** v1 ships exactly one rubric. Switching playbooks at runtime is a `triage-cli-oss` problem.
- **Python source maintenance.** `archive/python-source-2026-05-12.zip` is frozen; no porting back.
- **Read-only Atlassian enrichment from inside the CLI.** Suggested in `05.11.26-spec-revision.md` but out of scope here — the audit cost outweighs the convenience for v1. Reconsider once Drive-as-filesystem has bedded in.

## 10. Decisions on initial open questions

Resolved 2026-05-13. Each item records the question, the chosen answer, and the rationale. Open questions discovered during implementation are tracked in `docs/notes/` rather than re-added here.

1. **`quoted_rubric_row` validator strictness.** Resolved: **soft-warn**. The validator logs a warning on mismatch but accepts the response. Until we observe real LLM behavior under the new contract, picking strict vs. normalized is premature; tightening is a backward-compatible change later. Implementation note: the warning surfaces both in stderr and in `STATE.md` as `validator_warnings: [...]`, so drift is auditable rather than invisible.
2. **Rubric version mismatch handling.** Resolved: **warn at inbox/view time**. When the inbox renderer reads a `STATE.md` with a `rubric_version` that does not match the currently shipped rubric, it shows a non-blocking banner naming both versions. The on-disk artifact is honored as-is; no rewriting.
3. **`STATE.md` concurrent-write semantics.** Resolved: **summarized diff with full-diff drilldown**. When the soft-lock detects a conflicting owner, the CLI prints a summarized diff (changed fields with old → new). A key/flag opens the full diff in `$DIFF_VIEWER` (fallback `diff -u`). The analyst must still pass `--force` to overwrite.
4. **Inbox rendering of the ticket folder.** Resolved: **both modes**. Default view is a synthetic single-pane summary (fork letter, confidence, rubric row, status, owner, related tickets). A keybind switches into a per-file tabbed view across `INTAKE` / `EVIDENCE_PREFLIGHT` / `FORK_PACKET` / `DRAFTS` / `STATE`.
5. **Memory layer alignment.** Resolved: **migrate during v1**. SQLite FTS5 schema gains `fork_letter`, `quoted_rubric_row`, and `rubric_version` columns. `MEMORY.md` format is extended; the existing prune-by-edit workflow continues to work. A one-shot migration on first post-upgrade run reindexes existing entries with empty values for the new columns.
6. **Error surface for validation failures.** Resolved: **retry once, then stash for debug**. On validation failure the pipeline retries the LLM call once with a corrective system note. A second failure stashes the raw response at `Tickets/<id>/.debug/llm-response-<timestamp>.json` and surfaces a clear "validation failed; raw response stashed at <path>" error. Standardized error taxonomy (named codes for handoff in chat / runbooks) is deferred — for v1, variants in `pipeline::PipelineError` carry enough structure that future taxonomy work is non-breaking.

## Appendix — what was rejected and why

- **Confluence-based shared corpus.** Rejected. Confluence access at Carbyne is heavily audited; autonomous API writes have triggered real incidents. Drive-as-filesystem sidesteps the audit path entirely. (Anchor C.)
- **Adding `confluence.rs` for read-only enrichment.** Rejected for v1. The `05.11.26-spec-revision.md` proposed this as an enrichment source; the audit cost-benefit does not support it yet. Reconsider post-v1.
- **Embedded LLM-callable skills as `.skill` files.** Rejected. The DailyNOC `.skill` ZIP-with-SKILL.md format made sense in a Codex-native workflow but adds an indirection that the CLI does not need. Skills are stages of `investigate`, not loadable units.
- **Anthropic HTTP SDK as the default Claude provider.** Rejected (already prohibited in `CLAUDE.md`). The OAuth-only enterprise seat does not support a provisioned API key; the `claude --print` subprocess provider stays default for Claude.
- **Single-document prose output retained as a fallback.** Rejected. Dual-output paths drift apart; one of them stops being maintained. The five-markdown folder is the only output.
