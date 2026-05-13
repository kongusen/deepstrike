---
name: summarize_source
description: Summarize a fetched article or webpage into a structured research note
when_to_use: After fetch_and_clip returns content, to turn raw HTML/text into a usable note
effort: 3
estimated_tokens: 400
---

Given raw web content from fetch_and_clip, extract the key information for a research note.

Steps:
1. Identify the **main claim or finding** (1 sentence)
2. Extract **3–5 key points** — specific, data-backed where possible
3. Note the **source credibility**: author, publication, date if present
4. Flag any **limitations or biases** in the source
5. Rate **relevance to the research topic** (high/medium/low)

Output JSON for a research note:
```json
{
  "type": "research",
  "tags": ["#source", "#tag1", "#tag2"],
  "summary": "<main claim ≤80 chars>",
  "connections": [],
  "url": "<source URL>",
  "body": "**Main claim**: ...\n\n**Key points**:\n- ...\n\n**Source**: ...\n\n**Relevance**: high/medium/low"
}
```

If the content is irrelevant, paywalled, or too low quality, output:
```json
{ "skip": true, "reason": "<why>" }
```
