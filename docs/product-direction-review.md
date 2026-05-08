# Product Direction Review

Note: this is the pre-implementation adversarial review used to guide the
guided-investigation reset. Statements below about the "current implementation"
refer to the state before the guided-investigation changes in this branch.

Source basis: `codex-handoff.rtf`, plus a targeted read of the current CLI,
pipeline, models, Zendesk, Datadog, renderer, watcher, and inbox code.

This is an adversarial review, not a teardown. The current repo has useful
production pieces. The problem is that the implementation now implies the wrong
daily workflow: it treats the product as a report generator driven by site
resolution and Datadog logs, with Rich output and inbox/watch views wrapped
around that report. The intended workflow is a guided investigation that starts
from a Zendesk ticket and builds an evidence-backed handoff.

## 1. Where The Current Implementation Has Drifted

### The spine is still site lookup -> Datadog window -> LLM report

The main `triage` command parses a ticket, fetches Zendesk, loads the CNC/site
map, resolves a `SiteEntry`, optionally queries Datadog, then asks the LLM to
return a `TriageReport`. That is operational, but it is not yet a guided
investigation workflow.

The current `pipeline.triage_one` signature requires a resolved `SiteEntry`.
Even when `dd_client=None`, the caller must still provide `site_entry`. This
means ticket-only triage is not truly ticket-only. The user can pass
`--no-logs`, but the command still loads `data/cnc-map.json`, still resolves a
site, and still prompts for `site_name` when resolution fails. That is the
clearest product drift: Datadog can be skipped technically, but Datadog-shaped
metadata remains structurally required.

For a support engineer, that creates the wrong failure mode. A ticket with good
comments and useful attached logs should be investigable even if the customer
site is unknown, the CNC map is stale, or Datadog credentials are absent. Today,
those conditions block or distort the workflow before evidence intake has even
started.

### The LLM contract is report-first, not investigation-first

The LLM prompt describes the input as a Zendesk ticket plus "a window of
Datadog logs from the affected customer." It returns a compact JSON object for a
final `TriageReport`: finding, confidence, evidence, suggested note, next
checks, and unknowns.

That output is worth preserving, but it skips the intermediate product state the
handoff is asking for: what evidence exists, what was reviewed, what was
ignored, what was missing, what timeline was built, and what correlations were
made. The current model has `Ticket`, `Comment`, `LogLine`, `TriageBundle`, and
`TriageReport`, but no investigation session, evidence inventory, attachment
evidence, pasted evidence, local file evidence, timeline events, or assessment
object.

The product therefore has a final artifact, but not the workbench that produces
it.

### Evidence intake is not first-class

Zendesk ticket body and comments are fetched. Datadog logs are fetched. There is
no current first-class path for Zendesk attachment metadata, attachment
download, local log files, local directories, or pasted logs.

The Zendesk client fetches ticket metadata and comments. It does not expose
comment attachment metadata. The models do not represent attachments or local
evidence. The CLI does not prompt for evidence. The TUI/inbox does not provide
an evidence intake step. As a result, the app cannot yet support the desired
daily path:

1. Load ticket.
2. Review description and comments.
3. Detect attachments.
4. Ask for local or pasted evidence.
5. Normalize evidence.
6. Build a timeline.
7. Correlate signals.
8. Produce assessment and handoff.

Instead, the current path asks: can we resolve the site, pick an anchor, query
logs, and generate a note?

### Rich rendering and inbox UI are downstream of the wrong object

The Rich renderer is built around `TriageReport`, and the inbox TUI hydrates
saved `TriageReport` JSON sidecars and renders the selected report. That makes
sense for a report viewer. It does not make the inbox a guided investigation
workspace.

The inbox code is useful as a watcher/report surface, but it should not become
the primary product direction yet. It does not show investigation state,
evidence sources, missing evidence, correlation steps, or a workflow rail. It is
currently a view over saved reports plus a way to trigger the same pipeline for
queued tickets.

That is fine as an auxiliary operator tool. It is not the core guided
investigation product.

### Watch mode is valuable, but it reinforces the same pipeline assumption

The watcher polls Zendesk views, decides whether a ticket changed, resolves the
site, runs `pipeline.triage_one`, saves markdown/JSON, and tracks state. This is
a good production feature. The drift is that watcher and inbox now reuse the
same report-first pipeline, so any product investment there deepens the current
mental model unless the core pipeline is corrected first.

## 2. Valuable Recent Changes To Preserve

Preserve these pieces. They are not the problem.

### `TriageReport` and structured JSON output

The structured output contract is useful. Support and NOC workflows need
repeatable fields, not just prose. `TriageReport` should remain the saved final
artifact and compatibility format for one-shot triage, watcher output, inbox
hydration, and downstream automation.

