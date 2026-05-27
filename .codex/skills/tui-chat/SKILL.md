---
name: tui-chat
description: Use when changing the inbox TUI, chat surface, conversation logs, session manifests, attachments, progress banners, or follow-up turns.
---

# TUI and chat skill

Load this skill when a task touches the inbox UI or the conversational follow-up surface.

## Core doctrine

The TUI is a local operator cockpit over ticket-folder artifacts. Chat is a reviewable session log plus provider follow-up, not an invisible mutation path.

## Stable invariants

- `inbox` requires a TTY and owns the terminal. Diagnostics go to a log file, not into the TUI surface.
- The default detail pane is a synthesized summary from `STATE.md`; tabbed file views show the five markdown artifacts.
- `CONVERSATION.jsonl` is the source of truth for turns. `CONVERSATION.md` is derived and human-readable only.
- Per-ticket session files live under `<ticket_dir>/.session/`.
- Base ticket and base evidence snapshots are durable context for `/revise` and session-loss recovery; never overwrite original snapshots during follow-up mode.
- Attached files and pastes must carry provenance: basename/label, byte count, sha256 for files, extraction status, truncation flag, and whether it was sent to the provider.
- Provider prompts for follow-ups should include bounded, PII-redacted ticket context. Do not dump entire ticket folders or unbounded attachments into a prompt.

## Common files

- `triage-cli-rs/src/tui/inbox.rs`
- `triage-cli-rs/src/tui/chat.rs`
- `triage-cli-rs/src/chat.rs`
- `triage-cli-rs/src/pipeline/followup*.rs`
- `triage-cli-rs/src/providers/**`
- `triage-cli-rs/tests/integration/**`

## Test evidence to require

- For UI-state changes, test the rendered state, keybinding result, or output artifact the operator sees.
- For chat changes, assert exact JSONL turn fields, rendered `CONVERSATION.md` text, session manifest fields, or `.session/base-evidence-manifest.json` content.
- For attachment changes, assert provenance details and truncation/extraction flags.
- For provider-context changes, assert prompt caps and included/excluded snippets rather than relying on provider behavior.

## Keep this skill current

Update this file when changing keybindings, chat event schema, session manifest schema, attachment rules, context preamble caps, or follow-up/revise behavior.
