---
name: find_connections
description: Find and annotate connections between a new note and existing archive entries
when_to_use: After search_archive returns results and you need to evaluate relevance and annotate links
effort: 3
estimated_tokens: 400
---

Given a new note and a list of candidate archive entries, identify genuine connections.

Steps:
1. Read the new note's core claim or topic
2. For each search result, assess whether it:
   - Shares a core concept (strong connection)
   - Provides evidence for or against the note (supporting/contradicting)
   - Is a prerequisite or follow-up topic (chain connection)
   - Is merely tangentially related (weak — skip)
3. Keep only strong/supporting/chain connections
4. Return the note JSON with the `connections` array populated

Output the updated note JSON with connection IDs filled in:
```json
{
  "type": "...",
  "tags": [...],
  "summary": "...",
  "connections": ["note_id_1", "note_id_2"]
}
```

If no genuine connections exist, mark `"connections": []` and add tag `#island`.