Do not delete it. Generate it from an investigation session.

### Markdown and JSON save behavior

The paired markdown/JSON save behavior is practical. Markdown is pasteable and
human-readable. JSON is machine-readable and already powers inbox hydration.
Keep both.

### Rich one-shot rendering

The Rich layout is useful for terminal users who want a quick read. Keep it for
`triage-cli triage <ticket>` and for report viewing surfaces. Do not let Rich
layout decisions drive the domain model.

### Zendesk ticket ID/URL parsing and ticket/comment fetching

The current Zendesk read path is a strong base: ticket ID parsing supports raw
IDs and URLs, and the Zendesk client fetches the ticket plus full chronological
comment thread. That should become the first stage of guided investigation.

### Watch mode

Watcher mode solves a real operational need: poll a Zendesk view, triage changed
tickets, save artifacts, and maintain state. Keep it as a first-class product
pillar, but make it a consumer of the corrected investigation/report pipeline
rather than the force that defines the core workflow.

### Site/CNC mapping

Site/CNC mapping is valuable metadata. It helps find customer context and can
power Datadog enrichment. It should not block a ticket-only investigation.

### `DatadogClient`

The Datadog client is useful and reasonably contained. Keep it as an optional
evidence source. Its query validation and log normalization are good pieces. The
problem is not the client; the problem is where it sits in the product.

### Anchor/window logic

Anchor extraction and log windows are useful when Datadog is used. Keep them
behind optional Datadog evidence enrichment. They should not be required to
build an assessment from ticket/comments/attachments/local logs.

## 3. Pieces To Demote Or Move Behind Optional Interfaces

### Datadog

Demote Datadog from pipeline spine to optional evidence provider.

The core guided path must work with:

- Zendesk ticket only.
- Zendesk ticket plus comments.
- Zendesk ticket plus attachments.
- Zendesk ticket plus local logs.
- Zendesk ticket plus pasted logs.
- Any of the above plus optional Datadog logs.

Datadog-specific failures should be warnings or evidence-source failures, not
investigation failures, unless the user explicitly requested Datadog-only
enrichment.

### CNC/site lookup

Move CNC/site lookup behind an enrichment interface. It can add customer
metadata and may enable Datadog queries. It should not be required before the
app can produce a ticket assessment or internal note.

The current CLI behavior of prompting for `site_name` even when `--no-logs` is
the wrong operator experience. If no Datadog query will run, missing site data
should be recorded as an unknown, not requested as a blocking field.

### Station-level query planning

Do not build this into core guided investigation. It is a later Datadog
enrichment. It depends on having enough structured station identifiers and
knowing which Datadog tags are trustworthy. Building it now would deepen the
Datadog-first shape.

### Call-center log windows

Keep call-center log windows as Datadog query mechanics. Do not make them the
universal incident model. The investigation timeline should be evidence-based:
ticket creation, comments, attachment timestamps when available, log timestamps,
and user-provided context. A Datadog window is one evidence slice, not the
definition of the incident.

### Inbox TUI

Demote inbox from product direction to useful watcher/report UI. Do not delete
it. Do not expand it into the main TUI yet. A guided investigation workspace
needs different state and controls than an inbox/report viewer.

### Rich rendering

Keep Rich rendering as presentation. Do not add domain concepts because they
look good in panels. The domain objects should be evidence, timeline,
assessment, and handoff.

## 4. Challenge The Handoff Before Implementing

Most of the handoff is directionally right. A few points need tightening before
code changes.

### The repo already has a partial Datadog-optional path

The handoff says Datadog should be optional. The code already has `--no-logs`
and `dd_client=None` handling. That is worth acknowledging. The real defect is
not simply "Datadog is required"; it is that the ticket-only path still depends
on site resolution and a `SiteEntry`, and the LLM prompt/schema remain
Datadog-window-oriented.

Implementation should not re-solve optional Datadog from scratch. It should
remove Datadog-shaped requirements from the core data flow.

### Do not over-index on a TUI yet

The handoff includes a desired three-pane TUI. That may be the right eventual
shape, but building it before the domain model exists would repeat the current
drift: UI first, investigation state later. The smallest useful correction is a
service/domain layer and a minimal terminal flow, not a new Textual workspace.

### Do not pretend attachment ingestion is implemented

The handoff correctly says not to fake attachment downloading. The current
Zendesk code does not expose attachment metadata. The first pass should model
attachment evidence and collect metadata when available. Downloading,
extracting, and parsing attachment contents can follow.

### Do not delete the report path

