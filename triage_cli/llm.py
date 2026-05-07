"""Claude Agent SDK wrappers: ``triage(bundle)`` and ``extract_anchor(ticket)``.

Both are async, single-turn, no tools. Model resolves from the explicit arg,
then ``ANTHROPIC_MODEL`` env, then ``claude-sonnet-4-6``. The Agent SDK
inherits Claude Code's auth, so no API key is read here.
"""
from __future__ import annotations

import json
import logging
import os
import re
from datetime import datetime, timezone

from triage_cli.models import Ticket, TriageBundle, _fmt_ts, _indent_continuations

try:
    from claude_agent_sdk import (
        AssistantMessage,
        ClaudeAgentOptions,
        TextBlock,
        query,
    )
except ImportError as e:  # pragma: no cover - import-time guard
    raise RuntimeError(
        "claude-agent-sdk is not installed. Install with `pip install claude-agent-sdk` "
        "and ensure Claude Code is installed and authenticated."
    ) from e


logger = logging.getLogger(__name__)

DEFAULT_MODEL = "claude-sonnet-4-6"

TRIAGE_SYSTEM_PROMPT = """You are a triage assistant for a Network Engineer working on the Carbyne APEX
NG911/E911 platform at Axon. You receive a Zendesk ticket and a window of
Datadog logs from the affected customer. Produce a structured triage note in
markdown with exactly these four sections, in this order:

## Summary
Two sentences. What the ticket reports. No speculation.

## Log signals
What the logs actually show in the window. Quote sparingly. Note error
counts, recurring messages, and timing relative to the anchor timestamp. If
logs are empty or all routine, say so plainly. Do not infer causes here.

## Likely cause (inference)
Your best guess at the cause, given the ticket and logs. Mark this section as
inference. If the logs do not support a cause, say "Insufficient log evidence
to infer cause" rather than guessing.

## Suggested first action
One concrete step the engineer should take first. Prefer "check X" or
"verify Y" over open-ended advice. If you cannot suggest a useful action,
say so.

Rules:
- Do not invent log lines, error codes, ticket IDs, or past incidents.
- Do not assign priority or confidence scores.
- Do not pad. Empty findings are valid findings.
- If sections 2 and 3 disagree, that is signal; do not paper over it."""

ANCHOR_EXTRACTION_PROMPT = """You extract the most likely incident timestamp from a Zendesk ticket. Read
the subject, description, and comments. Return JSON with a single field:

{"timestamp": "<ISO 8601 in UTC>" or null}

Return null if there is no clear timestamp in the content. Do not guess. A
generic "this morning" with no date is null. An explicit "2026-05-06 14:32 PT"
is a timestamp. When in doubt, return null."""


def _resolve_model(model: str | None) -> str:
    """Pick the model: explicit arg > ANTHROPIC_MODEL env > default."""
    return model or os.getenv("ANTHROPIC_MODEL") or DEFAULT_MODEL


async def _collect_text(prompt: str, system_prompt: str, model: str) -> str:
    """Stream a single-turn query and concatenate AssistantMessage text blocks."""
    options = ClaudeAgentOptions(system_prompt=system_prompt, model=model)
    chunks: list[str] = []
    try:
        async for message in query(prompt=prompt, options=options):
            if isinstance(message, AssistantMessage):
                for block in message.content:
                    if isinstance(block, TextBlock):
                        chunks.append(block.text)
    except Exception as e:
        raise RuntimeError(f"Claude Agent SDK call failed: {e}") from e
    return "".join(chunks)


async def triage(bundle: TriageBundle, model: str | None = None) -> str:
    """Run the main triage call. Returns the raw markdown response, stripped."""
    resolved = _resolve_model(model)
    text = await _collect_text(
        prompt=bundle.as_user_message(),
        system_prompt=TRIAGE_SYSTEM_PROMPT,
        model=resolved,
    )
    return text.strip()


def _ticket_for_anchor(ticket: Ticket) -> str:
    """Render subject + description + chronological comments for anchor extraction."""
    lines = [
        f"Subject: {ticket.subject}",
        f"Description: {_indent_continuations(ticket.description)}",
        "Comments:",
    ]
    if ticket.comments:
        for c in ticket.comments:
            prefix = "" if c.is_public else "[internal] "
            body = _indent_continuations(c.body)
            lines.append(f"- {prefix}{_fmt_ts(c.created_at)} — {c.author}: {body}")
    else:
        lines.append("(no comments)")
    return "\n".join(lines)


_FENCE_RE = re.compile(r"^\s*```(?:json)?\s*(.*?)\s*```\s*$", re.DOTALL | re.IGNORECASE)


def _strip_code_fence(s: str) -> str:
    """If the model wrapped JSON in a ```json fence, peel it off; otherwise return as-is."""
    m = _FENCE_RE.match(s)
    return m.group(1) if m else s


async def extract_anchor(ticket: Ticket, model: str | None = None) -> datetime | None:
    """Best-effort timestamp extraction. Returns UTC-aware datetime or None.

    Never raises on parse errors — only on Agent SDK transport errors. The
    caller is expected to fall back to ``ticket.created_at`` when this returns
    None.
    """
    resolved = _resolve_model(model)
    raw = await _collect_text(
        prompt=_ticket_for_anchor(ticket),
        system_prompt=ANCHOR_EXTRACTION_PROMPT,
        model=resolved,
    )
    payload = _strip_code_fence(raw.strip())
    try:
        data = json.loads(payload)
    except json.JSONDecodeError:
        logger.debug("anchor extraction: response was not valid JSON: %r", raw)
        return None
    if not isinstance(data, dict) or "timestamp" not in data:
        logger.debug("anchor extraction: missing 'timestamp' key in %r", data)
        return None
    ts = data["timestamp"]
    if ts is None:
        return None
    if not isinstance(ts, str):
        logger.debug("anchor extraction: 'timestamp' was not a string: %r", ts)
        return None
    try:
        # fromisoformat handles offset suffixes; swap trailing Z for +00:00 for 3.10 compat.
        dt = datetime.fromisoformat(ts.replace("Z", "+00:00"))
    except ValueError:
        logger.debug("anchor extraction: could not parse timestamp %r", ts)
        return None
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    else:
        dt = dt.astimezone(timezone.utc)
    return dt
