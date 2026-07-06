# L4 · Reactive desk — signals + attention policy

L1's agent, now **open to the outside world**. Until here the agent only saw its goal and its own
tool results. Now external events reach a *running* loop and the kernel decides how much they should
interrupt.

```
 webhook / cron / job ──▶ gateway.ingest(sig) ─┐
                                               ├─▶ [pulled each turn] ─▶ kernel attention policy ─┐
 host monitor ─────────▶ runner.injectNote() ──┘        (by urgency)                              │
                                                    normal→queue · high→soft-interrupt · critical→preempt
                                                                                                  ▼
                                                                     surfaces to the model as a [SIGNAL] … line
```

## What you learn here

| Mechanism | Where it shows up |
|---|---|
| **SignalGateway** | A `SignalSource` you pass as `signalSource`. External code calls `gateway.ingest(sig)` (webhook) or `gateway.schedule(prompt)` (cron); the loop **pulls** the next signal at each turn boundary. One gateway can serve many peers via `recipient`. |
| **injectNote** | `runner.injectNote(text, urgency)` — the host's own channel onto the same stream, no full source needed. Use it to feed host-detected events back mid-run. |
| **Attention ladder** | Urgency maps to a kernel disposition: `normal` queues for the next boundary, `high` soft-interrupts, `critical` preempts. The agent never polls — the kernel drains and routes. |
| **[SIGNAL] surfacing** | A drained signal's `payload.goal` becomes the summary the model sees as a `[SIGNAL] …` line, plus a durable directive — so the reaction is visible and auditable. |

## The determinism trick

Signals only matter if they arrive *while the agent is working*. To make that reproducible without a
wall-clock race, both events fire as **side effects of the agent's own tool calls**: the wire alert
`ingest`s when the agent first `search`es, the editor's note `injectNote`s when it first
`read_source`s. In production these come from a webhook handler and a host monitor — the loop wiring
the agent sees is byte-identical; only the trigger moves off the tool call.

## Run

```sh
npx tsx 04-reactive-desk/main.ts            # two events arrive mid-run and reshape the brief
npx tsx 04-reactive-desk/main.ts --dry-run  # wiring only
../../python/.venv/bin/python 04-reactive-desk/main.py   # the Python mirror
```

In the live run the final brief acknowledges the **wire alert** ("a correction just landed") *and*
obeys the **high-urgency editor's note** (it names the queue / soft-interrupt / preempt ladder
explicitly) — two independent inbound channels, both folded in without the agent polling.

## What's next

**L5 · Governance + Quota + OS profile** puts a policy in front of the tools: a `Governance` verdict
gate (allow / deny / ask), a resource quota (token + subagent caps), and an OS-profile snapshot of
what the kernel is enforcing — the control plane made explicit.
