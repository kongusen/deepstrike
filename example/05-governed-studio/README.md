# L5 · Governed studio — the control plane

L1's agent, now behind a **policy**. Authority moves out of the prompt and into declarative,
kernel-enforced rules the model cannot argue with.

```
 model wants a tool ─▶ governancePolicy ─┬─ allow ───────────────▶ runs
                                         ├─ deny  ─▶ pre-filtered from schema (model never sees it)
                                         └─ ask_user ─▶ PermissionRequestEvent ─▶ onPermissionRequest (host decides)

 every spawn / write ─▶ resourceQuota (hard caps: concurrency · depth · cumulative · write-rate)
 after the run       ─▶ rebuildOsSnapshotFromSessionEvents ─▶ audit of what the kernel enforced
```

## What you learn here

| Mechanism | Where it shows up |
|---|---|
| **Deny (schema pre-filter)** | `{ pattern: "publish_public", action: "deny" }` — the tool is stripped from the schema before the provider call. The model never sees it, so there's no rollback turn and no way to try. |
| **ask_user (permission gate)** | `{ pattern: "email_editor", action: "ask_user" }` pauses at call time with a `PermissionRequestEvent`; `onPermissionRequest` returns `{approved}`. The gate is **tool-scoped** — the kernel surfaces the tool name + reason, not the call args — so the host decides per *capability*. |
| **Resource quota** | `resourceQuota` bounds `maxConcurrentSubagents` / `maxSpawnDepth` / `maxTotalSubagents` / memory-write rate. No sub-agents fire here, but the same caps bound L7/L8's fan-out. |
| **OS profile + snapshot** | `osProfile("native")` resolves the concrete kernel policy defaults; `rebuildOsSnapshotFromSessionEvents` reconstructs the enforced reality (tool-gated count, signals, memory ops) from the durable log — an audit trail, not a claim. |

## Authority lives with the host

The load-bearing idea: the prompt asks the model to notify the editor and *not* to publish, but the
prompt is not what stops it. `publish_public` is **absent** from the toolset (policy), and
`email_editor` only fires because `onPermissionRequest` **approved** it. Swap the host verdict to
`false` and the exact same model, same prompt, cannot send. Policy is enforced below the model.

## Run

```sh
npx tsx 05-governed-studio/main.ts            # deny + ask_user gates fire; OS snapshot printed
npx tsx 05-governed-studio/main.ts --dry-run  # wiring only
../../python/.venv/bin/python 05-governed-studio/main.py   # the Python mirror
```

You'll see one `[⚖ ask_user … APPROVED by studio-host]` gate, the notification tool run, and an OS
snapshot reporting `tool-gated (ask_user): 1` — while `publish_public` never once appears.

## What's next

**L6 · Loop agent** turns the single bounded run into a *self-pacing* one: `runLoop` replays one
stable session across rounds, and after each round the model proposes a pace verb —
continue / sleep / stop — that the kernel adjudicates. Silence means done.
