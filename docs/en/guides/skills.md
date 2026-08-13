# Skills

Skills are reusable packages of instructions, knowledge, and optional tool access. They let an Agent load a specialized way of working only when the current task needs it.

**Source code:**
- Runtime: `crates/deepstrike-core/src/context/skill_catalog.rs`
- SDK: `python/deepstrike/skills/registry.py`

---

## What a skill changes for an Agent

| Benefit | Behavior |
| --- | --- |
| --- | --- |
| Smaller default prompt | Only skill summaries are available until the Agent loads one. |
| Specialized instructions | The skill body becomes active context for the current task. |
| Focused tool access | `allowed_tools` can narrow the tools visible during that phase. |
| Phase-based work | Different skills can be loaded and released as the task changes. |

The value of a skill is not another Markdown file. It is a reusable capability boundary that keeps an Agent focused.

![Skills Mechanisms](/skills_mechanisms.svg)

## Concept

1. `SkillRegistry.scan()` scans `*.md` files, parses YAML frontmatter → `SkillMetadata`
2. The Agent sees an available-skills summary and can request a skill by name
3. The agent calls `skill(name="...")` → SDK reads the file body → returns it as a tool result
4. After loading, the skill enters `active_skills` and **narrows** the exposed tool set via `allowed_tools`

```python
# python/deepstrike/skills/registry.py
class SkillRegistry:
    """Scans a directory of .md skill files and registers them with the runtime."""

    def scan(self) -> list[SkillMetadata]:
        skills = []
        for path in self._dir.glob("*.md"):
            text = path.read_text(encoding="utf-8")
            meta = _parse_frontmatter(text)
            name = meta.get("name") or path.stem
            skills.append(SkillMetadata(
                name=str(name),
                description=str(meta.get("description", "")),
                when_to_use=str(meta.get("when_to_use", "")) or None,
                allowed_tools=_parse_tool_list(meta.get("allowed_tools")) or None,
                ...
            ))
        return skills
```

---

## Level 1: Directory scan

Create a skill file `skills/code-review.md`:

```markdown
---
name: code-review
description: Review code for bugs and style issues
when_to_use: When reviewing pull requests
allowed_tools: read_file
---

# Code Review Skill

1. Read the target files
2. Check for bugs, security issues, style
3. Output structured findings
```

Enable scanning:

```python
RuntimeOptions(
    ...,
    skill_dir="./skills",
)
```

The runner scans at startup and `register_skills` with the kernel.

---

## Level 2: Stable core tools

After skill gating, only skill-declared tools plus meta-tools are exposed by default. To always keep baseline tools:

```python
RuntimeOptions(
    ...,
    skill_dir="./skills",
    stable_core_tool_ids=["read_file", "grep"],
)
```

Maps to runtime `ContextManager.stable_core_tools`.

---

## Level 3: Tool-gating telemetry

```python
def on_metrics(m: TurnMetrics):
    print(f"turn={m.turn} skill={m.active_skill} exposed={m.tools_exposed} called={m.tools_called}")

RuntimeOptions(..., on_turn_metrics=on_metrics)
```

Compare `tools_exposed` vs `tools_called` to quantify over-exposure; consecutive turns with the same `active_skill` measure dwell time.

---

## Level 4: Deactivation and leases (K3)

Activation is no longer a one-way ticket. In long multi-phase runs, an early phase's skill body
does not have to occupy the knowledge slot forever:

```python
RuntimeOptions(..., skill_lease_turns=8)   # auto-deactivate 8 turns after each activation
runner.deactivate_skill("code-review")     # or explicit host-driven unload
```

- After deactivation the toolset **re-widens** at the next provider call (an epoch event, same
  cache cost class as activation)
- The skill body's knowledge pin (key `skill:<name>`) is dropped at the next compaction/renewal
  boundary (cache-safe)
- A later `skill(name)` call re-activates and re-pins fresh content
- **Deliberately no model-facing unload tool** — deactivation is host-driven only, avoiding
  load/unload thrash
- Lease expiry and explicit deactivation share one path; the sweep runs on the same cadence as
  capability leases (head of every event)

---

## Level 5: SkillMetadata fields

| Field | Description |
|-------|-------------|
| `name` | Unique identifier |
| `description` | Appears in catalog XML |
| `when_to_use` | Optional; helps model selection |
| `allowed_tools` | Tool ids allowed after load |
| `effort` | Optional difficulty hint |
| `estimated_tokens` | Token estimate (default `len/4`) |

---

## Runtime behavior

- The catalog **does not store body text** — only `build_tool_schema()` generates the meta-tool
- `active_skills` is a `BTreeMap<name, Option<expires_at_turn>>` (multiple skills union their tools; deactivation/leases since K3 — see Level 4)
- A successfully loaded skill body is additionally pinned into the knowledge partition (key `skill:<name>`; runtime upsert dedupes across wakes)
- Skills are meta-tools and do not count toward `recent_actions` progress log

---

## Further reading

- [Context Engineering](./context-engineering)
- Cursor Agent Skills follow a similar pattern; DeepStrike gates tools in the runtime
