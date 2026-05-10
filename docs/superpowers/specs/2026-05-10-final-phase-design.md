# Final phase: redactor + context builder + inbox polish

**Status:** draft, awaiting user review
**Date:** 2026-05-10
**Baseline commit:** `857df34` (merge: fix(inbox): surface poll aborts as TUI notifications and wire --no-logs flag)
**Origin:** brainstorm session — harvest of NocLense (sibling React/TS log-triage app, abandoned after scope creep) for ideas that genuinely belong in triage-cli's 1.0.

## Goal

Land three carefully-scoped additions, then declare 1.0 feature-complete. The features were selected from a wider candidate list against an explicit "stop at three" budget. Each one closes a gap that exists today, not a hypothetical future one:

1. **PII redactor (pre-LLM)** — today the pipeline sends Zendesk content (including internal-only comments) straight to Claude. Acceptable for a single-user terminal tool, but a real risk surface as soon as anything posts back to Zendesk. Close it now.
2. **Token-aware context builder** — `pipeline.triage_one` currently packs ticket + raw Datadog lines and hopes. Score evidence by relevance and drop noise; produces sharper LLM output, especially on busy sites.
3. **Inbox tokens + density modes** — extract inline styling into Textual CSS as a maintainability refactor (not a theme system), and add a compact / comfortable density toggle for heavy queue work.

After these three: no further v1 features. Bug-fixes and docs only until 1.0 ships.

## Non-goals (explicitly, to enforce the budget)

These are direct lessons from NocLense's scope-creep death spiral. Each one is a "no" you can point to during code review:

- **No new input formats.** Zendesk + Datadog + investigation evidence (file/paste). That's the frozen set.
- **No new AI providers.** Claude Agent SDK only. Do not add a provider layer "for flexibility" — see CLAUDE.md.
- **No new external integrations.** Zendesk and Datadog are it. No Jira, Confluence, PagerDuty, Slack, etc.
- **No vector embeddings, semantic search, agentic sub-agents, or auto-tagging.** NocLense documented walking these back, then proposed them again. Do not.
- **No "case management" expansion** beyond the existing `InvestigationSession` / `TriageReport` shape. No bookmarks, severity levels, tag taxonomies, multi-case workspaces, encrypted export packages.
- **No themes / theme switcher** — already a non-goal in `2026-05-07-tui-character-design.md`. Feature 3's "tokens" are organizational, not a theme system.
- **No name detection in the redactor.** Documented gap; revisit only if compliance requires (and even then, evaluate whether a separate NER pre-pass justifies the dependency).
- **No re-redaction of Zendesk-pre-redacted fields.** If a value already looks redacted (e.g., `***-***-1234`), pass through unchanged.

If a fourth idea feels essential during implementation, it goes in `docs/superpowers/specs/` as its own future spec, not into this one.

---

## Feature 1 — PII redactor

### Scope (locked during brainstorm)

