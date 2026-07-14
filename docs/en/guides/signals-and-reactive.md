# Signals & Reactive Session

Signals are the Agent OS **Attention / Signal Plane**. Cron jobs, webhooks, user interrupts, and peer events do not rewrite history directly; they enter `state_turn`, and the runner presents them to the right agent on the next turn.

**Source code:**
- `python/deepstrike/signals/gateway.py`
- `python/deepstrike/runtime/reactive_session.py`
- Kernel: `crates/deepstrike-core/src/signals/`

---

## Agent OS Positioning

| Component | OS semantics |
|-----------|--------------|
| `SignalGateway` | External event queue for schedule / ingest / recipient filtering |
| `state_turn` | Current-turn attention input, separated from long-lived history |
| `ReactiveSession` | Shared blackboard, SignalGateway, and RunGroup budget for multiple agents |
| `TurnPolicy` | Decides which agent responds to which event |

The signal plane answers "how does the outside world interrupt or wake an agent?" ReactiveSession answers "how do multiple agents coordinate inside one governance domain?"

![Signals & Reactive Mechanisms](/signals_mechanisms.svg)

## Concept

```
SignalGateway
  ├── schedule(ScheduledPrompt)  # cron
  ├── ingest(RuntimeSignal)      # webhook
  ├── claim_signal(recipient?)   # leased consumption
  ├── ack_signal / nack_signal   # confirm or redeliver
  └── next_signal(recipient?)    # compatibility claim-and-ack API

ReactiveSession
  ├── RunGroup        # shared budget
  ├── EventStream     # blackboard
  ├── SignalGateway   # recipient routing
  └── TurnPolicy      # who responds to which event
```

Signals land in the kernel context **signals partition** (`state_turn`).

---

## Level 1: Scheduled prompt

```python
import time
from deepstrike import SignalGateway, ScheduledPrompt, RuntimeOptions, RuntimeRunner

gateway = SignalGateway()
gateway.schedule(ScheduledPrompt(
    goal="Check deployment status and report",
    run_at_ms=int(time.time() * 1000) + 60_000,
))

runner = RuntimeRunner(RuntimeOptions(
    ...,
    signal_source=gateway,
))

async for event in runner.run("Start monitoring"):
    ...
```

---

## Level 2: Webhook injection

```python
from deepstrike import RuntimeSignal

# In an HTTP handler
gateway.ingest(RuntimeSignal(
    kind="external",
    payload={"event": "deploy_done", "version": "1.2.3"},
))
```

---

## Level 3: Recipient routing and delivery leases

When one gateway serves multiple peers, the runner claims by `recipient` and acknowledges only
after the kernel accepts the signal. Errors or a lost lease trigger a negative acknowledgement.
An unacknowledged claim becomes visible again after expiry, so a consumer crash does not immediately
lose the signal.

For manual consumption:

```python
claim = await gateway.claim_signal(recipient="analyst-1")
if claim is not None:
    try:
        await handle(claim.signal)
        await gateway.ack_signal(claim)
    except Exception:
        await gateway.nack_signal(claim)
        raise
```

The destructive compatibility API remains available:

```python
sig = await gateway.next_signal(recipient="analyst-1")
```

`SignalGateway` is the process-local default. Cross-process or restart recovery requires a durable
store implementing the same `LeasedSignalSource` contract and atomically owning claim / ack / nack.

Test: `python/tests/test_signal_addressing.py`

---

## Level 4: ReactiveSession

```python
from deepstrike import (
    ReactiveSession, ReactivePeerSpec, RunGroup,
    InMemoryGroupBudgetStore, react_by_mention,
)

group = RunGroup(id="team-1", budget_store=InMemoryGroupBudgetStore())
session = ReactiveSession(
    run_group=group,
    turn_policy=react_by_mention,
    make_runner=make_runner_fn,
    signal_gateway=SignalGateway(),
)

await session.emit(BlackboardEvent(author="user", text="@analyst analyze this data"))
reactions = await session.run_turns()
```

Each persona is one `runner.run(session_id=...)`; continuity comes from `SessionLog`.

Stateless-friendly: `emit` can run in an HTTP handler; `resume` rebuilds the peer set from `RunGroup` membership.

```python
# python/tests/test_reactive_session.py pattern
resumed = await ReactiveSession.resume(
    run_group=run_group,
    turn_policy=turn_policy,
    make_runner=make_runner,
)
```

---

## Kernel behavior

- Signals are rendered in `state_turn` each turn (not cached with history)
- `TurnPolicy` decides which peer runs on each blackboard event
- `RunGroup` enforces shared token/spawn budget across peers

---

## Further reading

- [Sub-Agents & Collaboration](./sub-agents-and-collaboration)
- [RunGroup budget](/en/concepts/run-group-budget)
- Test: `python/tests/test_reactive_session.py`
