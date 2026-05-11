"""LLM provider wrappers for triage, site extraction, and anchor extraction.

Provider selection is controlled by ``LLM_PROVIDER``. Production defaults to
Unleash, while Claude Agent SDK and OpenAI Responses API are available as
explicit fallback providers.
"""
from __future__ import annotations

import json
import logging
import os
import re
import sys
from datetime import UTC, datetime
from typing import Any, Protocol

import httpx
from pydantic import ValidationError

from triage_cli.models import (
    LLMTriageOutput,
    SiteEntry,
    Ticket,
    TriageBundle,
    fmt_ts,
    indent_continuations,
)
from triage_cli.redact import RedactionCounts, redact

logger = logging.getLogger(__name__)

DEFAULT_PROVIDER = "unleash"
DEFAULT_CLAUDE_MODEL = "claude-sonnet-4-6"
DEFAULT_OPENAI_MODEL = "gpt-5.5"
DEFAULT_UNLEASH_BASE_URL = "https://e-api.unleash.so"
DEFAULT_OPENAI_BASE_URL = "https://api.openai.com/v1"
LLM_TIMEOUT_SECONDS = 90.0

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


class LLMProvider(Protocol):
    """Minimal single-turn text provider contract."""

    name: str

    async def complete(self, *, prompt: str, system_prompt: str, model: str) -> str:
        """Return assistant text for a single prompt."""


class UnleashProvider:
    name = "unleash"

    async def complete(self, *, prompt: str, system_prompt: str, model: str) -> str:
        """Call Unleash /chats and return concatenated assistant text parts."""
        endpoint = f"{_base_url('UNLEASH_BASE_URL', DEFAULT_UNLEASH_BASE_URL)}/chats"
        headers = _unleash_headers()
        payload = {
            "assistantId": _required_env("UNLEASH_ASSISTANT_ID", provider=self.name),
            "stream": False,
            "messages": [
                {"role": "System", "text": system_prompt},
                {"role": "User", "text": prompt},
            ],
        }

        data, response_headers = await _post_json(endpoint, headers=headers, payload=payload)
        text = _unleash_text_from_response(data)
        if not text:
            request_id = _request_id_from_payload(data) or response_headers.get("RequestId")
            suffix = f" RequestId: {request_id}." if request_id else ""
            raise RuntimeError(
                "Unleash API response did not include any assistant text parts." + suffix
            )
        return text


class ClaudeAgentProvider:
    name = "claude"

    async def complete(self, *, prompt: str, system_prompt: str, model: str) -> str:
        """Stream a single-turn Claude Agent SDK query and concatenate text blocks."""
        try:
            from claude_agent_sdk import (
                AssistantMessage,
                ClaudeAgentOptions,
                TextBlock,
                query,
            )
        except ImportError as e:  # pragma: no cover - exercised through provider tests
            raise RuntimeError(
                "claude-agent-sdk is not installed. Install with "
                '`pip install -e ".[claude]"` and ensure Claude Code is installed '
                "and authenticated."
            ) from e

        options = ClaudeAgentOptions(system_prompt=system_prompt, model=model)
        chunks: list[str] = []
        try:
            async for message in query(prompt=prompt, options=options):
                if isinstance(message, AssistantMessage):
                    for block in message.content:
                        if isinstance(block, TextBlock):
                            chunks.append(block.text)
        # Catch transport-level failures only; let programming errors propagate.
        except (RuntimeError, OSError) as e:
            raise RuntimeError(f"Claude Agent SDK call failed: {e}") from e
        return "".join(chunks)