| Decision | Value |
|---|---|
| Categories redacted | Caller PII only: phone numbers, postal addresses, GPS / lat-long coords |
| Categories explicitly **not** redacted | Names (any kind), internal humans (employee names / `@yourcompany.com` emails), network/device IDs (IPs/MACs), operational IDs (Call-IDs, ticket #s, station codes, CNC names, site names) |
| Action | Replace with typed placeholder: `<PHONE>`, `<ADDR>`, `<COORDS>`. No numbering / no uniqueness preservation. |
| Placement | At the LLM call boundary — inside `triage_cli/llm.py`. All three LLM-touching functions (`triage()`, `extract_anchor()`, `extract_site()`) redact internally before sending. |
| Default | On |
| Disable flag | `--no-redact` (per-invocation; not stored in any config) |

### Why "at the LLM boundary" (vs at fetch or at bundle assembly)

- **At fetch** — too aggressive; analysts legitimately need raw caller info to follow up. Killing it at the network edge means the original data never exists in memory, breaking debugging and the `investigate` flow's read-only inspection.
- **At each bundle site** — explicit and visible, but error-prone: a future bundle path could forget to redact. Multiple points of trust.
- **At the LLM boundary (chosen)** — single point of trust. Today only `pipeline.triage_one` calls into `llm.py` directly; every other surface (`investigate`, `inbox`, `watch`) reaches the LLM transitively through `triage_one`. Putting redaction in `llm.py` protects all of them automatically and makes it impossible for a future consumer to bypass.

### Detection rules

Pure-stdlib regex. No new dependencies.

- **Phone:** match `+?1?[-.\s]?(?:\(?\d{3}\)?[-.\s]?)?\d{3}[-.\s]?\d{4}` plus a bare-10-digit fallback. Avoid matching inside obvious operational IDs (e.g., reject if the match is part of a longer alphanumeric token).
- **Postal address:** heuristic for `<number> <Capitalized words> <street suffix>` where street suffix ∈ a small list (`St|Ave|Rd|Blvd|Ln|Dr|Ct|Way|Pl|Hwy|Pkwy|Ter|Cir|...`). Optionally followed by `<unit>` (Apt/Ste/Unit). Optionally followed by `,? <City>, <ST> <ZIP>`.
- **GPS coords:** decimal lat,lon pairs: `-?\d{1,2}\.\d{4,}\s*[,;\s]\s*-?\d{1,3}\.\d{4,}`. Require 4+ decimal places to avoid matching version numbers, prices, etc.

False positives are accepted in v1; false negatives on names are accepted (documented gap).

### Pass-through guard

If a candidate match itself contains any of `***`, `xxx` (case-insensitive), or `[REDACTED]`, do not re-redact. Keep the original masking visible to the LLM — re-wrapping it is noise.

### Audit / visibility

- **Stderr** (when `--verbose` set): `redacted: 3 phones, 1 address, 0 coords` per LLM call. Two LLM calls per pipeline run = up to two lines.
- **When disabled** (`--no-redact`): stderr always prints `redaction: disabled` regardless of `--verbose`, so operating without redaction is impossible to miss.
- **Saved JSON** (`investigation` flow): the `TriageReport` JSON gains an optional `redaction_summary` field of shape `{"phones": int, "addresses": int, "coords": int, "enabled": bool}`. Renders nothing in the markdown sidecar; lives only in JSON for audit.

### Module structure

- **New:** `triage_cli/redact.py` (~80–150 LOC)
  - `redact(text: str) -> tuple[str, RedactionCounts]` — pure function
  - `RedactionCounts` — pydantic v2 model: `{phones: int, addresses: int, coords: int, enabled: bool}`
  - Compiled regex constants at module level
- **Modified:** `triage_cli/llm.py`
  - All three call sites (`triage()`, `extract_anchor()`, `extract_site()`) accept a new optional `redact_enabled: bool = True` kwarg
  - When `redact_enabled`, route every text input through `redact.redact()` before assembling the message
  - Counts surfaced via the existing `verbose` channel
- **Modified:** `triage_cli/cli.py`
  - Add `--no-redact` flag to `triage`, `investigate`, `watch`, `inbox`. Threaded through to `llm.py` via existing options shapes.
- **Modified:** `triage_cli/models.py`
  - Add `redaction_summary: RedactionCounts | None = None` to `TriageReport`.
- **No change:** `scripts/certify_readonly_my_queue.py`
  - The cert script's existing contract ("does not call Datadog or the LLM" — see CLAUDE.md) already covers the PII concern at this layer. The redactor is invisible to it. Worth re-running after merge to confirm nothing about the read-only boundary changed.

### Tests

- **New:** `tests/test_redact.py`
  - Positive cases per category (multiple phone formats, US addresses with/without unit/ZIP, decimal coord pairs)
  - Negative cases: operational IDs that look phone-like (e.g., `Call-ID 5551234567@host`), version numbers, version+date strings near coord regex
  - Pass-through guard cases (already-redacted values)
  - Counts match for mixed inputs
- **Modified:** existing `llm.py` / `pipeline.py` tests to assert redaction is on by default and counts are surfaced when verbose

### Edge cases / open questions

- **Phone regex precision** — strict (E.164-ish only) vs permissive (any 10+ digit blob)? Default: permissive enough to catch dashed/parenthesized US formats; flagged for tuning post-1.0 if false positives are noisy.
- **Multilingual addresses** — out of scope; document as "US-style addresses only."
- **LLM may quote redacted content in its output** — fine. The output token (`<PHONE>`) is opaque; quoting it doesn't leak anything.

---

## Feature 2 — Token-aware context builder

### Scope

Apply to `pipeline.triage_one`'s log-bundle assembly only — the inputs to the `triage()` LLM call. Out of scope for v1: `extract_anchor` (small input; doesn't benefit) and the `investigation` flow's `as_user_message` (different evidence shape; revisit if real friction emerges, not preemptively).

