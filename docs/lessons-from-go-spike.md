# Lessons from the Go spike

**Status:** archived; see tag `archive/go-spike` for the source tree.
**Date:** 2026-05-08
**Outcome:** stay on Python; port a small set of ideas back here.

## Why the spike happened

Onboarding friction. Operators install Python 3.11, create a venv,
`pip install -e ".[dev]"`, and authenticate the `claude` CLI before they can
triage one ticket. A Go binary plus the `claude` CLI removes the first three
steps. The spike asked whether single-binary distribution was worth a rewrite.

Conclusion: not yet. The install pain is real but solvable with packaging
(pipx, a wrapper script, or a small installer) without abandoning Python's
ecosystem advantages — Pydantic, Textual, Rich, the Agent SDK, and ~150 tests
of behavioral coverage that would have to be re-earned in Go.

The spike is being archived (tag `archive/go-spike`) rather than deleted so the
domain model and prompt work remain recoverable.

## Ideas worth porting back

### 1. `--no-llm` deterministic stub assessor

The Go spike ships a `StubAssessor` that produces a plausible `Assessment`
from the timeline alone — confidence is honestly downgraded based on evidence
count. It's used for CI, offline development, and as the automatic fallback
when the `claude` CLI is missing on `PATH`.

Python has nothing equivalent. Tests monkeypatch the Agent SDK call, but a
developer running `triage-cli triage 12345 --mock` without an authenticated
`claude` session today gets a hard error rather than a useful local run.

**Action:** add `--no-llm` to the `triage` and `investigate` commands; route
to a deterministic stub that fills `TriageReport`/`Assessment` from the
bundle without an LLM call. Auto-fall-back when the Agent SDK raises a "no
authenticated session" error, with a stderr warning.

### 2. `doctor` subcommand

The Go branch's `doctor` checks the Zendesk env vars, output dir
writability, watcher state dir writability, optional Datadog config, and
performs a 5-second `GET /users/me.json` Zendesk probe and a `claude
--version` PATH probe. One screen of output, one command, no guesswork
during onboarding.

Python should have the same. Most of the checks are one-liners; the value
is having them in one place.

**Action:** add `triage-cli doctor` that prints a checklist of env, paths,
and CLI dependencies. Exit 0 on all-green, 1 on any critical failure;
warnings (e.g. missing Datadog config) don't fail.

### 3. Honest-confidence enum on `Assessment`

The Go `Assessment.Confidence` is enum-validated (`high`, `likely`,
`unknown`). The prompt explicitly tells the model to say `unknown` when
evidence is thin and includes a thin-evidence example. The Python prompt
allows this but doesn't enforce it; nothing rejects a `TriageReport` whose
confidence is a free-form string.

**Action:** tighten the Pydantic field on `TriageReport` (or
`InvestigationSession.Assessment` if/when it lands) to a
`Literal["high", "likely", "unknown"]`. Update the prompt to mirror the Go
branch's calibration language.

### 4. Polymorphic evidence as the core abstraction

Already absorbed into `triage_cli/investigation.py` and the
`InvestigationEvidence` model — comments, attachments, local files, and
pasted text all normalize to `TimelineEvent`s. The Go branch validated the
shape independently; treat that as confirmation that the
`product-direction-review.md` direction was right and keep going. No new
action needed.

### 5. Investigation-progress TUI (deferred)

The Go branch's `--tui` flag opens a three-pane Bubble Tea progress view for
a single investigation: workflow rail (left), active step detail (top
right), timeline (bottom). It addresses a real Python gap — long LLM calls
feel like a hang in the linear stderr flow — but isn't urgent.

Captured as a Python-side spec in `docs/superpowers/specs/2026-05-08-investigation-progress-tui.md`.
Build it when operators complain about the linear flow, not before.

## Ideas not worth porting

- **Cobra command structure.** Typer is fine.
- **Bubble Tea source.** Textual is the Python-side equivalent.
- **Mock fetcher pattern.** Python tests already monkeypatch effectively;
  no architectural change needed.
- **`skills/` directory and SKILL.md packaging.** Operator guidance lives
  in `docs/runbooks/` and that's working.
- **Subprocess-the-claude-CLI assessor.** The Agent SDK already does this
  under the hood with better error handling.
- **Three TUIs in one tool.** Python has the Textual inbox; that's enough
  TUI surface. The investigation-progress TUI is opt-in and additive.

## What the spike didn't solve

- **Distribution.** Python `pip install -e` is still the install path.
  Worth a follow-up: pipx packaging or a curl-to-bash wrapper that creates
  a venv, installs, and runs `doctor`.
- **Datadog client port.** The Go branch deferred it; Python's is the only
  working implementation.
- **CNC/site map relevance.** The product-direction-review demoted this;
  whether it stays or goes is still an open product question.

## Recovery

```bash
git checkout tags/archive/go-spike
```

The full Go tree, including `triage-cli-go/HANDOFF_GO_SPIKE.md` and
`docs/go-spike-notes.md`, lives at that tag. The remote branch
`origin/triage-cli-go` was deleted on 2026-05-08 once this doc was merged.
