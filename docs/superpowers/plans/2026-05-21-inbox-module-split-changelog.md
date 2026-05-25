# Inbox Module Split PR Changelog

Ongoing notes for the PR that addresses the inbox TUI ownership regression.

## 2026-05-21

- Split the monolithic `triage-cli-rs/src/tui/inbox.rs` into `tui/inbox/` modules: event loop, app state/actions, polling/triage orchestration, STATE.md parsing, render helpers, and chat session logic now live in separate files.
- Removed the inbox-only `PhaseReporter`; inbox pipeline progress now flows through `pipeline::ChannelReporter` and is translated into inbox row progress events in `poll.rs`.
- Preserved the existing public helpers used by tests and callers through `tui::inbox` re-exports.