### Why this is needed

`pipeline.triage_one` today queries Datadog with a `±window_minutes` window around the anchor, capped at 200 lines (`log_truncated=True` at cap). Those lines are dumped into the prompt verbatim. On busy sites, the cap is hit routinely and the LLM sees a wall of low-relevance noise. NocLense's documented #1 lesson:

> "Sending 500 high-signal log lines produces dramatically better diagnoses than sending 10,000 unfiltered lines."

We don't need 10,000 → 500. We need 200 → ~50 *intentional* lines, with a record of what got dropped so the analyst can sanity-check.

### Scoring heuristic (deterministic, simple, no ML)

Each candidate Datadog log line gets an integer score:

| Component | Weight |
|---|---|
| Severity: `ERROR` / `error` | +5 |
| Severity: `WARN` / `warn` | +3 |
| Severity: `INFO` / `info` | +1 |
| Severity: `DEBUG` / `debug` | 0 |
| Token from ticket subject appears in line (case-insensitive, len ≥ 4) | +2 per token, capped +6 |
| Within ±60s of resolved anchor (vs the broader configured window) | +2 |
| Identical message body already kept (dedupe penalty) | −3 |

Tie-breakers, in order: timestamp ascending (oldest first within the same score), original index.

Scoring is a pure function over `(line, anchor, subject_tokens, already_kept_messages)`. No state across calls.

### Token budget

Conservative, hardcoded. Not user-configurable in v1.

| Section | Target tokens |
|---|---|
| System prompt (existing) | ~1000 |
| Ticket payload | up to 3000 |
| Log section (after scoring) | up to 6000 |
| Reserved for response | 2000 |

Token estimation: `len(text) // 4` (the ~4-chars-per-token approximation). No tokenizer dependency — accuracy is good enough for this purpose, and the alternative is dragging in `tiktoken` or similar for a number we only need ballpark-correct.

### Selection algorithm

```
1. Score every candidate line.
2. Sort descending by score, stable on tie-breakers.
3. Greedy fill: pop highest-scored line, add if running token total ≤ budget.
4. Re-sort the kept set chronologically before rendering.
5. Return (rendered_log_section, ContextSummary).
```

Chronological output preserves the natural reading order an analyst expects, even though selection was relevance-based.

### Visibility

- **Stderr** (when `--verbose`): `logs: 200 candidates, kept 47 (score-ranked, 6021 of 6000-token budget used)`
- **In the rendered triage note** (always, when any lines were elided): a compact line at the bottom of the evidence section: `Note: 153 of 200 log lines elided by relevance scoring (severity, subject match, anchor proximity).`
- **In saved JSON:** `TriageReport` gains `context_summary: {candidates: int, kept: int, budget_tokens: int, used_tokens: int}` for audit.

The score values themselves are an implementation detail — never surfaced.

### Module structure

- **New:** `triage_cli/context.py` (~100–150 LOC)
  - `score_log_line(line, anchor, subject_tokens, already_kept) -> int` — pure
  - `extract_subject_tokens(subject: str) -> list[str]` — lowercase, drop stopwords, len ≥ 4
  - `estimate_tokens(text: str) -> int` — `len(text) // 4`
  - `build_log_section(lines, anchor, subject, budget=6000) -> tuple[str, ContextSummary]`
  - `ContextSummary` — pydantic v2: `{candidates: int, kept: int, budget_tokens: int, used_tokens: int}`
