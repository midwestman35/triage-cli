"""LLM provider protocol."""
from __future__ import annotations

from typing import Protocol, runtime_checkable


@runtime_checkable
class LLMProvider(Protocol):
    """Minimal single-turn text provider contract."""

    name: str

    async def complete(self, *, prompt: str, system_prompt: str, model: str) -> str:
        """Return assistant text for a single-turn prompt."""
