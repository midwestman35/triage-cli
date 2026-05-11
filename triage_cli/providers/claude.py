"""Claude Agent SDK provider — inherits Claude Code auth, no API key needed."""
from __future__ import annotations

import logging

logger = logging.getLogger(__name__)


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
        except ImportError as e:
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
        except (RuntimeError, OSError) as e:
            raise RuntimeError(f"Claude Agent SDK call failed: {e}") from e
        return "".join(chunks)