- **Modified:** `triage_cli/pipeline.py`
  - `triage_one` calls `context.build_log_section` with the Datadog result
  - The returned `ContextSummary` is attached to the `TriageReport` it builds
- **Modified:** `triage_cli/render.py`
  - When `report.context_summary.kept < report.context_summary.candidates`, print the elision note in the rendered output
- **Modified:** `triage_cli/models.py`
  - Add `context_summary: ContextSummary | None = None` to `TriageReport`

### Tiny-input fast path

If the candidate list ≤ 25 lines AND total estimated tokens ≤ 2000, skip scoring and return everything. Avoids overhead and an unnecessary "elided 0 of 12" footnote when the input was small.

### Tests

- **New:** `tests/test_context.py`
  - Score weights produce expected ranking on synthetic mixed-severity input
  - Subject-token boost works (ticket subject "SIP timeout" → lines containing "timeout" score higher)
  - Anchor proximity boost works (lines within ±60s rank above same-severity lines outside)
  - Dedupe penalty prevents identical messages from filling the budget
  - Tiny-input fast path bypasses scoring
  - Token budget honored — kept set never exceeds budget
  - Output is chronologically sorted regardless of selection order

### Edge cases

- **No anchor available** — proximity component returns 0; selection still works on severity + subject.
- **All lines same severity** — ranking falls back to subject match + chronology; behavior is sane.
- **Empty subject** — `subject_tokens = []`, no boost from that component; still selects sensibly.
- **Single-line input** — fast path returns it unchanged.

---

## Feature 3 — Inbox tokens + density modes

### Scope

| Decision | Value |
|---|---|
| Touches | `triage_cli/inbox/app.py`, `triage_cli/inbox/widgets.py`, `triage_cli/watcher.py` (state schema), new `triage_cli/inbox/inbox.tcss` |
| Out of scope (explicit) | Themes / theme picker (already a non-goal in `2026-05-07-tui-character-design.md`); colors for `triage` / `investigate` markdown output (Rich handles its own); animations beyond Textual defaults |
| Density modes | Two: `compact` (1-line rows) and `comfortable` (2-line rows: subject + requester preview) |
| Density toggle | Keybinding `d` cycles density |
| Default density | `comfortable` on first run |
| Persistence | Stored in existing `data/watcher-state-<view-key>.json` under a new optional `ui` section |

### Why "tokens, not themes"

Today the inbox styling is set inline / per-widget. That's fine for getting it shipped, but it makes the calm-Rich-default decision invisible — colors live wherever a developer happened to put them, and divergence creeps in. The token refactor extracts the existing palette into one Textual CSS file with named tokens (`$status-triaged`, `$status-pending`, `$row-pad-compact`, etc.). **The palette doesn't change; the organization does.**

This is *explicitly not* a theme system. There is no `--theme` flag, no light/dark toggle, no user-customizable palette. The existing `2026-05-07-tui-character-design.md` non-goals stand verbatim.

### Token surface (initial)

```css
/* triage_cli/inbox/inbox.tcss */
$status-triaged:  $success;       /* green-ish, calm */
$status-triaging: $warning;       /* amber, in-flight */
$status-pending:  $secondary;     /* dim */
$status-failed:   $error;         /* muted red */

$priority-urgent: $error;
$priority-high:   $warning;
$priority-normal: $foreground;
$priority-low:    $secondary;

$row-pad-compact:     0;
$row-pad-comfortable: 1;

$panel-border: $primary-darken-2;
```

(Exact values to be tuned during implementation against the existing inline styles — the goal is parity, not redesign.)

### Density modes

- **`comfortable`** (default): two visible lines per row — line 1 is `<status-icon> <ticket-id> <subject (truncated)>`; line 2 is `    <requester> · <updated_at relative>`. Padding `$row-pad-comfortable`.
- **`compact`**: one visible line — `<status-icon> <ticket-id> <subject (truncated)> <requester> <updated_at>`. Padding `$row-pad-compact`. More rows visible at once for queue-burndown work.

Toggle is the keybinding `d`. On toggle:
- Inbox redraws with the new density
- A non-blocking notification (`self.notify(...)`) says `density: compact` (or `comfortable`)
- The new value is persisted to state immediately

