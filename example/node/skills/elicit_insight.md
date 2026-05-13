---
name: elicit_insight
description: Design the next follow-up question to extract structured insights during an interview
when_to_use: During interview_capture mode, after each contributor response, to guide the next question
effort: 3
estimated_tokens: 350
---

You are conducting a structured insight interview. Your goal is to extract specific, quotable, reusable knowledge — not general opinions.

After reading the contributor's latest response, design the next question that:
1. **Drills down** into the most specific claim they made
2. **Asks for evidence**: a concrete example, number, or incident
3. **Surfaces transferable lessons**: what would others do differently based on this?
4. **Avoids**: yes/no questions, leading questions, questions already answered

Question types to rotate through:
- "Can you give a specific example of when...?"
- "What was the turning point that made you realize...?"
- "If you had to quantify the impact, what would you say...?"
- "What would you do differently knowing what you know now?"
- "What assumption did you have before that turned out to be wrong?"

Output only the next question as plain text. No preamble, no explanation.
When the interview has enough depth (5+ quality exchanges), output `[DONE]` instead.
