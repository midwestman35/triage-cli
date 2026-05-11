# ADR 0001: Split PR 9 and drop context/density scope

**Status:** accepted
**Date:** 2026-05-11

## Context

PR #9 combined several unrelated changes: LLM-boundary redaction, a token-aware
Datadog context builder, inbox-density UI polish, an LLM provider switch, and
packaged setup work. The current product direction demotes Datadog from the
investigation spine to optional enrichment and demotes the inbox from primary
surface to a viewer over saved reports.

The PR also carried two review-blocking hazards:

- The context scorer's duplicate penalty did not apply during candidate
  scoring, so a central claimed behavior did not run.
- `triage_cli/context.py` and `triage_cli/models.py` introduced an import cycle.

## Decision

Do not merge PR #9 as a bundle. Split the surviving work into smaller PRs and
drop the feature work tied to the old evidence and UI shapes.

Surviving work:

- LLM-boundary redactor, because Jira and Confluence enrichment increase prompt
  PII exposure.
- LLM provider protocol, because the tool needs a real provider abstraction
  rather than Unleash with one fallback.
- Packaged setup and doctor commands, because onboarding checks are orthogonal
  infrastructure.

Dropped work:

- Token-aware Datadog context builder and `ContextSummary`.
- Render elision footnote tied to that context builder.
- Inbox-density keybinding, state migration, and density-specific layouts.
- Final-phase spec and plan framing that mixed old scope with provider reversal.

## Consequences

The next relevance scorer should be rebuilt after the new evidence shapes are
known: prior tickets, Jira hits, Confluence runbooks, dropped log files, and
optional Datadog logs. The inbox should receive polish only after the saved
report viewer direction stabilizes.

The closed PR #9 remains available for archaeology; replacement PRs preserve
the mergeable parts independently.
