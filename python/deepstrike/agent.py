from __future__ import annotations
import asyncio
import json
import pathlib
from typing import AsyncIterator, TYPE_CHECKING
from deepstrike._kernel import (
    LoopStateMachine, LoopPolicy, RuntimeTask,
    Message, ToolCall, ToolSchema, ToolResult,
    Governance, SignalRouter,
)
from deepstrike.providers.base import LLMProvider
from deepstrike.providers.stream import (
    StreamEvent, TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent, ErrorEvent,
)
from deepstrike.tools.registry import RegisteredTool
from deepstrike.tools.execution import execute_tools
if TYPE_CHECKING:
    from deepstrike.memory.protocols import (
        DreamStore, DreamResult,
        MemoryEntry as DsMemoryEntry,
        CurationResult as DsCurationResult,
        CurationStats as DsCurationStats,
    )
    from deepstrike.knowledge.source import KnowledgeSource


class Agent:
    def __init__(
        self,
        provider: LLMProvider,
        *,
        max_tokens: int,
        max_turns: int,
        timeout_ms: int | None = None,
        skill_dir: str | None = None,
        extensions: dict | None = None,
        governance: Governance | None = None,
        signal_router: SignalRouter | None = None,
        knowledge_source: "KnowledgeSource | None" = None,
        dream_store: "DreamStore | None" = None,
        agent_id: str | None = None,
    ):
        self._provider = provider
        self._policy = LoopPolicy(
            max_tokens=max_tokens,
            max_turns=max_turns,
            timeout_ms=timeout_ms,
        )
        self._tools: dict[str, RegisteredTool] = {}
        self._skill_dir = pathlib.Path(skill_dir) if skill_dir else None
        self._extensions: dict = extensions or {}
        self._governance = governance
        self._blocked_tools: set[str] = set()
        self._signal_router = signal_router
        self._knowledge_source = knowledge_source
        self._dream_store = dream_store
        self._agent_id = agent_id
        self._interrupted = False

    def block_tool(self, name: str) -> "Agent":
        if self._governance:
            self._governance.block_tool(name)
        self._blocked_tools.add(name)
        return self

    def interrupt(self) -> None:
        """Signal the running loop to stop after the current step."""
        self._interrupted = True

    def register(self, *tools: RegisteredTool) -> "Agent":
        for t in tools:
            self._tools[t.schema.name] = t
        return self

    def unregister(self, tool_name: str) -> "Agent":
        self._tools.pop(tool_name, None)
        return self

    async def run(self, goal: str, criteria: list[str] | None = None, extensions: dict | None = None) -> str:
        result = None
        async for event in self.run_streaming(goal, criteria=criteria, extensions=extensions):
            if isinstance(event, DoneEvent):
                result = event
        return f"done in {result.iterations} turns ({result.status})" if result else "done"

    async def run_streaming(self, goal: str, *, criteria: list[str] | None = None, extensions: dict | None = None) -> AsyncIterator[StreamEvent]:
        self._interrupted = False
        from deepstrike._kernel import SkillMetadata as KernelSkillMetadata
        sm = LoopStateMachine(self._policy)
        sm.set_tools([t.schema for t in self._tools.values()])
        ext = {**self._extensions, **(extensions or {})}

        # Scan skill directory and register metadata so the kernel injects the skill meta-tool.
        if self._skill_dir and self._skill_dir.is_dir():
            from deepstrike.skills.registry import SkillRegistry
            registry = SkillRegistry(str(self._skill_dir))
            skill_metas = registry.scan()
            sm.set_available_skills([
                KernelSkillMetadata(
                    name=m.name,
                    description=m.description or "",
                    when_to_use=getattr(m, "when_to_use", None),
                    effort=getattr(m, "effort", None),
                    estimated_tokens=getattr(m, "estimated_tokens", 0) or 0,
                )
                for m in skill_metas
            ])

        # Enable in-session memory retrieval when both store and agent identity are configured.
        if self._dream_store and self._agent_id:
            sm.set_memory_enabled(True)

        # Enable knowledge meta-tool when a KnowledgeSource is configured.
        if self._knowledge_source:
            sm.set_knowledge_enabled(True)

        action = sm.start(RuntimeTask(goal, criteria=criteria))
        final_text = ""

        while not sm.is_terminal():
            if self._interrupted:
                action = sm.feed_timeout()
                break

            # Drain queued signals through kernel SignalRouter
            if self._signal_router and self._signal_router.depth() > 0:
                queued = self._signal_router.next()
                if queued:
                    disposition = self._signal_router.ingest(queued, action.kind == "execute_tools")
                    if disposition in ("interrupt_now", "interrupt"):
                        action = sm.feed_timeout()
                        break

            sm.take_observations()

            if action.kind == "call_llm":
                final_text = ""
                final_tool_calls: list[ToolCall] = []
                messages = list(action.messages or [])

                try:
                    gen = await self._provider.stream(messages, action.tools or [], extensions=ext if ext else None)
                    async for evt in gen:
                        yield evt
                        if isinstance(evt, TextDelta):
                            final_text += evt.delta
                        elif isinstance(evt, ToolCallEvent):
                            final_tool_calls.append(ToolCall(
                                id=evt.id, name=evt.name,
                                arguments=json.dumps(evt.arguments),
                            ))
                except Exception as exc:
                    yield ErrorEvent(message=str(exc))
                    action = sm.feed_timeout()
                    break

                action = sm.feed_llm_response(Message(
                    role="assistant",
                    content=final_text,
                    tool_calls=final_tool_calls,
                ))

            elif action.kind == "execute_tools":
                calls = action.calls or []

                # Governance: blocked tools check
                permitted = []
                for c in calls:
                    if c.name in self._blocked_tools:
                        yield ErrorEvent(message=f"tool blocked: {c.name}")
                    else:
                        permitted.append(c)
                calls = permitted

                # Intercept meta-tool calls
                skill_calls = [c for c in calls if c.name == "skill"]
                memory_calls = [c for c in calls if c.name == "memory"]
                knowledge_calls = [c for c in calls if c.name == "knowledge"]
                regular_calls = [c for c in calls if c.name not in ("skill", "memory", "knowledge")]

                all_results = []
                for c in skill_calls:
                    try:
                        args = json.loads(c.arguments) if isinstance(c.arguments, str) else c.arguments
                        name = str(args.get("name", ""))
                    except Exception:
                        name = ""
                    content = None
                    if self._skill_dir and name:
                        skill_path = self._skill_dir / f"{name}.md"
                        if skill_path.exists():
                            content = skill_path.read_text(encoding="utf-8")
                    output = content if content is not None else f'Skill "{name}" not found.'
                    is_error = content is None
                    yield ToolResultEvent(call_id=c.id, name=c.name, content=output, is_error=is_error)
                    all_results.append(ToolResult(call_id=c.id, output=output, is_error=is_error))

                for c in memory_calls:
                    if self._dream_store and self._agent_id:
                        try:
                            args = json.loads(c.arguments) if isinstance(c.arguments, str) else c.arguments
                            query = str(args.get("query", ""))
                            top_k = int(args.get("top_k", 5))
                            entries = await self._dream_store.search(self._agent_id, query, top_k)
                            if entries:
                                output = "\n---\n".join(
                                    f"[score={e.score:.3f}] {e.text}" for e in entries
                                )
                            else:
                                output = "No relevant memories found."
                            is_error = False
                        except Exception as exc:
                            output = f"Memory search error: {exc}"
                            is_error = True
                    else:
                        output = "Memory retrieval not configured."
                        is_error = True
                    yield ToolResultEvent(call_id=c.id, name=c.name, content=output, is_error=is_error)
                    all_results.append(ToolResult(call_id=c.id, output=output, is_error=is_error))

                results = await execute_tools(regular_calls, self._tools)
                for r in results:
                    tool_name = next((c.name for c in regular_calls if c.id == r.call_id), "")
                    yield ToolResultEvent(call_id=r.call_id, name=tool_name, content=r.output, is_error=r.is_error)
                all_results.extend(results)

                # knowledge meta-tool interception
                for c in knowledge_calls:
                    if self._knowledge_source:
                        try:
                            args = json.loads(c.arguments) if isinstance(c.arguments, str) else c.arguments
                            query = str(args.get("query", ""))
                            top_k = int(args.get("top_k", 5))
                            snippets = await self._knowledge_source.retrieve(query, top_k)
                            output = "\n---\n".join(snippets) if snippets else "No relevant knowledge found."
                            is_error = False
                        except Exception as exc:
                            output = f"Knowledge retrieval error: {exc}"
                            is_error = True
                    else:
                        output = "Knowledge source not configured."
                        is_error = True
                    yield ToolResultEvent(call_id=c.id, name=c.name, content=output, is_error=is_error)
                    all_results.append(ToolResult(call_id=c.id, output=output, is_error=is_error))

                action = sm.feed_tool_results(all_results)

            elif action.kind == "done":
                break

        result = action.result
        yield DoneEvent(
            iterations=result.turns_used if result else 0,
            total_tokens=result.total_tokens_used if result else 0,
            status=result.termination if result else "error",
        )

    async def dream(self, agent_id: str, now_ms: int = 0) -> "DreamResult":
        """
        Trigger an idle dreaming cycle for the given agent.

        Phase 1 — kernel rule-based analysis + LLM prompt assembly (pure computation)
        Phase 2 — LLM synthesis call (I/O, here)
        Phase 3 — kernel parses + curates results (pure computation)
        Phase 4 — commit delta to DreamStore (I/O, here)

        Requires `dream_store` to be configured on the Agent.
        """
        from deepstrike.memory.protocols import (
            DreamResult,
            MemoryEntry as DsMemoryEntry,
            CurationResult as DsCurationResult,
            CurationStats as DsCurationStats,
        )
        from deepstrike._kernel import (
            IdlePipeline,
            SessionData as KernelSessionData,
            MemoryEntry as KernelMemoryEntry,
        )

        if self._dream_store is None:
            raise RuntimeError("dream_store not configured on Agent")

        # --- Phase 0: SDK I/O — load raw data ----------------------------
        sessions = await self._dream_store.load_sessions(agent_id)
        existing = await self._dream_store.load_memories(agent_id)

        if not sessions:
            return DreamResult()

        # Convert DreamStore types → kernel types
        kernel_sessions = [
            KernelSessionData(
                session_id=s.session_id,
                agent_id=s.agent_id,
                messages=s.messages,
                metadata=json.dumps(s.metadata) if s.metadata is not None else "null",
                created_at_ms=float(s.created_at_ms),
                updated_at_ms=float(s.updated_at_ms),
            )
            for s in sessions
        ]
        kernel_memories = [
            KernelMemoryEntry(
                text=e.text,
                score=e.score,
                metadata=json.dumps(e.metadata) if e.metadata is not None else "null",
            )
            for e in existing
        ]

        # --- Phase 1: kernel builds synthesis prompt (pure computation) --
        pipeline = IdlePipeline(agent_id)
        action = pipeline.feed_trigger(kernel_sessions, kernel_memories, float(now_ms))
        if action.kind == "noop":
            return DreamResult()
        if action.kind != "synthesize_insights":
            raise RuntimeError(f"unexpected action after feed_trigger: {action.kind}")

        # --- Phase 2: SDK calls LLM for synthesis (I/O) ------------------
        synthesis_text = ""
        async for evt in await self._provider.stream(
            action.messages or [], [], extensions=None
        ):
            if isinstance(evt, TextDelta):
                synthesis_text += evt.delta

        # --- Phase 3: kernel parses + curates (pure computation) ---------
        action2 = pipeline.feed_synthesis_result(synthesis_text)
        if action2.kind != "commit_memories":
            raise RuntimeError(
                f"unexpected action after feed_synthesis_result: {action2.kind}"
            )

        cr = action2.curation_result
        rr = action2.run_result

        # Convert kernel types → DreamStore types
        def _parse_meta(s: str) -> object:
            try:
                return json.loads(s)
            except Exception:
                return None

        ds_result = DsCurationResult(
            to_add=[
                DsMemoryEntry(
                    text=e.text,
                    score=e.score,
                    metadata=_parse_meta(e.metadata),
                )
                for e in (cr.to_add or [])
            ],
            to_remove_indices=list(cr.to_remove_indices or []),
            stats=DsCurationStats(
                insights_processed=cr.stats.insights_processed,
                duplicates_removed=cr.stats.duplicates_removed,
                conflicts_resolved=cr.stats.conflicts_resolved,
                entries_added=cr.stats.entries_added,
            ),
        )

        # --- Phase 4: SDK writes delta to store (I/O) --------------------
        await self._dream_store.commit(agent_id, ds_result, existing)

        return DreamResult(
            sessions_processed=rr.sessions_processed,
            insights_extracted=rr.insights_extracted,
            entries_added=cr.stats.entries_added,
            entries_removed=len(cr.to_remove_indices or []),
        )


