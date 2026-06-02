# Context Slots & Compression

**Status:** Current (four-slot model)  
**Supersedes:** the earlier six-partition context design.  
**Related:** runtime performance work for token counting, prompt caching, and renderer behavior.

---

## Four-slot model

Context is organized around LLM API layout, not six narrative partitions.

```
RenderedContext
├── system_stable     Slot 1 — Identity (never changes within a run)
├── system_knowledge  Slot 2 — Knowledge (low-frequency changes)
└── turns
    ├── [0]           Slot 3 — State (task_state + signals, every call)
    └── [1..N]        Slot 4 — History (high-frequency, compression target)
```

| Slot | Kernel source | Change rate | Anthropic mapping |
|------|---------------|-------------|-------------------|
| 1 — `system_stable` | `partitions.system` | Never | `system[0]` + `cache_control: ephemeral` |
| 2 — `system_knowledge` | `partitions.knowledge` | Low | `system[1]` + `cache_control: ephemeral` |
| 3 — `turns[0]` | `task_state` + `signals` | Every turn | First user turn: goal, plan, progress, compression log, runtime signals |
| 4 — `turns[1..N]` | `partitions.history` | High | Conversation transcript |

OpenAI and other single-system-slot providers receive `system_text = system_stable + system_knowledge`.

### Removed partitions

The old model used `system + working + task_state + memory + skill + artifacts + history`. Removed:

| Old partition | New home |
|---------------|----------|
| `working` | `signals: Vec<String>` — ephemeral, folded into Slot 3 |
| `memory` | Slot 2 via `push_knowledge()` / `initialMemory` → `add_knowledge_message` |
| `skill` | Slot 2 when loaded; skill meta-tool results also appear in history |
| `artifacts` | Slot 2 or history references |
| `dashboard` | Dropped — task state in `task_state` only |

### Kernel APIs

| API | Target | Notes |
|-----|--------|-------|
| `push_knowledge(msg, tokens)` | Slot 2 | Durable, cacheable knowledge blocks |
| `AddKnowledgeMessage` (ABI) | Slot 2 | Host injects knowledge at startup or mid-run |
| `push_signal(text)` | Slot 3 | Rollback notes, interrupts, sub-agent summaries; cleared after render |
| `initialMemory` (SDK) | Slot 2 | Maps to `add_knowledge_message`, not deleted `add_memory_message` |

Meta-tool retrieval (`memory(query)`, `knowledge(query)`) returns land in **history** as tool results — the model needs them in the conversation flow.

---

## Pressure & compression ladder

Thresholds are fractions of `max_tokens`:

```
rho = observed_input_tokens / max_tokens   (falls back to estimate when no provider usage)

rho > 0.70  →  SnipCompact
rho > 0.80  →  MicroCompact
rho > 0.90  →  ContextCollapse
rho > 0.95  →  AutoCompact
rho > 0.98  →  Renewal (new sprint)
```

The pipeline is **cumulative**: at `MicroCompact`, Snip runs first, then Micro, stopping early if the token target is met.

**Invariant:** Only `history` is modified. Slots 1–3 are preserved across all compression tiers.

---

## Per-tier behavior

All four tiers append to `task_state.compression_log` via `CompressionEntry` — append-only, never overwritten.

### SnipCompact (`rho > 0.70`)

- **Target:** `Content::Text` assistant messages in history exceeding `snip_per_msg` (default 5% of `max_tokens`, floor 50t).
- **Action:** Head + tail truncation with `… [… N tokens omitted …] …`.
- **Log:** `[snip_compact]` entry with truncation stats (no semantic summary).
- **Skip:** `Content::Parts` (tool results).

### MicroCompact (`rho > 0.80`)

- **Target:** `Content::Parts` tool-result messages in history with token count > 200.
- **Action:** Replace with compact excerpt: `[tool result: id | name | Nt]\n<head>…<tail>`.
- **Log:** `[micro_compact]` entry with excerpt stats.
- **Skip:** Messages under 200t.

### ContextCollapse (`rho > 0.90`)

- **Target:** Oldest history messages, preserving last `preserve_recent_turns` (from `ContextConfig`, not hardcoded).
- **Action:** Drain oldest N messages until token count ≤ target.
- **Summary:** `RuleSummarizer` → `task_state.log_compression("context_collapse", summary)`.
- **Archive:** Removed messages returned in `LoopObservation::Compressed.archived`.

### AutoCompact (`rho > 0.95`)

- **Target:** All history except last K turns (`preserve_recent_turns` from config).
- **Action:** Drain everything except last K.
- **Summary:** Same summarizer with `auto_compact` label → `compression_log`.
- **Archive:** Removed messages in observation.

### Renewal (`rho > 0.98`, after compression)

Fires when rho remains above threshold even after compression.

- **Carries:** Slot 1 (`system`) + Slot 2 (`knowledge`) + `task_state` (goal, plan, progress, `compression_log`).
- **Clears:** `history` (keeps last `carryover_ratio × max_tokens` of turns); `signals` (ephemeral); `task_state.scratchpad` (content already in `progress` for cognitive continuity).
- **Does not carry:** signals, scratchpad.

---

## Summary routing

Collapse and Auto summaries no longer write to `scratchpad`. Both use the unified path:

```
CompressionEntry → task_state.compression_log
                 → task_state.format_compact()  (renders last 3 entries under compression_history:)
                 → build_state_turn()           (Slot 3 / turns[0])
                 → provider API
```

Snip and Micro also log to `compression_log` (action label only or with stats).

---

## Renderer behavior

### Token budget

```
remaining = max_tokens - system_stable_tokens - system_knowledge_tokens
```

History fills **newest-first** within `remaining`. The first `preserve_recent_msgs` history messages are always included. Text messages truncate at the budget boundary; Parts messages are included whole.

### Turn normalization (`normalize_turn_prefix`)

After AutoCompact, the preserved tail may be all `assistant`/`tool` with no leading user turn. The renderer inserts `[context resumed]` as a user anchor instead of silently dropping the tail.

### State turn (Slot 3)

`build_state_turn()` composes:

1. `task_state.format_compact()` — goal, plan, progress, blocked_on, compression_history
2. `signals.join("\n")` — rollback notes, interrupts, sub-agent summaries
3. Trailing `Proceed.` anchor

If the first history turn is `Content::Parts` (tool result) and signals need folding, a new user turn is inserted to carry them — signals are never dropped.

Signals are cleared after render; they are per-turn ephemeral.

---

## Partition state after each tier

| After tier | history | task_state.compression_log | signals | scratchpad |
|------------|---------|---------------------------|---------|------------|
| SnipCompact | text truncated in-place | +snip entry | unchanged | unchanged |
| MicroCompact | tool results excerpted | +micro entry | unchanged | unchanged |
| ContextCollapse | oldest N removed | +collapse summary | unchanged | unchanged |
| AutoCompact | all but last K removed | +auto summary | unchanged | unchanged |
| Renewal | carryover tail only | carried | cleared | cleared |

---

## Prompt caching (Anthropic)

Slot 1 and Slot 2 map to separate Anthropic system blocks with `cache_control: ephemeral`. Because `task_state` and `signals` render into `turns[0]` — not the system prefix — the cacheable prefix stays stable across turns.

See the provider guide for adapter behavior and prompt-caching notes.
