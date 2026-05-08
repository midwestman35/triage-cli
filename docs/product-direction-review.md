# Product Direction Review — Refocus on Guided Investigation + Watcher

**Date:** 2026-05-08
**Branch:** `claude/refocus-triage-cli-ErrxO`
**Scope:** Adversarial review of the refocus brief before implementing the smallest useful correction.

This is a working document. It pushes back on parts of the brief that are misdiagnosed or over-prescribed, then proposes a tighter delta plan than what the brief asks for. Implementation in this branch follows the plan in §6.

---

## 1. Where has the current implementation drifted?

The brief calls the drift "report-first / Rich-rendering / Datadog-oriented". That is partly right and partly misdiagnosed. Reading the code:

- **Datadog is already optional in the pipeline.** `pipeline.triage_one(ticket, site_entry, *, dd_client: DatadogClient | None, ...)` (`triage_cli/pipeline.py:87-97`) takes a nullable client; line 121-128 explicitly skips Datadog when `dd_client is None`, and `--no-logs` exposes that on the CLI (`triage_cli/cli.py:128`). So "Datadog as the spine" is mostly a UX/mental-model artifact, not an architectural one. **The fix is in framing, not in tearing out wiring.**

- **The actual drift is shape, not surface area.** Today's mental model is `ticket → bundle → LLM → report → render`. It is a one-shot, non-interactive transformation. There is no point in the flow where a human can say "here is more evidence, please incorporate it." That is the real gap. Calling the drift "Datadog-oriented" leads to the wrong fix (remove Datadog) when the right fix is "wrap the pipeline in a session that accumulates evidence from many sources, of which Datadog is one."

- **Site/CNC resolution is wired into the spine and is interactive.** `cli.triage` (lines 174-198) blocks on a `typer.prompt` if site can't be resolved. This is a real drift: a user with no Datadog credentials and no mapped site is *blocked* from getting a ticket-only triage without manually passing `--site`. That is the strongest evidence of "Datadog-shaped thinking" leaking into the core path — site lookup only matters because Datadog needs it. In a guided investigation where Datadog is one optional source among many, site resolution should be skipped silently when the user doesn't ask for Datadog evidence.

- **`TriageBundle.as_user_message()` always emits a `# Logs` section** (`models.py:113-126`), even when there are no logs. The LLM sees `(no logs in window)` instead of "logs were not consulted." Small but cumulative: it primes the model to think a Datadog read happened even when it didn't.

- **The inbox TUI (`triage_cli/inbox/`) is a Zendesk inbox browser**, not an investigation surface. It is the right UI for the watcher (queue review), but if it becomes the only TUI, the product reads as "inbox-shaped." That is the brief's concern, and it is valid — but the response is not to remove it; it is to add a guided-investigation surface alongside it.

So the drift is real but narrower than the brief says: **the core path treats triage as a one-shot pipeline, and site/Datadog assumptions leak into ticket-only flows.** Everything else can be preserved with surgical changes.

---

## 2. Pushback on the brief

The brief is mostly right. These are the points where I think it is over-prescriptive or actively wrong, with the recommended adjustment.

### 2.1 The proposed `Assessment` model is redundant with `TriageReport`

The brief proposes:

```python
class Assessment(BaseModel):
    summary: str
    likely_root_cause: str
    confidence: Literal["low", "medium", "high"]
    correlation: list[str]
    unknowns: list[str]
    next_steps: list[str]
    suggested_internal_note: str
```

The existing `TriageReport` (via `LLMTriageOutput`, `models.py:158-177`) already has `finding`, `confidence`, `evidence`, `suggested_note`, `next_checks`, `unknowns`. The only genuinely new fields are `summary` and `correlation`.

**Don't fork the schema.** Add `summary: str | None` and `correlation: list[str]` (both optional, default empty) to `LLMTriageOutput`. Update the system prompt to ask for them. Keep `TriageReport` as the single artifact. Two near-identical models with different field names is a mistake we will pay for every time someone has to remember which one is the "real" one.

### 2.2 `InvestigationEvidence` with parallel typed lists is awkward

The brief proposes separate `attachments`, `local_files`, `pasted_logs`, `optional_sources` lists. This makes "iterate over all evidence" require visiting four lists, and adding a fifth source type requires schema changes in two places.