class OpenAIResponsesProvider:
    name = "openai"

    async def complete(self, *, prompt: str, system_prompt: str, model: str) -> str:
        """Call the OpenAI Responses API and return output_text content."""
        endpoint = f"{_base_url('OPENAI_BASE_URL', DEFAULT_OPENAI_BASE_URL)}/responses"
        headers = {
            "Authorization": f"Bearer {_required_env('OPENAI_API_KEY', provider=self.name)}",
            "Accept": "application/json",
            "Content-Type": "application/json",
        }
        payload = {
            "model": model,
            "instructions": system_prompt,
            "input": prompt,
            "store": False,
        }

        data, response_headers = await _post_json(endpoint, headers=headers, payload=payload)
        text = _openai_text_from_response(data)
        if not text:
            request_id = _openai_request_id(data) or response_headers.get("x-request-id")
            suffix = f" RequestId: {request_id}." if request_id else ""
            raise RuntimeError(
                "OpenAI Responses API response did not include output_text." + suffix
            )
        return text


def _resolve_provider_name() -> str:
    """Pick the provider from env, defaulting production use to Unleash."""
    provider = (os.getenv("LLM_PROVIDER") or DEFAULT_PROVIDER).strip().lower()
    if provider == "codex":
        return "openai"
    return provider


def _provider_for_name(name: str) -> LLMProvider:
    if name == "unleash":
        return UnleashProvider()
    if name == "claude":
        return ClaudeAgentProvider()
    if name == "openai":
        return OpenAIResponsesProvider()
    raise RuntimeError(
        f"Unsupported LLM_PROVIDER {name!r}. Expected 'unleash', 'claude', "
        "'openai', or 'codex'."
    )


def _resolve_model(model: str | None) -> str:
    """Pick the model for providers that accept model ids."""
    provider = _resolve_provider_name()
    if provider == "claude":
        return model or os.getenv("ANTHROPIC_MODEL") or DEFAULT_CLAUDE_MODEL
    if provider == "openai":
        return model or os.getenv("OPENAI_MODEL") or DEFAULT_OPENAI_MODEL
    return model or ""


def _runtime_label(model: str | None) -> str:
    provider = _resolve_provider_name()
    resolved = _resolve_model(model)
    return f"{provider}:{resolved}" if resolved else provider


async def _collect_text(prompt: str, system_prompt: str, model: str) -> str:
    """Run a single-turn query through the configured provider."""
    provider = _provider_for_name(_resolve_provider_name())
    return await provider.complete(prompt=prompt, system_prompt=system_prompt, model=model)


async def _post_json(
    endpoint: str,
    *,
    headers: dict[str, str],
    payload: dict[str, Any],
) -> tuple[Any, dict[str, str]]:
    try:
        async with httpx.AsyncClient(timeout=LLM_TIMEOUT_SECONDS) as client:
            response = await client.post(endpoint, headers=headers, json=payload)
    except httpx.RequestError as e:
        raise RuntimeError(f"LLM provider API call failed: {e}") from e

    if response.status_code >= 400:
        raise RuntimeError(_provider_error_message(response))

    try:
        data = response.json()
    except json.JSONDecodeError as e:
        raise RuntimeError("LLM provider API response was not valid JSON.") from e
    return data, dict(response.headers)


def _base_url(env_name: str, default: str) -> str:
    return (os.getenv(env_name) or default).strip().rstrip("/")


def _required_env(name: str, *, provider: str) -> str:
    value = os.getenv(name, "").strip()
    if not value:
        raise RuntimeError(f"{name} must be set when LLM_PROVIDER={provider}.")
    return value


def _unleash_headers() -> dict[str, str]:
    api_key = _required_env("UNLEASH_API_KEY", provider="unleash")
    lowered = api_key.lower()
    if lowered.startswith("bearer "):
        authorization = api_key
    elif lowered.startswith("bearer:"):
        authorization = f"Bearer {api_key.split(':', 1)[1].strip()}"
    else:
        authorization = f"Bearer {api_key}"

    headers = {
        "Authorization": authorization,
        "Accept": "application/json",
        "Content-Type": "application/json",
    }
    account = os.getenv("UNLEASH_ACCOUNT", "").strip()
    if account:
        headers["unleash-account"] = account
    return headers


