---
name: classify_and_tag
description: Classify a note and assign structured tags, summary, and type
when_to_use: When processing any new note or thought that needs to be organized into the knowledge base
effort: 2
estimated_tokens: 300
---

Organize the given note into a structured JSON entry for the knowledge base.

Steps:
1. Determine the **type**: one of `idea`, `article`, `task`, `reference`, `insight`, `research`
2. Assign **2–5 tags** in `#tag` format (lowercase, no spaces)
3. Write a **one-line summary** ≤80 characters — specific and quotable, not generic
4. List **connections**: IDs of related notes already found via search_archive (empty array if none)

Output strictly this JSON (no markdown fence needed):
```json
{
  "type": "<type>",
  "tags": ["#tag1", "#tag2"],
  "summary": "<summary ≤80 chars>",
  "connections": []
}
```

Rules:
- Summary must be concrete: include a specific claim, number, or judgment
- Reject vague summaries like "interesting article about AI" — say what specifically
- Tags should reflect topic, domain, and action-type where relevant (e.g. #toread, #todo)
