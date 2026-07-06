---
name: citation-style
description: Format every claim in the studio's house citation style before answering.
when_to_use: When the task asks for a written brief or answer that must carry citations.
allowed_tools: format_citation
---

# Citation-style skill

The studio's house style for a sourced brief:

1. Make each factual claim its own sentence.
2. After a claim, cite with the `format_citation` tool — never hand-write a citation.
   Pass the source `id` you read; the tool returns the canonical `[Title — id]` form.
3. Never cite a source you have not `read_source`'d in this run.
4. Close the brief with a `Sources:` line listing every id you cited, in first-use order.

While this skill is active the toolset is narrowed to the citation tools plus the studio's
stable-core (`search`, `read_source`) — anything not needed for citing is hidden, so the model
cannot wander into unrelated tools mid-write.
