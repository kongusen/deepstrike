---
name: synthesize_cluster
description: Synthesize a cluster of related notes into a single higher-order insight
when_to_use: When 3+ notes share a common theme and can be fused into a distilled insight
effort: 5
estimated_tokens: 600
---

Given a set of related notes on the same theme, synthesize them into a single insight note.

Steps:
1. Identify the central tension, pattern, or claim that unites the notes
2. Extract the strongest 2–3 supporting points across all notes
3. Note any contradictions or open questions
4. Write a synthesis note in this structure:
   - **Core claim**: one sentence
   - **Evidence**: 2–3 bullet points citing source note IDs
   - **Open questions**: what's still unresolved
5. Assign appropriate tags and a high-quality summary

Output JSON:
```json
{
  "type": "insight",
  "tags": ["#synthesis", "#tag1", "#tag2"],
  "summary": "<core claim ≤80 chars>",
  "connections": ["source_note_id_1", "source_note_id_2"],
  "body": "**Core claim**: ...\n\n**Evidence**:\n- ...\n\n**Open questions**: ..."
}
```
