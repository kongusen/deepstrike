"""
hello_agent — Sprint 1 end-to-end demo.

Usage:
    cd deepstrike/python
    pip install -e .
    ANTHROPIC_API_KEY=sk-... python examples/hello_agent/main.py "Read README.md and summarize"
"""
import asyncio
import os
import sys
from deepstrike import Agent, AnthropicProvider, read_file, TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent


async def main(goal: str):
    provider = AnthropicProvider(api_key=os.environ["ANTHROPIC_API_KEY"])
    agent = Agent(provider, max_tokens=200_000, max_turns=10).register(read_file)

    async for event in agent.run_streaming(goal):
        if isinstance(event, TextDelta):
            print(event.delta, end="", flush=True)
        elif isinstance(event, ToolCallEvent):
            print(f"\n[→ {event.name}({list(event.arguments.values())[0] if event.arguments else ''})]")
        elif isinstance(event, ToolResultEvent):
            preview = event.content[:80].replace("\n", " ")
            print(f"[← {preview}{'...' if len(event.content) > 80 else ''}]")
        elif isinstance(event, DoneEvent):
            print(f"\n[done in {event.iterations} turns, ~{event.total_tokens} tokens]")


if __name__ == "__main__":
    goal = " ".join(sys.argv[1:]) or "What files are in the current directory?"
    asyncio.run(main(goal))
