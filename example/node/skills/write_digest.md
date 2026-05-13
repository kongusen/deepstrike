---
name: write_digest
description: Generate a structured daily digest from a set of notes
when_to_use: When the user requests /export digest or a summary of recent notes
effort: 4
estimated_tokens: 500
---

Generate a well-structured digest from the provided notes.

Structure:
1. **Header**: date + note count
2. **Top insights** (max 5): the most interesting or actionable notes, with a one-line annotation
3. **By category**: group remaining notes by type
4. **Open tasks**: all notes with type=task, formatted as a checklist
5. **Emerging themes**: 2–3 tag clusters that appear frequently

Format as clean Markdown, suitable for reading in a text editor or sharing.

Example output:
```markdown
# FlashNote Digest — 2026-05-13
12 notes captured

## Top Insights
- MoE稀疏激活的核心是负载均衡损失，不是路由本身 #machinelearning
- vLLM vs SGLang: SGLang在长序列推理上快30% #benchmark

## By Category
### idea (4)
- ...

## Open Tasks
- [ ] 读完SGLang论文
- [ ] 整理推理引擎对比表

## Emerging Themes
- #llm-inference (6 notes)
- #startups (3 notes)
```