**Use a single `evidence: list[EvidenceSource]` with a `kind` discriminator.** Pydantic v2 handles tagged unions cleanly, but for v1 of this we don't even need a discriminated union — a flat `EvidenceSource(kind, label, source_ref, ...)` model is enough. Adding a new source type (Datadog, syslog format, future formats) is then one enum value plus one ingest function, no schema fork.

### 2.3 Comments-as-evidence will double-count without care

The brief says "include ticket comments as evidence" while also keeping `Ticket.comments`. If we do both naively, the LLM sees comments twice: once in the ticket section and again in the timeline.

**Pick one.** My recommendation: the LLM input is `(ticket header) + (unified timeline)`. The ticket header is subject, requester, tags, created_at — no comment bodies. Comments become `TimelineEvent`s with `kind="zendesk_comment"`. Same for log lines. This makes correlation a deterministic, sorted merge instead of asking the LLM to interleave streams it has been handed separately.

### 2.4 `TimelineEvent` should subsume `LogLine`, not co-exist with it

We currently have `LogLine` (timestamp, level, message, attributes). The brief proposes `TimelineEvent` (timestamp | None, source, kind, message, raw_ref). Letting both exist creates a conversion layer for no payoff.

**Make `TimelineEvent` the canonical shape** and use it for Datadog log lines too. Keep `LogLine` as a temporary alias if anything imports it (it doesn't, outside `models` / `pipeline` / `datadog`); simpler to delete it once Datadog ingestion is rewritten to emit `TimelineEvent` directly. For this slice, I'll keep `LogLine` intact (legacy `triage_one` still uses it) and add `TimelineEvent` as the new shape; we can collapse them in a follow-up once both paths work.

### 2.5 The branch name in the brief is wrong

The brief says `git checkout -b product/guided-investigation-reset`. The system instructions for this session pin work to `claude/refocus-triage-cli-ErrxO`. I'm using the system-instructed branch. If you wanted the branch renamed, say so explicitly — I'm not going to override the harness on my own.

### 2.6 "Iterative investigation" is in tension with one-shot LLM calls

The brief reads sequentially: ingest → parse → correlate → assess → done. Real investigators want to add evidence and re-assess. I am not building re-assessment into v1, but the design must not foreclose it: `InvestigationSession` is mutable state; `run_assessment(session)` is a function the caller can call as many times as it wants, each call producing a fresh `TriageReport`. The CLI flow this pass is single-shot for simplicity, but the data model supports the loop.

### 2.7 Attachments are a real privacy boundary

The codebase doesn't fetch attachment bytes today (`zendesk.py` reads only ticket meta + comment bodies). NG911 attachments can include call recordings, CAD exports, screenshots with PII. Sending those to the LLM is a different privacy class than sending internal comments. **For this pass, I am adding the `EvidenceSource` metadata path only.** Actual attachment download + parsing is a follow-up that needs an explicit per-attachment opt-in step (not "ingest all attachments by default"), and probably a binary-vs-text gate. The brief allows this — "If Zendesk attachment downloading is not currently implemented, do not fake it."

### 2.8 The TUI in the brief is a future state

The brief shows a three-pane Textual workspace. Building that this pass is too much. The brief itself says "If a full TUI is too large for this pass, implement the command and service layer first." I'm doing exactly that: terminal-prompt guided flow now (typer prompts, no Textual), three-pane workspace later. The domain models and orchestration are designed so the TUI is a thin presentation layer over the same `InvestigationSession`.

### 2.9 The inbox TUI should be left alone, not rebranded

The brief is silent on the existing `triage_cli/inbox/` package. The instinct will be to either delete it or repurpose it. **Do neither.** It is the watcher's UI for reviewing the queue of reports the watcher has already produced. That is a legitimate, narrow role. The new `investigate` command is a peer entry point, not a replacement. The README will reframe both as siblings under the two-pillar story.

### 2.10 "Daily-use path supports Zendesk ticket only" is fine but warrants a confidence floor

A triage with no logs, no attachments, no pasted text is just "an LLM hot take on a ticket subject." That is genuinely useful for a 30-second smell test, but the LLM should be biased toward `confidence: low` in that case. The existing `Confidence calibration` rules in the system prompt already say "low: logs absent, ambiguous, or contradict the ticket" — the new prompt should preserve that rule and extend it: "low when no evidence beyond the ticket itself was provided."

---

## 3. Which recent changes are valuable and should be preserved?

Keep, no changes:

- `TriageReport` model and its `LLMTriageOutput` base. (Add fields; do not replace.)
- `render.save_note` (markdown + JSON dual write, collision-safe filenames). It is exactly the persistence contract we want.
- `render.print_note` and the `to_markdown` / `rich_layout` split — the TTY-vs-pipe branch is right.
- Watcher (`triage_cli/watcher.py`): state file format, `should_triage` decider, atomic save, prune cap.
- `cli.triage` non-interactive one-shot — gets renamed-in-spirit to "fast path" in docs but the command stays.
- `cli.build-map` and the underlying script.
- The Claude Agent SDK choice. Don't switch to the Anthropic HTTP SDK. (CLAUDE.md is explicit; the brief tries not to touch this and shouldn't.)
- The inbox TUI — leave it untouched. It is the watcher's review surface.
- Site map data files and the `extract.lookup_site` resolution priority. (Site becomes optional metadata, not removed.)

