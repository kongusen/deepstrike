# Skills

Skills are the Agent OS **Capability Plane**. They move capability instructions out of default context, load them through a meta-tool only when the agent asks, and then narrow the exposed tool surface.

**Source code:**
- Kernel: `crates/deepstrike-core/src/context/skill_catalog.rs`
- SDK: `python/deepstrike/skills/registry.py`

---

## Agent OS Positioning

| Responsibility | Description |
|----------------|-------------|
| To the Context VM | Keeps only skill metadata in stable context; bodies enter as needed via knowledge / tool result |
| To the tool plane | `allowed_tools` narrows visible tools and reduces accidental calls |
| To governance | Skill gating happens before schema exposure and composes with Governance |
| To long tasks | Different phases can load different capabilities instead of bloating the system prompt |

The value of a skill is not another Markdown file. It turns capability into an addressable, auditable, gateable OS resource.

![Skills Mechanisms](/skills_mechanisms.svg)

## Concept

1. `SkillRegistry.scan()` scans `*.md` files, parses YAML frontmatter → `SkillMetadata`
2. The kernel injects a `skill` meta-tool whose description embeds `<available_skills>` XML
3. The agent calls `skill(name="...")` → SDK reads the file body → returns it as a tool result
4. After loading, the skill enters `active_skills` and **narrows** the exposed tool set via `allowed_tools`

```python
# python/deepstrike/skills/registry.py
class SkillRegistry:
    """Scans a directory of .md skill files and registers them with the kernel."""

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

Maps to kernel `ContextManager.stable_core_tools`.

---

## Level 3: Tool-gating telemetry

```python
def on_metrics(m: TurnMetrics):
    print(f"turn={m.turn} skill={m.active_skill} exposed={m.tools_exposed} called={m.tools_called}")

RuntimeOptions(..., on_turn_metrics=on_metrics)
```

Compare `tools_exposed` vs `tools_called` to quantify over-exposure; consecutive turns with the same `active_skill` measure dwell time.

---

## Level 4: SkillMetadata fields

| Field | Description |
|-------|-------------|
| `name` | Unique identifier |
| `description` | Appears in catalog XML |
| `when_to_use` | Optional; helps model selection |
| `allowed_tools` | Tool ids allowed after load |
| `effort` | Optional difficulty hint |
| `estimated_tokens` | Token estimate (default `len/4`) |

---

## Kernel behavior

- The catalog **does not store body text** — only `build_tool_schema()` generates the meta-tool
- `active_skills` is a `BTreeSet`; v1 **only grows** (multiple skills union their tools)
- Skills are meta-tools and do not count toward `recent_actions` progress log

---

## Further reading

- [Context Engineering](./context-engineering)
- Cursor Agent Skills follow a similar pattern; DeepStrike gates tools at the kernel layer