The handoff asks to preserve `TriageReport`, one-shot rendering, and watcher.
That matters. The repo should evolve by adding an investigation layer beneath
the report, not by replacing working production behavior with a large new
surface.

## 5. New Core Pipeline

The new primary path should be `triage-cli investigate <ticket>`.

Core pipeline:

1. Parse Zendesk ticket number or URL.
2. Fetch Zendesk ticket and full comments.
3. Create an `InvestigationSession`.
4. Add ticket description and comments as baseline evidence.
5. Detect Zendesk attachment metadata where available.
6. Prompt for optional evidence:
   - ingest Zendesk attachments, when implemented;
   - add local file;
   - add local directory;
   - paste logs;
   - skip.
7. Normalize evidence into a common evidence model.
8. Build a timeline from ticket events, comments, and any timestamped evidence.
9. Optionally enrich:
   - site/CNC lookup;
   - Datadog logs;
   - anchor/window extraction;
   - later station-level queries.
10. Correlate symptoms, comments, timestamps, and evidence.
11. Produce an `Assessment`:
   - summary;
   - likely root cause;
   - confidence;
   - correlation;
   - unknowns/gaps;
   - next steps;
   - suggested internal note.
12. Generate a `TriageReport` from the session for compatibility.
13. Save markdown and JSON artifacts.

Command split:

- `triage-cli investigate <ticket>`: primary guided investigation. Minimal
  first version can be a guided terminal flow.
- `triage-cli triage <ticket>`: fast one-shot report. It should continue to
  work and can internally use the new investigation service once stable.
- `triage-cli watch --view <id>`: automated watcher. Keep as a first-class
  production mode.
- `triage-cli build-map`: optional map maintenance.

The core object should be `InvestigationSession`, not `TriageBundle`.
`TriageBundle` can remain as the compatibility structure for the current LLM
report path until the new assessment prompt lands.

## 6. Smallest Useful Implementation Slice

The smallest slice that moves the repo in the right direction is not a full TUI
and not a broad command rewrite. It is a narrow domain-and-command correction.

### Slice A: Add investigation models

Add models for:

- `InvestigationSession`
- `InvestigationEvidence`
- `AttachmentEvidence`
- `LocalFileEvidence`
- `PastedEvidence`
- `TimelineEvent`
- `Assessment`

Keep `TriageReport`. Make the new models additive so existing tests and saved
report behavior do not break.

### Slice B: Add a guided investigation service

Add a small service that accepts a fetched `Ticket` and creates an
`InvestigationSession`:

- ticket description becomes evidence;
- comments become evidence;
- attachment metadata path exists but does not claim to download content;
- local file paths can be registered as evidence;
- pasted text can be registered as evidence;
- a simple timeline is built from ticket creation and comments;
- a basic assessment can be generated from the available evidence;
- a `TriageReport` can still be produced for current render/save behavior.

This service should not require `SiteEntry` or `DatadogClient`.

### Slice C: Add `triage-cli investigate <ticket>`

Implement a minimal guided terminal command:

1. Fetch the ticket.
2. Show a concise ticket/comment review.
3. Show detected evidence counts.
4. Ask whether to add a local file or pasted evidence.
5. Generate assessment/report.
6. Save markdown/JSON with the existing save path.

Do not build inbox TUI, Datadog-first investigation, station-level querying,
Slack notifications, posting back to Zendesk, multi-user backend,
daemon scheduler, or complex plugin architecture.

### Slice D: Loosen one-shot triage without breaking it

Keep `triage-cli triage <ticket>` working as the fast report path. The first
follow-up correction should allow `--no-logs` to avoid mandatory site-map
resolution. If no Datadog enrichment is requested, missing site data should be
captured as an unknown and the report should still be generated from Zendesk
content.

This is probably the highest-leverage behavioral fix after the models exist.

### Slice E: Leave watcher and inbox alone except for integration

Do not expand inbox. Do not redesign watcher. Once the investigation service is
stable, watcher can call the new one-shot path and still save `TriageReport`
artifacts. Until then, preserving watcher behavior is more valuable than
reshaping it.

## 7. Decision Summary

The repo should move from:

```text
ticket -> site lookup -> Datadog window -> LLM report -> Rich output
```

to:

```text
ticket -> investigation session -> evidence intake -> timeline/correlation
       -> assessment -> Zendesk-ready note/report
       -> optional enrichment from site map and Datadog
```

The current implementation has enough good infrastructure to support that
change. The mistake would be building more UI and Datadog-specific planning
before the investigation domain exists.

Recommended next implementation pass: add the investigation models and service,
wire a minimal `investigate` command, and make sure ticket-only investigation
does not require Datadog credentials, site-map presence, CNC metadata, or a
call-center log window.
