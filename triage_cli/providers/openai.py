"""OpenAI Responses API provider — uses /responses endpoint with gpt-5.5."""
from __future__ import annotations

import json
import logging
from typing import Any

import httpx

from triage_cli.providers.unleash import (
    LLM_TIMEOUT_SECONDS,
    _base_url,
    _provider_error_message,
    _required_env,
)

logger = logging.getLogger(__name__)

DEFAULT_OPENAI_BASE_URL = "https://api.openai.com/v1"


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


def _openai_request_id(data: Any) -> str | None:
    if isinstance(data, dict):
        value = data.get("id")
        return value if isinstance(value, str) and value else None
    return None


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