def _unleash_text_from_response(data: Any) -> str:
    """Extract Text parts from a documented Unleash chat response."""
    if isinstance(data, list):
        return "".join(_unleash_text_from_response(item) for item in data)
    if not isinstance(data, dict):
        return ""

    message = data.get("message")
    if not isinstance(message, dict):
        return ""
    parts = message.get("parts")
    if not isinstance(parts, list):
        return ""

    chunks: list[str] = []
    for part in parts:
        if not isinstance(part, dict) or part.get("type") != "Text":
            continue
        text = part.get("text")
        if isinstance(text, str):
            chunks.append(text)
    return "".join(chunks)


def _maybe_redact(text: str, *, enabled: bool) -> tuple[str, RedactionCounts]:
    """Redact when enabled; pass-through with disabled counts when not."""
    if not enabled:
        return text, RedactionCounts(enabled=False)
    return redact(text)


def _openai_text_from_response(data: Any) -> str:
    """Extract output_text from an OpenAI Responses API payload."""
    if not isinstance(data, dict):
        return ""
    top_level = data.get("output_text")
    if isinstance(top_level, str):
        return top_level

    output = data.get("output")
    if not isinstance(output, list):
        return ""

    chunks: list[str] = []
    for item in output:
        if not isinstance(item, dict):
            continue
        content = item.get("content")
        if not isinstance(content, list):
            continue
        for part in content:
            if not isinstance(part, dict) or part.get("type") != "output_text":
                continue
            text = part.get("text")
            if isinstance(text, str):
                chunks.append(text)
    return "".join(chunks)


def _request_id_from_payload(data: Any) -> str | None:
    if isinstance(data, dict):
        value = data.get("requestId")
        return value if isinstance(value, str) and value else None
    return None


def _openai_request_id(data: Any) -> str | None:
    if isinstance(data, dict):
        value = data.get("id")
        return value if isinstance(value, str) and value else None
    return None


def _provider_error_message(response: httpx.Response) -> str:
    detail = response.text.strip()
    request_id = response.headers.get("RequestId") or response.headers.get("x-request-id")
    try:
        payload = response.json()
    except json.JSONDecodeError:
        payload = None
    if isinstance(payload, dict):
        detail_value = payload.get("message") or payload.get("error") or payload.get("detail")
        if isinstance(detail_value, dict):
            detail = str(detail_value.get("message") or detail_value)
        elif isinstance(detail_value, str):
            detail = detail_value
        request_id = request_id or _request_id_from_payload(payload) or _openai_request_id(payload)
    suffix = f" RequestId: {request_id}." if request_id else ""
    detail_suffix = f": {detail}" if detail else ""
    return f"LLM provider API call failed with HTTP {response.status_code}{detail_suffix}.{suffix}"


