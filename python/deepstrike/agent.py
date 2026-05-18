from __future__ import annotations
import asyncio
import json
import pathlib
from typing import AsyncIterator, TYPE_CHECKING, Awaitable, Callable, Any
from deepstrike._kernel import (
    LoopStateMachine, LoopPolicy, RuntimeTask,
    Message, ToolCall, ToolSchema, ToolResult,
    SignalRouter,
)
from deepstrike.governance import Governance
from deepstrike.providers.base import LLMProvider, RenderedContext
from deepstrike.providers.stream import (
    StreamEvent, TextDelta, ToolCallEvent, ToolDeltaEvent, ToolSuspendEvent, ToolResultEvent, DoneEvent, ErrorEvent,
    PermissionRequestEvent,
)
from deepstrike.tools.registry import RegisteredTool
from deepstrike.tools.execution import execute_tools
from deepstrike.tools.registry import normalize_tool_chunk, tool_chunk_text, validate_tool_arguments
from collections.abc import AsyncIterable
import asyncio
if TYPE_CHECKING:
    from deepstrike.memory.protocols import (
        DreamStore, DreamResult,
        SessionStore,
        MemoryEntry as DsMemoryEntry,
        CurationResult as DsCurationResult,
        CurationStats as DsCurationStats,
    )
    from deepstrike.knowledge.source import KnowledgeSource
    from deepstrike.signals.types import SignalSource


def _strip_frontmatter(content: str) -> str:
    import re
    return re.sub(r"^---\n.*?\n---\n?", "", content, count=1, flags=re.DOTALL)