### State schema extension

`data/watcher-state-<view-key>.json` (shared with `watch`) currently has shape `{"version": 1, "triaged": {...}}`. Extend to:

```json
{
  "version": 2,
  "triaged": {...},
  "ui": {
    "density": "comfortable"
  }
}
```

**Migration path:** bump `STATE_VERSION` to `2`. The current code raises on version mismatch (per CLAUDE.md). Replace that with a small forward-migrator: when reading a v1 file, populate `ui.density = "comfortable"` and write back as v2 on the next state save. No analyst action required.

`watcher.py` is a shared file with `watch`; the migration must not break `watch`-only invocations (they ignore the `ui` block).

### Status bar

The existing inbox already shows `Last poll: HH:MM. Next: HH:MM.` in the empty-list state. Extend: when the list is non-empty, show the same timing in the footer next to the existing footer keys. No new dependency, no new widget — just an extension of `Footer` content.

### Module structure

- **New:** `triage_cli/inbox/inbox.tcss` — token definitions + selectors
- **Modified:** `triage_cli/inbox/app.py`
  - Add `Binding("d", "cycle_density", "density")` to `BINDINGS`
  - Implement `action_cycle_density()` — flip state, persist, notify, redraw
  - Load `inbox.tcss` via Textual's `CSS_PATH`
  - On mount, read `ui.density` from state (default `comfortable`)
- **Modified:** `triage_cli/inbox/widgets.py`
  - `TicketListWidget` accepts a `density` prop and renders rows accordingly
  - Compact and comfortable layouts share data; only the row template differs
- **Modified:** `triage_cli/watcher.py`
  - State schema migration (v1 → v2)
  - `WatcherState` model gains optional `ui: WatcherUIState | None`

### Tests

- **New:** `tests/test_inbox_density.py`
  - Density default is `comfortable` on a fresh state file
  - `d` action persists density to state
  - State migration reads v1, writes v2 with default `ui.density`
  - Both densities render without exception against a fixture row set
- **Modified:** `tests/test_watcher_state.py`
  - v1 → v2 migration round-trip

### Edge cases

- **State write contention** — the inbox writes state on every density toggle; `watch` writes state on every iteration. Both processes don't run simultaneously for the same view (single-user tool, per existing non-goal). Last-writer-wins is acceptable, same as the existing rule.
- **`NO_COLOR` env var** — Textual respects this natively; tokens fall back to monochrome. No additional handling required.
- **Tiny terminal** — comfortable mode shows fewer rows; that's expected. Compact mode degrades gracefully — rows still readable.

---

## Cross-cutting

### Build sequence (recommended)

1. **Feature 1 (redactor) first.** Smallest surface, highest-clarity tests, unblocks the safer-LLM posture before Feature 2 changes the same files.
2. **Feature 2 (context builder) second.** Builds on the same `llm.py` integration point as Feature 1; lands while context is fresh and avoids a second round of `pipeline.py` rebases.
3. **Feature 3 (inbox tokens + density) last.** Independent surface; touches no overlapping files. Can be done concurrently with 1+2 if desired, but linearizing avoids review-thrash.

### Dependency hygiene

No new runtime dependencies. Stdlib `re` for redaction; manual char-count token estimator for context budgeting; Textual already vendored.

### Stdout / stderr discipline (existing rule, restated)

All new diagnostic output (redaction counts, context summary, density toggle notification) follows the existing rule — stdout reserved for the rendered triage note; everything else stderr or in-TUI notification. See `CLAUDE.md`.

### Documentation updates

- `README.md` — document `--no-redact` flag (and that redaction is on by default), and the `d` density toggle in the inbox keybindings list.
- `docs/runbooks/04-troubleshooting.md` — short section on redactor false positives and how to confirm with `--no-redact`.
- `docs/runbooks/07-inbox-mode.md` — add density toggle to the keybindings table.
- `docs/CHEATSHEET.md` — add the new flag and keybinding to their respective tables.
- `CLAUDE.md` — append a note in the LLM-access section that PII redaction is applied at the boundary by default. The existing "Internal Zendesk comments **are** sent to the LLM" caveat is **refined, not removed**: comment text itself still flows through (employee notes, internal context); only caller PII inside it is redacted. The "must be revisited if anything posts back to Zendesk" warning still stands — the post-back risk is reduced but not eliminated.

