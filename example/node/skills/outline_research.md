---
name: outline_research
description: Create a research outline and search plan for a given topic before starting deep research
when_to_use: At the start of a /research session, before fetching any URLs
effort: 2
estimated_tokens: 250
---

Given a research topic, create a structured outline and search plan.

Steps:
1. Decompose the topic into 3–5 sub-questions
2. Identify what's already known (from archive search results if available)
3. List 3–5 specific search queries to run
4. Define success criteria: what a good research output looks like

Output:
```markdown
## Research Plan: <topic>

### Sub-questions
1. ...
2. ...

### Already known (from archive)
- ...

### Search queries
1. "<query 1>"
2. "<query 2>"

### Success criteria
- Covers at least 3 independent sources
- Includes concrete comparison (numbers, benchmarks, or case studies)
- Word count 600–1200
- Structure: TL;DR / comparison / conclusion / references
```