class Agent:
    def __init__(
        self,
        provider: LLMProvider,
        *,
        max_tokens: int,
        max_turns: int = 25,
        timeout_ms: int | None = None,
        system_prompt: str | None = None,
        skill_dir: str | None = None,
        extensions: dict | None = None,
        on_tool_suspend: Callable[[ToolSuspendEvent], Awaitable[Any] | Any] | None = None,
        governance: Governance | None = None,
        signal_source: "SignalSource | None" = None,
        knowledge_source: "KnowledgeSource | None" = None,
        dream_store: "DreamStore | None" = None,
        session_store: "SessionStore | None" = None,
        agent_id: str | None = None,
    ):
        self._provider = provider
        self._policy = LoopPolicy(
            max_tokens=max_tokens,
            max_turns=max_turns,
            timeout_ms=timeout_ms,
        )
        self._max_tokens = max_tokens
        self._tools: dict[str, RegisteredTool] = {}
        self._system_prompt = system_prompt
        self._skill_dir = pathlib.Path(skill_dir) if skill_dir else None
        self._extensions: dict = extensions or {}
        self._on_tool_suspend = on_tool_suspend
        self._governance = governance
        self._signal_source = signal_source
        self._knowledge_source = knowledge_source
        self._dream_store = dream_store
        self._session_store = session_store
        self._sessions: dict[str, "SessionData"] = {}
        self._agent_id = agent_id
        self._interrupted = False
        self._pending_interrupt = False
        self._turn = 0
        self._pressure = 0.0

    def block_tool(self, name: str) -> "Agent":
        if self._governance is None:
            self._governance = Governance()
        self._governance.block_tool(name)
        return self

    def interrupt(self) -> None:
        """Signal the running loop to stop after the current step."""
        self._interrupted = True

    @property
    def turn(self) -> int:
        """Current turn index within the active run (0 before a run starts)."""
        return self._turn

    @property
    def pressure(self) -> float:
        """Token budget pressure ratio [0–1]. Updated after each run completes."""
        return self._pressure

    def register(self, *tools: RegisteredTool) -> "Agent":
        for t in tools:
            self._tools[t.schema.name] = t
        return self

    def unregister(self, tool_name: str) -> "Agent":
        self._tools.pop(tool_name, None)
        return self

    async def run(self, goal: str, criteria: list[str] | None = None, extensions: dict | None = None, session_id: str | None = None) -> str:
        content = ""
        async for event in self.run_streaming(goal, criteria=criteria, extensions=extensions, session_id=session_id):
            if isinstance(event, TextDelta):
                content += event.delta
        return content

    async def run_streaming(self, goal: str, *, criteria: list[str] | None = None, extensions: dict | None = None, session_id: str | None = None) -> AsyncIterator[StreamEvent]:
        self._interrupted = False
        self._pending_interrupt = False
        self._turn = 0
        self._pressure = 0.0

        if self._knowledge_source:
            await self._knowledge_source.init()

        from deepstrike._kernel import SkillMetadata as KernelSkillMetadata
        sm = LoopStateMachine(self._policy)
        sm.set_tools([t.schema for t in self._tools.values()])
        ext = {**self._extensions, **(extensions or {})}

        if self._system_prompt:
            tokens = max(1, len(self._system_prompt) // 4)
            sm.add_system_message(self._system_prompt, tokens)

        previous_session = await self._load_session(session_id) if session_id else None
        previous_msgs = list(previous_session.messages) if previous_session else []
        if previous_msgs:
            sm.preload_history(previous_msgs)

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
        import time as _time
        session_start = int(_time.time() * 1000)
        router = SignalRouter(max_queue_size=256)

        while not sm.is_terminal():
            if self._interrupted:
                action = sm.feed_timeout()
                break
            if self._pending_interrupt:
                self._pending_interrupt = False
                action = sm.feed_timeout()
                break

            if self._signal_source:
                sig = await self._signal_source.next_signal()
                if sig:
                    disposition = router.ingest(
                        sig.to_kernel_signal(),
                        action.kind == "execute_tools",
                    )
                    if disposition == "interrupt_now":
                        action = sm.feed_timeout()
                        break
                    if disposition == "interrupt":
                        self._pending_interrupt = True

            queued = router.next()
            while queued:
                if queued.urgency == "critical":
                    action = sm.feed_timeout()
                    break
                if queued.urgency == "high":
                    self._pending_interrupt = True
                queued = router.next()
            if self._interrupted or sm.is_terminal():
                break

            sm.take_observations()

            if action.kind == "call_llm":
                self._turn += 1
                final_text = ""
                final_tool_calls: list[ToolCall] = []
                raw_ctx = action.context
                context = RenderedContext(
                    system_text=getattr(raw_ctx, "system_text", "") or "",
                    turns=list(getattr(raw_ctx, "turns", [])),
                )

                try:
                    async for evt in self._provider.stream(context, action.tools or [], extensions=ext if ext else None):
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
                permitted = []
                denied_results = []
                for c in calls:
                    if self._governance:
                        self._governance.set_time(int(_time.time() * 1000))
                        verdict = self._governance.evaluate(c.name, c.arguments)
                        if verdict.kind == "deny":
                            message = f"permission denied: {c.name} - {verdict.reason or ''}"
                            yield ErrorEvent(message=message)
                            denied_results.append(ToolResult(call_id=c.id, output=message, is_error=True))
                            continue
                        if verdict.kind == "rate_limited":
                            retry_after = int(verdict.retry_after_ms or 0)
                            message = f"rate limited: {c.name} - retry after {retry_after}ms"
                            yield ErrorEvent(message=message)
                            denied_results.append(ToolResult(call_id=c.id, output=message, is_error=True))
                            continue
                        if verdict.kind == "ask_user":
                            yield PermissionRequestEvent(
                                call_id=c.id,
                                tool_name=c.name,
                                arguments=c.arguments,
                                reason=verdict.reason or "",
                            )
                            denied_results.append(ToolResult(
                                call_id=c.id,
                                output=f"awaiting user approval: {c.name}",
                                is_error=True,
                            ))
                            continue
                    permitted.append(c)
                calls = permitted

                # Intercept meta-tool calls
                skill_calls = [c for c in calls if c.name == "skill"]
                memory_calls = [c for c in calls if c.name == "memory"]
                knowledge_calls = [c for c in calls if c.name == "knowledge"]
                regular_calls = [c for c in calls if c.name not in ("skill", "memory", "knowledge")]

                all_results = list(denied_results)
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
                            raw = skill_path.read_text(encoding="utf-8")
                            content = _strip_frontmatter(raw)
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

                results = []
                queue: asyncio.Queue[tuple[str, object]] = asyncio.Queue()

                async def run_regular_tool(call):
                    registered = self._tools.get(call.name)
                    if registered is None:
                        await queue.put(("result", ToolResult(call_id=call.id, output=f"unknown tool: {call.name}", is_error=True)))
                        return
                    try:
                        kwargs = json.loads(call.arguments or "{}")
                        validation_error = validate_tool_arguments(registered.schema.parameters, kwargs)
                        if validation_error:
                            await queue.put(("result", ToolResult(call_id=call.id, output=f"invalid arguments: {validation_error}", is_error=True)))
                            return
                        output = await registered(**kwargs)
                        if isinstance(output, AsyncIterable):
                            chunks = []
                            iterator = output.__aiter__()
                            resume_value = None
                            while True:
                                try:
                                    raw_chunk = await (iterator.__anext__() if resume_value is None else iterator.asend(resume_value))
                                    resume_value = None
                                except StopAsyncIteration:
                                    break
                                normalized = normalize_tool_chunk(raw_chunk)
                                if normalized.get("type") == "suspend":
                                    event = ToolSuspendEvent(
                                        call_id=call.id,
                                        name=call.name,
                                        suspension_id=str(normalized.get("suspensionId", normalized.get("suspension_id", ""))),
                                        payload=normalized.get("payload"),
                                    )
                                    await queue.put(("delta", event))
                                    if self._on_tool_suspend is None:
                                        await queue.put(("result", ToolResult(
                                            call_id=call.id,
                                            output=f"tool suspended without resume handler: {event.suspension_id}",
                                            is_error=True,
                                        )))
                                        return
                                    resume_value = self._on_tool_suspend(event)
                                    if hasattr(resume_value, "__await__"):
                                        resume_value = await resume_value
                                    continue
                                text = tool_chunk_text(raw_chunk)
                                chunks.append(text)
                                await queue.put(("delta", ToolDeltaEvent(
                                    call_id=call.id,
                                    name=call.name,
                                    delta=text,
                                    chunk=None if isinstance(raw_chunk, str) else normalized,
                                )))
                            output = "".join(chunks)
                        await queue.put(("result", ToolResult(call_id=call.id, output=str(output), is_error=False)))
                    except Exception as exc:
                        await queue.put(("result", ToolResult(call_id=call.id, output=str(exc), is_error=True)))

                tasks = [asyncio.create_task(run_regular_tool(call)) for call in regular_calls]
                pending_results = len(tasks)
                while pending_results:
                    kind, item = await queue.get()
                    if kind == "delta":
                        yield item
                    else:
                        results.append(item)
                        pending_results -= 1
                if tasks:
                    await asyncio.gather(*tasks)
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
        if result and self._max_tokens:
            self._pressure = result.total_tokens_used / self._max_tokens

        new_msgs = list(sm.drain_new_messages())

        if self._dream_store and self._agent_id and new_msgs:
            try:
                from deepstrike.memory.protocols import SessionData
                now_ms = int(_time.time() * 1000)
                import uuid as _uuid
                await self._dream_store.save_session(SessionData(
                    session_id=str(_uuid.uuid4()),
                    agent_id=self._agent_id,
                    messages=new_msgs,
                    created_at_ms=session_start,
                    updated_at_ms=now_ms,
                ))
            except Exception:
                pass

        if session_id:
            from deepstrike.memory.protocols import SessionData
            now_ms = int(_time.time() * 1000)
            await self._save_session(SessionData(
                session_id=session_id,
                agent_id=self._agent_id or "default",
                messages=[*previous_msgs, *new_msgs],
                metadata=previous_session.metadata if previous_session else None,
                created_at_ms=previous_session.created_at_ms if previous_session else session_start,
                updated_at_ms=now_ms,
            ))

        yield DoneEvent(
            iterations=result.turns_used if result else 0,
            total_tokens=result.total_tokens_used if result else 0,
            status=result.termination if result else "error",
        )

    async def _load_session(self, session_id: str):
        if self._session_store:
            return await self._session_store.load_session(session_id)
        return self._sessions.get(session_id)

    async def _save_session(self, data):
        if self._session_store:
            await self._session_store.save_session(data)
        else:
            self._sessions[data.session_id] = data

    async def dream(self, agent_id: str, now_ms: int | None = None) -> "DreamResult":
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
        import time as _time
        if now_ms is None:
            now_ms = int(_time.time() * 1000)

        # --- Phase 0: SDK I/O — load raw data ----------------------------
        sessions = await self._dream_store.load_sessions(agent_id)
        existing = await self._dream_store.load_memories(agent_id)

        if not sessions:
            return DreamResult()

        # Convert DreamStore types → kernel types
        def _to_kernel_message(message: object) -> Message:
            if isinstance(message, Message):
                return message
            if isinstance(message, dict):
                raw_tool_calls = message.get("tool_calls") or message.get("toolCalls") or []
                tool_calls = [
                    ToolCall(
                        id=str(call.get("id", "")),
                        name=str(call.get("name", "")),
                        arguments=str(call.get("arguments", "{}")),
                    )
                    for call in raw_tool_calls
                    if isinstance(call, dict)
                ]
                return Message(
                    role=str(message.get("role", "user")),
                    content=str(message.get("content", "")),
                    tool_calls=tool_calls,
                )
            raise TypeError(f"unsupported session message type: {type(message)!r}")

        kernel_sessions = [
            KernelSessionData(
                session_id=s.session_id,
                agent_id=s.agent_id,
                messages=[_to_kernel_message(m) for m in s.messages],
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
        synth_msgs = action.messages or []
        synth_system = "\n\n".join(m.content for m in synth_msgs if m.role == "system")
        synth_turns = [m for m in synth_msgs if m.role != "system"]
        synth_context = RenderedContext(system_text=synth_system, turns=synth_turns)
        async for evt in self._provider.stream(synth_context, [], extensions=None):
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