---

## 4. Which pieces should be demoted or moved behind optional interfaces?

- **Site/CNC resolution in the guided flow.** Skip silently unless the user picks Datadog as evidence. No interactive prompt. (`cli.triage` keeps its prompt for backward compat.)
- **Datadog query in the guided flow.** Becomes one of several evidence-source pickers. Same `DatadogClient` under the hood.
- **Anchor extraction.** Currently runs whenever Datadog is enabled. In the guided flow it runs only when Datadog evidence is requested.
- **The `TriageBundle` model.** Stays for the legacy `triage` command. The new flow uses `InvestigationSession` and a different prompt builder. We can collapse the two later when we are confident.
- **`station_tag` / station-level Datadog query.** Already not used; stays in `.env.example` as a v2 reservation.
- **Rich rendering as the "default."** Already TTY-conditional; just clarify in docs. Markdown is the canonical artifact.

---

## 5. What should the new core pipeline be?

Two pillars, parallel CLI commands, one shared report artifact.

```
Pillar 1: Guided Investigation
  triage-cli investigate <ticket-id-or-url>
    1. Parse ticket id/url
    2. Fetch ticket (subject, description, comments, attachment manifest)
    3. Initial summary (printed)
    4. Loop: pick evidence source
         [a] Zendesk attachments  (metadata only, this pass)
         [f] Local file
         [d] Local directory
         [p] Paste text
         [g] Datadog query (skipped if no creds / no site resolved)
         [s] Done, run assessment
    5. Build deterministic timeline (merge ticket events + parsed log events,
       sort by timestamp, unparsed bag stays as a per-source attachment)
    6. Run assessment (single LLM call over: ticket header + manifest + timeline)
    7. Render TriageReport, offer to save markdown + JSON

Pillar 2: Automated Watcher  (unchanged)
  triage-cli watch --view <id>
    Same loop as today, calling pipeline.triage_one (legacy fast path).
    Saves reports to disk; inbox TUI rehydrates them.

Fast path (preserved)
  triage-cli triage <ticket-id-or-url>
    Existing one-shot non-interactive command. Untouched in this pass.
    Kept for scripting, watcher reuse, and "I just want a quick read."

Maintenance
  triage-cli build-map        unchanged
  triage-cli inbox            unchanged (watcher review surface)
```

Datadog stops being "the spine" because the spine is now the timeline, and Datadog is one of N sources that can feed it.

---

## 6. The smallest implementation slice

Goal: ship the two-pillar story with working code, no fake features, no half-implementations. TUI deferred.

### 6.1 Domain models (additions only; nothing removed this pass)

In `triage_cli/models.py`:
- Add `summary: str | None = None` and `correlation: list[str] = []` to `LLMTriageOutput`. Optional so old saved JSON still loads.

In a new `triage_cli/timeline.py`:
- `TimelineEvent` (timestamp `datetime | None`, source `str`, kind `str`, level `str | None`, message `str`, attributes `dict[str, Any]`).
- `parse_lines(text, source) -> tuple[list[TimelineEvent], int unparsed_count]`. Tier 1: ISO-8601 prefix regex. Tier 2: JSON line with `timestamp`/`@timestamp`/`time`. Tier 3: line is unparsed, counted but not emitted as an event.
- `merge(streams) -> list[TimelineEvent]`. Sort by timestamp; events with `timestamp=None` go to the end.