async def triage(
    bundle: TriageBundle,
    *,
    model: str | None = None,
    verbose: bool = False,
    redact_enabled: bool = True,
) -> tuple[LLMTriageOutput, RedactionCounts]:
    """Run the main triage call. Returns a parsed `LLMTriageOutput` and redaction counts.

    On malformed JSON, retries once with a stricter nudge appended to the
    user prompt. Verbose mode logs the first-attempt failure. Second failure
    raises RuntimeError into the caller's failure path.
    """
    resolved = _resolve_model(model)
    runtime_label = _runtime_label(model)
    raw_prompt = bundle.as_user_message()
    base_prompt, counts = _maybe_redact(raw_prompt, enabled=redact_enabled)
    if counts.enabled:
        if verbose:
            print(
                f"redacted: {counts.phones} phones, {counts.addresses} addresses, "
                f"{counts.coords} coords",
                file=sys.stderr,
            )
    else:
        print("redaction: disabled", file=sys.stderr)

    raw = (await _collect_text(
        prompt=base_prompt,
        system_prompt=TRIAGE_SYSTEM_PROMPT,
        model=resolved,
    )).strip()
    try:
        llm_output = LLMTriageOutput.model_validate_json(_strip_code_fence(raw))
        return llm_output, counts
    except (json.JSONDecodeError, ValidationError) as e:
        if verbose:
            logger.warning(
                "triage: first attempt returned invalid JSON from %s; retrying. %s",
                runtime_label, e,
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
            llm_output = LLMTriageOutput.model_validate_json(_strip_code_fence(raw2))
            return llm_output, counts
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
    *,
    redact_enabled: bool = True,
    verbose: bool = False,
) -> str | None:
    """Best-effort site identification from the ticket against the known site list.

    Returns a site_name string that exists in the provided list, or None on no
    confident match, missing/invalid JSON, hallucinated name, or any failure mode.
    Only provider transport errors raise RuntimeError.
    """
    resolved = _resolve_model(model)
    runtime_label = _runtime_label(model)
    known_names = {e.site_name.lower(): e.site_name for e in sites if e.site_name}
    if not known_names:
        return None

    site_list = "\n".join(
        f"  site_name: {e.site_name}  |  friendly_name: {e.friendly_name}"
        for e in sites
        if e.site_name
    )
    ticket_text = (
        f"Ticket subject: {ticket.subject}\n"
        f"Org: {ticket.requester_org or '(none)'}\n"
        f"Description (first 500 chars): {ticket.description[:500]}"
    )
    redacted_ticket_text, counts = _maybe_redact(ticket_text, enabled=redact_enabled)
    if counts.enabled:
        if verbose:
            print(
                f"redacted: {counts.phones} phones, {counts.addresses} addresses, "
                f"{counts.coords} coords",
                file=sys.stderr,
            )
    else:
        print("redaction: disabled", file=sys.stderr)
    prompt = f"Known sites:\n{site_list}\n\n{redacted_ticket_text}"

    raw = await _collect_text(
        prompt=prompt,
        system_prompt=SITE_EXTRACTION_PROMPT,
        model=resolved,
    )
    payload = _strip_code_fence(raw.strip())
    try:
        data = json.loads(payload)
    except json.JSONDecodeError:
        logger.debug("extract_site: invalid JSON from %s: %r", runtime_label, raw)
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


async def extract_anchor(
    ticket: Ticket,
    model: str | None = None,
    *,
    redact_enabled: bool = True,
    verbose: bool = False,
) -> datetime | None:
    """Best-effort timestamp extraction from the ticket body.

    Returns a timezone-aware UTC datetime when the model returns a clear
    timestamp; returns None if the model returns null, the response is not
    valid JSON, the 'timestamp' key is missing, or the value cannot be
    parsed as ISO 8601. Raises RuntimeError only on provider transport
    failures (caller should not retry automatically).
    """
    resolved = _resolve_model(model)
    runtime_label = _runtime_label(model)
    raw_prompt = _ticket_for_anchor(ticket)
    prompt, counts = _maybe_redact(raw_prompt, enabled=redact_enabled)
    if counts.enabled:
        if verbose:
            print(
                f"redacted: {counts.phones} phones, {counts.addresses} addresses, "
                f"{counts.coords} coords",
                file=sys.stderr,
            )
    else:
        print("redaction: disabled", file=sys.stderr)
    raw = await _collect_text(
        prompt=prompt,
        system_prompt=ANCHOR_EXTRACTION_PROMPT,
        model=resolved,
    )
    payload = _strip_code_fence(raw.strip())
    try:
        data = json.loads(payload)
    except json.JSONDecodeError:
        logger.debug("extract_anchor: invalid JSON from %s: %r", runtime_label, raw)
        return None
    if not isinstance(data, dict) or "timestamp" not in data:
        logger.debug("extract_anchor: missing 'timestamp' key from %s in %r", runtime_label, data)
        return None
    ts = data["timestamp"]
    if ts is None:
        return None
    if not isinstance(ts, str):
        logger.debug("extract_anchor: 'timestamp' was not a string from %s: %r", runtime_label, ts)
        return None
    try:
        # fromisoformat handles offset suffixes; swap trailing Z for +00:00 for 3.10 compat.
        dt = datetime.fromisoformat(ts.replace("Z", "+00:00"))
    except ValueError:
        logger.debug("extract_anchor: could not parse timestamp from %s: %r", runtime_label, ts)
        return None
    dt = dt.replace(tzinfo=UTC) if dt.tzinfo is None else dt.astimezone(UTC)
    return dt
