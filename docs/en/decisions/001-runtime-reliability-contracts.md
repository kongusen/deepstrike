# ADR-001: Runtime Reliability Contracts

## Status

Accepted

## Date

2026-07-14

## Context

Several SDK mechanisms currently expose stronger semantics than their storage or lifecycle
boundaries can enforce:

- group budgets read cumulative usage before a run and charge it afterwards, so concurrent runs can
  admit against the same remaining budget;
- signals are destructively dequeued without a delivery lease, while an unaddressed signal is
  described as a broadcast even though only one consumer receives it;
- event and signal listeners run after state is committed, but a listener exception is returned to
  the caller as if the commit failed;
- background enrichment tasks outlive the mutable runner state that supplied their session and run
  identity;
- file-backed session logs assign sequence numbers outside a serialized append boundary.

Fixing these independently would create several incompatible implementations of atomicity,
cancellation, and error handling. The SDK needs shared reliability contracts first, followed by
incremental migration of the mechanisms that use them.

## Decision

### 1. Separate required mutations, decisions, and observers

- A required mutation succeeds only after its durable state is committed.
- A decision hook fails closed unless its public contract explicitly configures another policy.
- An observer never changes the result of an already committed mutation. Observer failures are
  isolated and sent to a structured error reporter.

### 2. Make delivery semantics explicit

An unaddressed signal is a **shared** queue item: one eligible consumer receives it. It is not a
broadcast. A broadcast is explicit fan-out to a known recipient set, producing one addressed queue
item per recipient.

Durable signal/event stores will use `claim -> ack | nack` leases. The current in-memory gateway
keeps its pull API for compatibility while adopting the same shared-versus-broadcast terminology.

### 3. Make sequencing and budgets atomic at their owning store

`SessionLog.append` owns per-session sequence allocation and append ordering. Built-in stores must
serialize concurrent appends for a session.

Cross-run budget enforcement will use `reserve -> settle | release`. A store that implements only
`read -> charge` is a legacy accounting store and must not claim concurrent quota enforcement.

### 4. Carry immutable operation identity

Execution and background work receive an immutable operation context containing run/session/agent
identity, cancellation, deadline, and provenance. Background work must not recover identity from
mutable runner-wide fields after it has been scheduled.

### 5. Own every asynchronous task

Fire-and-forget work is replaced by a managed task scope owned by a run. A scope records task
failures and is explicitly drained or cancelled before the run releases its state.

### 6. Keep the migration additive

Public contracts are extended before legacy paths are deprecated. Node and Python receive matching
contract tests. Each migration slice must leave both SDKs buildable and preserve unrelated behavior.

## Migration slices

1. Serialize file session-log appends; isolate observer failures; implement explicit signal fan-out;
   validate skill identifiers at the shared loader boundary.
2. Introduce operation context and managed task scope; migrate execution planes and memory
   enrichment.
3. Introduce budget reservations and durable delivery leases; migrate RunGroup, SignalGateway, and
   ReactiveSession reaction checkpoints.
4. Remove heuristic control-plane fallbacks and legacy contracts after a documented deprecation
   window.

## Alternatives considered

### Patch each mechanism independently

Rejected because it duplicates locks, error callbacks, and cancellation rules while allowing their
semantics to drift again.

### Put all reliability behavior in RuntimeRunner

Rejected because stores own atomicity and adapters own external side effects. A runner-level lock
cannot provide cross-process sequencing, quota reservation, or remote idempotency.

### Break all public interfaces immediately

Rejected because existing custom stores and execution planes are observable public contracts.
Additive migration gives consumers a usable transition path and lets contract tests prove each
slice before removal of legacy behavior.

## Consequences

- Some previously swallowed observer failures become visible through structured reporting without
  changing the committed operation's result.
- True broadcast requires callers to provide or own a recipient set.
- Concurrent quota enforcement requires a transactional store capability; a plain append/read log
  remains useful for accounting but is no longer described as sufficient by itself.
- Run completion may wait for explicitly drainable background work, or cancel it according to the
  configured scope policy.
