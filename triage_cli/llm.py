"""LLM dispatch layer: factory + three top-level functions.

Provider is selected by LLM_PROVIDER env var (default: unleash).
Valid values: unleash, claude, openai.
"""
from __future__ import annotations

import json
import logging
import os
import re
from datetime import UTC, datetime

from pydantic import ValidationError

from triage_cli.models import (
    LLMTriageOutput,
    SiteEntry,
    Ticket,
    TriageBundle,
    fmt_ts,
    indent_continuations,
)
from triage_cli.providers.base import LLMProvider

logger = logging.getLogger(__name__)

DEFAULT_CLAUDE_MODEL = "claude-sonnet-4-6"
DEFAULT_OPENAI_MODEL = "gpt-5.5"

TRIAGE_SYSTEM_PROMPT = """You are a triage assistant for a Network Engineer
working on the Carbyne APEX NG911/E911 platform at Axon. You receive a Zendesk
ticket and a window of Datadog logs from the affected customer. Return a single
JSON object — no prose, no commentary, no fences required but a ```json fence is
acceptable — matching this schema:

{
  "finding":         "<one or two sentences. What's likely wrong. No padding.>",
  "confidence":      "low" | "medium" | "high",
  "evidence":        [{"timestamp": "<ISO 8601 UTC or null>",
                       "service": "<service name or null>",
                       "message": "<terse, factual; quote sparingly>"}],
  "suggested_note":  "<paste-ready Zendesk internal note. Markdown allowed.
                       Hedge on uncertain claims. Cite log lines you saw.>",
  "next_checks":     ["<concrete verification step>", ...],
  "unknowns":        ["<what you couldn't determine>", ...]
}

Confidence calibration:
- "high":   logs and ticket agree on a specific failure mode.
- "medium": logs are consistent with one cause but don't prove it.
- "low":    logs absent, ambiguous, or contradict the ticket.

Rules:
- Do not invent log lines, error codes, ticket IDs, or past incidents.
- Empty arrays for next_checks/unknowns are preferred over filler.
- If you would hedge three times in finding, the right field is confidence:"low"."""

SITE_EXTRACTION_PROMPT = """You identify which Carbyne APEX customer site a
Zendesk support ticket is about. A list of known sites is provided. Return JSON
with a single field:

{"site_name": "<site_name from the list>" or null}

Rules:
- You MUST only return a site_name that appears verbatim in the provided list.
- Return null if no site clearly matches — do not guess.
- Geographic, agency name, and abbreviation cues in the subject/description
  matter more than exact wording. "Roswell PD GA" → look for a Georgia/Roswell site."""

ANCHOR_EXTRACTION_PROMPT = """You extract the most likely incident timestamp
from a Zendesk ticket. Read the subject, description, and comments. Return JSON
with a single field:

{"timestamp": "<ISO 8601 in UTC>" or null}

Return null if there is no clear timestamp in the content. Do not guess. A
generic "this morning" with no date is null. An explicit "2026-05-06 14:32 PT"
is a timestamp. When in doubt, return null."""


def get_provider() -> LLMProvider:
    """Return the configured LLM provider."""
    from triage_cli.providers.claude import ClaudeAgentProvider
    from triage_cli.providers.openai import OpenAIResponsesProvider
    from triage_cli.providers.unleash import UnleashProvider

    match os.getenv("LLM_PROVIDER", "unleash").lower():
        case "unleash":
            return UnleashProvider()
        case "claude":
            return ClaudeAgentProvider()
        case "openai":
            return OpenAIResponsesProvider()
        case p:
            raise ValueError(f"Unknown LLM_PROVIDER: {p!r}. Valid: unleash, claude, openai")


def _model_for_provider(provider: LLMProvider) -> str:
    match provider.name:
        case "openai":
            return os.getenv("OPENAI_MODEL", DEFAULT_OPENAI_MODEL)
        case _:
            return os.getenv("ANTHROPIC_MODEL", DEFAULT_CLAUDE_MODEL)


async def _collect_text(prompt: str, system_prompt: str, model: str) -> str:
    """Run a single-turn query through the configured provider."""
    provider = get_provider()
    return await provider.complete(prompt=prompt, system_prompt=system_prompt, model=model)


async def triage(
    bundle: TriageBundle,
    model: str | None = None,
    *,
    verbose: bool = False,
) -> LLMTriageOutput:
    """Run the main triage call. Returns a parsed `LLMTriageOutput`.

    On malformed JSON, retries once with a stricter nudge appended to the
    user prompt. Verbose mode logs the first-attempt failure. Second failure
    raises RuntimeError into the caller's failure path.
    """
    provider = get_provider()
    resolved = model or _model_for_provider(provider)
    base_prompt = bundle.as_user_message()

    raw = (await _collect_text(
        prompt=base_prompt,
        system_prompt=TRIAGE_SYSTEM_PROMPT,
        model=resolved,
    )).strip()
    try:
        return LLMTriageOutput.model_validate_json(_strip_code_fence(raw))
    except (json.JSONDecodeError, ValidationError) as e:
        if verbose:
            logger.warning(
                "triage: first attempt returned invalid JSON from %s; retrying. %s",
                provider.name, e,
            )
        retry_prompt = (
            base_prompt
            + "\n\nReturn ONLY a single valid JSON object matching the schema. "
            + "No prose, no commentary."
        )
        raw2 = (await _collect_text(
            prompt=retry_prompt,
            system_prompt=TRIAGE_SYSTEM_PROMPT,
            model=resolved,
        )).strip()
        try:
            return LLMTriageOutput.model_validate_json(_strip_code_fence(raw2))
        except (json.JSONDecodeError, ValidationError) as e2:
            raise RuntimeError(
                f"LLM returned invalid TriageReport JSON after retry: {e2}"
            ) from e2