In a new `triage_cli/evidence.py`:
- `EvidenceKind` StrEnum: `zendesk_ticket`, `zendesk_comment`, `zendesk_attachment`, `local_file`, `local_directory`, `pasted_text`, `datadog_query`.
- `EvidenceSource` model (kind, label, source_ref, parsed bool, event_count int, truncated bool, notes `str | None`). One model, discriminated by `kind`.
- `from_local_file(path) -> tuple[EvidenceSource, list[TimelineEvent]]`.
- `from_local_directory(path, glob='*.log') -> tuple[EvidenceSource, list[TimelineEvent]]` (each matching file ingested; directory becomes one source with combined event count).
- `from_pasted_text(text, label) -> tuple[EvidenceSource, list[TimelineEvent]]`.
- `from_zendesk_attachment_metadata(comment, attachment_dict) -> EvidenceSource` — metadata only; no bytes downloaded; mark `parsed=False`, `notes="binary download not yet implemented"`.

In a new `triage_cli/investigation.py`:
- `InvestigationSession` (ticket, sources `list[EvidenceSource]`, timeline `list[TimelineEvent]`, report `TriageReport | None`).
- `add_source(session, source, events) -> None`.
- `to_assessment_prompt(session) -> str`. Renders ticket header (no comment bodies), source manifest, timeline, and instructs the LLM to produce the extended `LLMTriageOutput` schema.
- `run_assessment(session, *, verbose, show_spinner) -> TriageReport`. Calls a new `llm.assess(session)` and wraps the result in `TriageReport`.

In `triage_cli/llm.py`:
- Add `ASSESSMENT_SYSTEM_PROMPT` (variant of `TRIAGE_SYSTEM_PROMPT`, asking for `summary` and `correlation` and dropping the "Datadog log window" framing).
- Add `async assess(session: InvestigationSession, ...) -> LLMTriageOutput`. Mirrors `triage()`'s retry/JSON-fence-strip behavior.

In `triage_cli/cli.py`:
- Add `investigate <ticket>` command. Terminal-prompt loop. No Textual yet.
- Existing `triage`, `watch`, `inbox`, `build-map` untouched.

In `triage_cli/render.py`:
- Render `summary` (above Finding) and `correlation` (between Evidence and Next Checks) when present.

### 6.2 Tests

- `tests/test_timeline.py`: ISO 8601 parsing, JSON-line parsing, mixed-stream merge, unparsed counter.
- `tests/test_evidence.py`: local file ingestion, pasted text, attachment metadata stub.
- `tests/test_investigation.py`: `add_source` accumulates, `to_assessment_prompt` emits manifest + timeline, `run_assessment` (with mocked `llm.assess`) returns a `TriageReport` with the new fields.
- Existing tests stay green. No changes to `test_pipeline.py`, `test_inbox_app.py`, `test_watcher.py`.

### 6.3 Docs

- `README.md`: rewrite the "What this is" lede around the two pillars; reorganize Usage with `investigate` as the primary command and `triage` as the fast-path alias; demote Datadog setup to "optional enrichment" but keep the env vars documented.
- This file (`docs/product-direction-review.md`) committed as the audit trail.

### 6.4 Out of scope for this pass

- Three-pane Textual TUI for guided investigation.
- Actual Zendesk attachment download and binary parsing.
- Re-running assessment with revised evidence (data model supports it; CLI does not yet expose it).
- Migrating `LogLine` → `TimelineEvent` in the legacy `triage_one` path.
- Posting back to Zendesk.
- Site/CNC removal from the legacy `triage` command (left alone; only the new flow skips it).

---

## 7. Acceptance check

- [x] Adversarial review committed in `docs/`.
- [ ] `EvidenceSource`, `TimelineEvent`, `InvestigationSession` in code.
- [ ] `triage-cli investigate <ticket>` runs end-to-end, terminal-prompt guided.
- [ ] Datadog no longer required for the primary mental model (this file + README).
- [ ] `triage-cli triage <ticket>` still works.
- [ ] `triage-cli watch --view <id>` still works.
- [ ] New tests pass; existing tests pass; `ruff check .` clean.
- [ ] README updated to describe both pillars and Datadog as optional.

The implementation in this branch addresses the unchecked items.
