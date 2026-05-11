"""Unleash LLM provider — calls e-api.unleash.so /chats."""
from __future__ import annotations

import json
import logging
import os
from typing import Any

import httpx

logger = logging.getLogger(__name__)

DEFAULT_UNLEASH_BASE_URL = "https://e-api.unleash.so"
LLM_TIMEOUT_SECONDS = 90.0


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


def _request_id_from_payload(data: Any) -> str | None:
    if isinstance(data, dict):
        value = data.get("requestId")
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
        request_id = request_id or _request_id_from_payload(payload)
    suffix = f" RequestId: {request_id}." if request_id else ""
    detail_suffix = f": {detail}" if detail else ""
    return f"LLM provider API call failed with HTTP {response.status_code}{detail_suffix}.{suffix}"
