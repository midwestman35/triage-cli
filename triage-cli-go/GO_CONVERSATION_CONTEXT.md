# Go Spike Conversation Context — `triage-cli`

This document summarizes the Go-specific product and architecture conversation for the `triage-cli` rewrite spike.

It should be used as reference context by Claude Code, Codex, or another coding agent when working on the Go branch.

---

## 1. Where the project started

`triage-cli` started as a Python CLI for triaging Zendesk tickets related to Carbyne APEX NG911/E911 operations.

The original Python pipeline was roughly:

```txt
Zendesk ticket
→ requester/customer lookup
→ CNC/site map lookup
→ Datadog log query
→ Claude-generated triage note
→ Markdown output
```

The app currently has:

```txt
Python
Typer
Pydantic
Rich
Zendesk API client
Datadog API client
Claude Agent SDK usage
watch mode
markdown/json artifact output
```

The current repo is useful, but the product direction began drifting toward a report-first and Datadog-oriented flow.

---

## 2. Product correction

The core realization:

> Datadog should not be the spine of the app.

The more useful daily workflow is:

```txt
Zendesk ticket number or URL
→ guided triage and investigation
→ collect ticket context
→ ingest whatever evidence exists
→ correlate comments/logs/attachments
→ assess likely root cause
→ suggest next steps
→ generate Zendesk-ready internal note
```

The tool should still support Datadog later, but only as optional evidence enrichment.

---

## 3. Two first-class features

The app now has two first-class product pillars:

### Guided Investigation

A user provides a Zendesk ticket ID or URL.

The app guides them through:

```txt
1. Load ticket
2. Review description/comments
3. Inspect attachment metadata
4. Ingest local logs or pasted evidence
5. Parse evidence
6. Build a timeline
7. Correlate signals
8. Produce assessment
9. Suggest next steps
10. Generate internal note/handoff
```

### Automated Watcher

The existing watcher is still important.

It should remain a mainstay feature for polling Zendesk views and producing saved triage artifacts for new/updated tickets.

But the watcher/inbox should not drive the TUI design yet.

---

## 4. Why consider Go?

The user asked why we should stay in Python if nobody else is using the tool yet.

The answer: Python is fastest for validating the workflow, but Go may be the best long-term shape for a polished CLI.

Go benefits:

```txt
single binary
fast startup
simple distribution
great CLI ergonomics
excellent long-running watcher behavior
easy local file/state handling
clean package boundaries
good Bubble Tea TUI ecosystem
```

Python benefits:

```txt
fastest iteration
existing code already works
great parsing and LLM orchestration
Pydantic models
Textual/Rich
```

TypeScript was considered but ranked lower for this specific tool. It is more compelling if MCP/web/agent ecosystem becomes central, but less compelling for a local-first operator CLI.

Recommendation from the conversation:

```txt
This weekend:
- Keep Python as behavioral reference.
- Start a Go spike branch.
- Evaluate whether Go feels better as the final product shell.
```

---

## 5. Printing Press Library inspiration

The user referenced:

```txt
https://github.com/mvanhorn/printing-press-library
```

The relevant mindset from that repo:

```txt
polished CLI tools
single-purpose commands
local README/SKILL docs
agent-readable output
human-readable output
single-binary ergonomics
focused tools that become muscle memory
```

We should not copy the repo wholesale, but should borrow the model:

```txt
self-contained CLI
focused commands
local docs
agent skills
local artifacts/state
JSON + Markdown output
```

---

## 6. Desired command shape

Suggested long-term command structure:

```bash
triage-cli investigate <ticket>
triage-cli triage <ticket>
triage-cli watch --view <id>
triage-cli doctor
triage-cli version
triage-cli build-map    # optional, if site/CNC map remains
```

### `investigate`

Primary guided flow.

### `triage`

Fast one-shot non-interactive report.

### `watch`

Poll Zendesk view and save triage artifacts.

### `doctor`

Check config/environment.

### `version`

Print build/version information.

---

## 7. Desired guided flow

The daily-use path:

```txt
User provides ticket number or Zendesk URL.
Tool fetches ticket context.
Tool shows initial summary.
Tool reviews comments.
Tool identifies attachments.
Tool asks user for logs/evidence.
Tool ingests local files, directories, pasted logs, or attachments.
Tool builds timeline.
Tool generates assessment, correlation, likely root cause, next steps.
Tool generates internal note.
Tool saves markdown/json artifact.
```

The app should be useful even with:

```txt
Zendesk ticket only
Zendesk ticket + comments
Zendesk ticket + attachments
Zendesk ticket + local logs
Zendesk ticket + pasted logs
```

No Datadog credentials should be required.

---

## 8. Desired TUI direction

Eventually, use a three-pane guided workspace:

```txt
┌─ triage-cli · ZD-12345 · Guided Investigation ─────────────────────────────┐
│ Status: Evidence gathering · Sources: Zendesk, attachments, local logs      │
├──────────────────────────┬────────────────────────────────────────────────┤
│ Workflow                 │ Active Step                                    │
│                          │                                                │
│ ✓ Ticket loaded          │ Initial Assessment                             │
│ ✓ Comments reviewed      │                                                │
│ → Evidence intake        │ User reports intermittent audio loss.          │
│   Log parsing            │ Ticket contains 2 attachments and 4 comments.  │
│   Correlation            │                                                │
│   Assessment             │ Missing: workstation/station logs              │
│   Suggested next steps   │                                                │
│   Export note            │ [A] ingest attachments  [L] add local logs     │
├──────────────────────────┴────────────────────────────────────────────────┤
│ Evidence / Timeline                                                        │
│ 09:12  Ticket created: "Audio dropping from workstation"                   │
│ 09:18  Internal note: customer says issue began after reboot               │
│ 09:23  Attachment found: station_logs.zip                                  │
└────────────────────────────────────────────────────────────────────────────┘
```

But for the overnight spike, do not overfocus on a perfect TUI. A minimal guided terminal flow is enough.

---

## 9. What to avoid

Avoid making these the center of the rewrite:

```txt
Datadog-first investigation
station-level Datadog query planning
inbox-first TUI
Slack notifications
Zendesk note posting
multi-user backend
SQLite history
full MCP integration
perfect Bubble Tea app
```

Those may come later.

---

## 10. What to preserve from Python

Keep behaviorally:

```txt
ticket ID/URL parsing
Zendesk ticket fetch
comments as evidence
structured report generation
markdown/json artifact save
watcher mode concept
pipe-friendly output
optional Datadog
optional CNC/site metadata
```

Do not preserve Python implementation details just because they exist.

---

## 11. Expected Go spike result

The spike is successful if we can run:

```bash
go run ./cmd/triage-cli investigate 12345 --mock
```

and see:

```txt
ticket loaded
initial summary
evidence intake
timeline
assessment
saved markdown/json artifacts
```

Even if Zendesk is stubbed, the product flow should be clear.

---

## 12. Suggested package structure

```txt
cmd/triage-cli/main.go

internal/cli/
internal/config/
internal/zendesk/
internal/investigation/
internal/evidence/
internal/timeline/
internal/assessment/
internal/render/
internal/watcher/
internal/store/
internal/integrations/datadog/
internal/integrations/sitemap/

docs/
skills/
testdata/
```

---

## 13. Important mental model

The product is not a log searcher.

The product is a guided triage assistant.

It should answer:

```txt
What is the ticket reporting?
What evidence do we have?
What evidence is missing?
What patterns correlate?
What is the likely cause?
What should we do next?
What should the internal note say?
```

That is the north star.
