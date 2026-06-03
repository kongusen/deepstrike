---
name: stability-drill
description: Procedure for long-running DeepStrike SDK validation with ordered tool steps, checkpoints, memory retrieval, and large-result spooling.
when_to_use: Use before executing the node/examples long-running stability demo.
effort: 1
estimated_tokens: 520
---

# Stability Drill

Run the validation as an ordered loop. Record exactly one `record_step` result for
each step number before moving to the next number.

Early in the run, query both `knowledge` and `memory` so the context includes the
configured retrieval paths. Treat those retrieved snippets as guidance, not as
completion evidence.

At each checkpoint, call `verify_checkpoint` and inspect the returned JSON. If it
reports missing steps, fill the missing step numbers before continuing.

When asked to emit a large payload, call `emit_large_payload` after the matching
`record_step`. Do not paste the large payload into the final answer; summarize
whether it was emitted and whether the run continued normally afterwards.

The final answer should include:

- steps requested and steps recorded
- checkpoint status
- whether large payloads were emitted
- whether the run encountered timeout, budget, permission, or replay issues