def _ticket_for_anchor(ticket: Ticket) -> str:
    """Render subject + description + chronological comments for anchor extraction."""
    lines = [
        f"Subject: {ticket.subject}",
        f"Description: {indent_continuations(ticket.description)}",
        "Comments:",
    ]
    if ticket.comments:
        for c in ticket.comments:
            prefix = "" if c.is_public else "[internal] "
            body = indent_continuations(c.body)
            lines.append(f"- {prefix}{fmt_ts(c.created_at)} — {c.author}: {body}")
    else:
        lines.append("(no comments)")
    return "\n".join(lines)


_FENCE_RE = re.compile(r"^\s*```(?:json)?\s*(.*?)\s*```\s*$", re.DOTALL | re.IGNORECASE)


def _strip_code_fence(s: str) -> str:
    """If the model wrapped JSON in a ```json fence, peel it off; otherwise return as-is."""
    m = _FENCE_RE.match(s)
    return m.group(1) if m else s


async def extract_site(
    ticket: Ticket,
    sites: list[SiteEntry],
    model: str | None = None,
) -> str | None:
    """Best-effort site identification from the ticket against the known site list.

    Returns a site_name string that exists in the provided list, or None on no
    confident match, missing/invalid JSON, hallucinated name, or any failure mode.
    Only provider transport errors raise RuntimeError.
    """
    provider = get_provider()
    resolved = model or _model_for_provider(provider)
    known_names = {e.site_name.lower(): e.site_name for e in sites if e.site_name}
    if not known_names:
        return None

    site_list = "\n".join(
        f"  site_name: {e.site_name}  |  friendly_name: {e.friendly_name}"
        for e in sites
        if e.site_name
    )
    prompt = (
        f"Known sites:\n{site_list}\n\n"
        f"Ticket subject: {ticket.subject}\n"
        f"Org: {ticket.requester_org or '(none)'}\n"
        f"Description (first 500 chars): {ticket.description[:500]}"
    )

    raw = await _collect_text(
        prompt=prompt,
        system_prompt=SITE_EXTRACTION_PROMPT,
        model=resolved,
    )
    payload = _strip_code_fence(raw.strip())
    try:
        data = json.loads(payload)
    except json.JSONDecodeError:
        logger.debug("extract_site: invalid JSON from %s: %r", provider.name, raw)
        return None
    if not isinstance(data, dict) or "site_name" not in data:
        logger.debug("extract_site: missing 'site_name' key in %r", data)
        return None
    sn = data["site_name"]
    if sn is None:
        return None
    if not isinstance(sn, str):
        logger.debug("extract_site: 'site_name' was not a string: %r", sn)
        return None
    canonical = known_names.get(sn.lower())
    if canonical is None:
        logger.debug("extract_site: LLM returned unknown site_name %r", sn)
        return None
    return canonical


async def extract_anchor(ticket: Ticket, model: str | None = None) -> datetime | None:
    """Best-effort timestamp extraction from the ticket body.

    Returns a timezone-aware UTC datetime when the model returns a clear
    timestamp; returns None if the model returns null, the response is not
    valid JSON, the 'timestamp' key is missing, or the value cannot be
    parsed as ISO 8601. Raises RuntimeError only on provider transport
    failures (caller should not retry automatically).
    """
    provider = get_provider()
    resolved = model or _model_for_provider(provider)
    raw = await _collect_text(
        prompt=_ticket_for_anchor(ticket),
        system_prompt=ANCHOR_EXTRACTION_PROMPT,
        model=resolved,
    )
    payload = _strip_code_fence(raw.strip())
    try:
        data = json.loads(payload)
    except json.JSONDecodeError:
        logger.debug("extract_anchor: invalid JSON from %s: %r", provider.name, raw)
        return None
    if not isinstance(data, dict) or "timestamp" not in data:
        logger.debug("extract_anchor: missing 'timestamp' key from %s in %r", provider.name, data)
        return None
    ts = data["timestamp"]
    if ts is None:
        return None
    if not isinstance(ts, str):
        logger.debug("extract_anchor: 'timestamp' was not a string from %s: %r", provider.name, ts)
        return None
    try:
        dt = datetime.fromisoformat(ts.replace("Z", "+00:00"))
    except ValueError:
        logger.debug("extract_anchor: could not parse timestamp from %s: %r", provider.name, ts)
        return None
    dt = dt.replace(tzinfo=UTC) if dt.tzinfo is None else dt.astimezone(UTC)
    return dt
