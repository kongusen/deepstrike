# Multimodal Input

DeepStrike accepts **image and audio** input alongside text. The kernel's content model is a typed union of content parts, so a user turn can mix text with images and audio; the host serializes each part into the shape the target vendor expects. Images are supported on every provider; audio is supported where the vendor's API accepts it and rejected (never silently dropped) everywhere else.

**Code entry points**:

- `node/src/types.ts` — `ContentPart`, `TextPart`, `ImagePart`, `AudioPart`
- `node/src/providers/base.ts` — per-vendor serialization + `UnsupportedModalityError`
- `node/src/runtime/kernel-step.ts` — content-part ↔ kernel serde
- `python/deepstrike/providers/base.py`, `python/deepstrike/runtime/kernel_step.py` — Python mirrors

## Agent OS Positioning

| Responsibility | Description |
|----------------|-------------|
| To the kernel | The kernel carries typed content parts and counts their token weight; it never touches the vendor wire format |
| To the host | Providers serialize each part into vendor-native blocks (Anthropic image blocks, OpenAI `image_url` / `input_audio`, Gemini `inlineData`) |
| To pressure | Images and audio contribute real token weight to ρ, so compaction sees their cost instead of treating them as free |
| To replay | Attachments persist in the session log and are restored on resume, so a crashed multimodal run recovers the image |

![Multimodal input across context pressure, provider serialization, and replay](/multimodal_mechanisms.svg)

Multimodal input is a content-shape concern, not a routing one: [Provider Routing](./provider-routing) selects *which* vendor answers, while this guide covers *how* non-text content reaches that vendor.

## Level 1: Send an image with the run

Pass a `ContentPart[]` as `attachments`. They are seeded into history before the first render, so the model sees them on turn one.

```typescript
// Node
const png = "iVBORw0KGgo..." // base64 (no data: prefix)
await collectText(runner.run({
  sessionId: "vision",
  goal: "Describe the attached image.",
  attachments: [{ type: "image", data: png, mediaType: "image/png" }],
}))
```

```python
# Python
await collect_text(runner.run(
    goal="Describe the attached image.",
    session_id="vision",
    attachments=[{"type": "image", "data": png, "media_type": "image/png"}],
))
```

`run({ attachments })` ingress is available in **all four SDKs** (Node, Python, Rust `run_streaming_with_attachments`, WASM).

## Level 2: Content-part shapes

| Part | Fields | Notes |
|------|--------|-------|
| `image` | `url?` \| `data?` (base64), `mediaType?`, `detail?` | `url` and `data` are mutually exclusive; `detail` is `"low" \| "auto" \| "high"` |
| `audio` | `data` (base64), `mediaType` | base64 only — audio has no URL form |

`mediaType` defaults to `image/png` when omitted on a data image. A part with neither `url` nor `data` carries no content.

## Level 3: Per-vendor serialization

Each provider maps parts to its own wire format, or raises `UnsupportedModalityError` — it never sends a silent placeholder:

| Vendor | image | audio |
|--------|-------|-------|
| Anthropic | `source` block (base64 / url) | `UnsupportedModalityError` |
| OpenAI (chat) | `image_url` | `input_audio` (`mp3`/`wav`) |
| OpenAI (Responses) | `input_image` | `UnsupportedModalityError` |
| Gemini | `inlineData` / `fileData` | `inlineData` |
| Ollama | `images[]` | `UnsupportedModalityError` |

A model's advertised `modalities.input` (in `profiles.ts`) only lists what the content model can actually deliver — there is no `video`/`pdf` content part, so those are never advertised.

## Level 4: Token accounting

The context-pressure gate is not blind to attachment cost. `ContentPart.estimate_tokens` weighs an image by its detail tier and audio by its decoded byte length:

| Part | Token estimate |
|------|----------------|
| image `detail: "low"` | 85 |
| image `detail: "auto"` (default) | 255 |
| image `detail: "high"` | 680 |
| audio | ≈ decoded-bytes / 1600 |

So a large image genuinely raises ρ and can trigger compaction, instead of being counted as a single structural token.

## Level 5: Attachments survive resume

Attachments are persisted in the `run_started` event. On a crash-and-resume the live seed is skipped (the run is mid-flight), so history is rebuilt from the log — the reconstruction restores the attachments as a `Content::Parts` turn rather than flattening to text. A resumed multimodal run still sees its image. See [Session, Replay & Recovery](./session-replay-and-recovery).

## Boundaries

- **No video or document parts.** The content model is text / image / audio only; there is no `video` or `pdf`/`document` content part in any SDK.
- **Input only.** Providers serialize multimodal *requests*; parsing multimodal *output* (an image the model returns) is not wired — assistant turns are text + tool calls.
- **Unsupported ⇒ error, not drop.** Sending audio to a vendor that cannot take it raises `UnsupportedModalityError` so the failure is visible, never a silent `[audio: …]` placeholder.

## Kernel / Host Boundary

| Kernel (`deepstrike-core`) | Host SDK |
|----------------------------|----------|
| Typed `Content::Parts`; token weighting; attachment persistence + reconstruction | Vendor serialization; `run({ attachments })` ingress; base64 encoding |

## Further reading

- [Provider Routing](./provider-routing) — selecting the vendor that answers
- [Session, Replay & Recovery](./session-replay-and-recovery) — how attachments survive a resume
