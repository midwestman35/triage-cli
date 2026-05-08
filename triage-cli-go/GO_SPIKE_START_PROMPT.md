# Short Kickoff Prompt for Orchestrating Agent

You are starting a Go spike branch for `triage-cli`.

Read `HANDOFF.md` and `GO_CONVERSATION_CONTEXT.md` first.

Your mission is to evaluate and begin a from-scratch Go rewrite of `triage-cli` as a polished CLI-first guided investigation tool.

Do not blindly port the Python repo. Use Python as the behavioral reference only.

Core product pillars:

1. `triage-cli investigate <ticket>` — guided Zendesk ticket investigation.
2. `triage-cli watch --view <id>` — automated watcher remains a mainstay feature.

Datadog is optional enrichment, not the app spine.

Start by writing `docs/go-spike-notes.md` with:
- what you are preserving from Python
- what you are redesigning
- what is deferred
- proposed Go package structure
- overnight implementation plan

Then implement the smallest useful Go spike:

```txt
1. Go module skeleton
2. Cobra CLI root
3. Commands: investigate, triage, watch, doctor, version
4. Core investigation/evidence/timeline/assessment models
5. Mock guided investigation flow
6. Markdown/JSON artifact output
7. Basic tests
```

Success target:

```bash
go run ./cmd/triage-cli investigate 12345 --mock
```

should produce a readable guided triage flow and save Markdown/JSON artifacts.

Use subagents if available:
- Architecture
- CLI
- Domain Models
- Evidence
- Zendesk
- Assessment
- Render/Artifacts
- Watcher
- QA

Avoid overbuilding the full TUI overnight. Build a clean foundation for it.