### Scope guard for code review

Reviewers (and you, looking at your own diff) should reject any of the following without further discussion:

- A `--theme` flag, named-palette configuration, or any color customization surface
- Any new file format parser or input source
- Any new external API client beyond Zendesk + Datadog
- Anything that does multi-step LLM reasoning or spawns sub-agents
- A "save bookmark / tag / star" capability on tickets
- Encryption / signing on saved triage notes
- Use of `tiktoken` or another tokenizer dependency for Feature 2
- A NER model or any new ML dep for Feature 1

If you genuinely need one of these, it goes in a *new* spec, not amended into this one.

---

## Open questions for user review

These are the spots where I picked a default during the brainstorm but the choice has real downside. Flag any to change before we move to a plan.

- **Q1.** Phone regex precision — currently permissive (matches `5551234567`, `(555) 123-4567`, `555-123-4567`, `+1 555 123 4567`). Strict (E.164-only) would have fewer false positives but miss the analyst-pasted formats that show up in tickets. **Default: permissive.** Switch?
- **Q2.** Log scoring weights — `+5/+3/+1/0` for severity, `+2` per subject token (cap +6), `+2` for anchor proximity, `−3` dedupe. Numbers are reasonable but untuned. **Default: ship as-is, tune post-1.0 if real cases push back.** Acceptable?
- **Q3.** Density default — `comfortable` (more readable) or `compact` (more rows)? **Default: `comfortable`.** Switch?
- **Q4.** Stderr redaction line — print every invocation (always-on visibility) or only `--verbose`? **Default: `--verbose` only**, except when `--no-redact` is set (always announced). Switch?
- **Q5.** Where does `redaction_summary` appear in saved output — JSON only, or also a small footer in the saved markdown? **Default: JSON only** (keeps the markdown clean for paste-back). Switch?
- **Q6.** State schema bump (v1 → v2) — do a forward-migrator write-back as proposed, or hold off and only write `ui` block when the user actually toggles density (leaving v1 files untouched)? **Default: forward-migrate on first read.** Switch?
- **Q7.** Address regex scope — the current proposal includes optional `,? <City>, <ST> <ZIP>` after the street line. This catches more PII but creates a tension: if a ticket says *"caller at 123 Main St, Springfield, IL 62701"* and **Springfield** is a known site name, the address regex will swallow the city as part of the address and weaken `extract_site`'s signal. Two choices: **(a) street-line only** — match `123 Main St` but stop before the comma; preserves city/state for site matching at the cost of leaking them. **(b) full address** — match through ZIP; safer PII-wise but may force the analyst to use `--site` more often when extract_site fails. **Default: (a) street-line only**, since site extraction is the load-bearing operational signal and city names are rarely sensitive in 911 dispatch context. Switch?

---

## Appendix: NocLense lessons recap (so the next reader has the context)

NocLense was a React/TypeScript log-triage app the same author built before triage-cli. It started with a sharp goal — *reduce SIP incident triage from 30–60 min to under 15 min* — and shipped that. Then it absorbed, over ~3 months: 6 input formats, 4 AI providers, 4 external integrations (Zendesk/Jira/Confluence/Datadog), auto-tag learning loops, agentic sub-agents, vector embeddings, "case management" with severity/bookmarks/tags, workspace export packs, onboarding wizards. By March 2026 it was 141 TS files + 50 docs and stuck on consolidation work.

The three features in this spec were chosen because they each close a gap that exists in triage-cli *today* and are bounded — they have a clear "done." Five other harvestable ideas (export packaging, NER name detection, multi-provider AI abstraction, phase-model UX in interactive investigate, embedding-based dedupe) were considered and explicitly rejected as either out-of-scope or premature.

The non-goals section above is not aspirational. It is the load-bearing part of this spec.
