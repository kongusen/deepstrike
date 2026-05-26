# UTF-8 truncation in context render/compression

## Summary

`crates/deepstrike-core/src/context/renderer.rs` (render budget path) and
`compression.rs` (`SnipCompactor`) previously sliced strings with raw byte
indices (`&text[..keep_chars]`). When the cut lands inside a multi-byte UTF-8
scalar (common for CJK content in quant/research agents), Rust panics:

```text
thread '…' panicked at 'byte index N is not a char boundary; it is inside …'
```

## Root cause

| Location | Old behavior |
|----------|----------------|
| `renderer.rs:~101` | `keep_chars = text.len() * remaining / tokens` then `&text[..keep_chars]` |
| `compression.rs:~29` | `&text[..self.max_chars]` in `SnipCompactor` |

Both treat **byte length** as truncatable without aligning to `char` boundaries.

## Fix (kernel)

Centralized helpers in `context/text.rs`:

- `truncate_bytes_at_char_boundary`
- `truncate_with_suffix`
- `proportional_byte_keep` (render path)

All compression/render cuts must go through these helpers (or
`deepstrike-tokenizer::Tokenizer::truncate` for token-budget paths).

## Application workaround (pre-upgrade SDK)

Session replay can defensively sanitize `llm_completed.content` before
`preloadHistory`:

- Python: `deepstrike.runtime.replay_sanitize.sanitize_replay_text`
- Node/WASM: `sanitizeReplayText` in `runtime/replay-sanitize.ts`
- Rust: `sanitize_replay_text` in `runtime/replay.rs`

This reduces pressure on the render/compress path for very long CJK transcripts
and remains useful as defense-in-depth after kernel fix.

## Compression roadmap (systematic)

Current pipeline (history partition only):

1. **SnipCompact** (ρ > 0.70) — byte cap snip (now char-safe)
2. **MicroCompact** (ρ > 0.80) — tool-result placeholders
3. **ContextCollapse** (ρ > 0.90) — drop oldest turns
4. **AutoCompact** (ρ > 0.95) — single placeholder

Follow-ups:

- Unify byte-cap and token-cap truncation via `deepstrike-tokenizer`
- Snip by **token budget**, not byte `max_chars`, for provider alignment
- Record compression artifacts in SessionLog (summary body optional)
- Render and compress must share one truncation module (no duplicate logic)

## Affected versions

- **Reported on:** `@deepstrike/sdk@0.1.16`
- **Fixed in:** deepstrike-core (post-0.1.16)
