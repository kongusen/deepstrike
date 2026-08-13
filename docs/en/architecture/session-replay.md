# Sessions and Recovery

An Agent session is the durable thread of work identified by `sessionId`. It contains the goal, turns, tool activity, approvals, signals, memory activity, workflow progress, and enough recovery state to continue after an interruption.

## What a session gives you

| Need | Session behavior |
| --- | --- |
| Continue a conversation | Reuse the same `sessionId` and the Agent sees the prior useful context. |
| Recover an interrupted run | Start the same session again; pending work and the last durable boundary are restored. |
| Wait for a person or event | Suspend on approval, child completion, or an external signal and resume later. |
| Debug a decision | Inspect structured events for model output, tool calls, policy decisions, and results. |
| Test without a provider | Replay fixed provider responses and assert the Agent's decisions deterministically. |

## Session implementations

| Type | Use |
| --- | --- |
| `InMemorySessionLog` | Local experiments and tests. |
| `FileSessionLog` | Local applications that need continuity across process restarts. |
| Custom `SessionLog` | Applications that store sessions in a database or service. |

## Resume a session

```ts
await collectText(runner.run({
  sessionId: "research-42",
  goal: "Continue the source review and finish the brief.",
}))
```

Use the same `sessionId` after a process restart. The application does not need to rebuild the conversation by hand or invent a special “resume” prompt.

## Durable memory is separate

Session history answers “what happened in this run?” Durable memory answers “what should this Agent remember for future runs?” Use [`MemoryStore`](../guides/memory) for facts and preferences worth carrying forward; keep transient tool output in the session.

## Further reading

- [Long-running sessions guide](../guides/session-replay-and-recovery)
- [Context and multimodal input](../guides/context-engineering)
- [Evaluation and replay](../guides/harness-and-eval)
