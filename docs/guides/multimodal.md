# 多模态输入

DeepStrike 在文本之外还接受**图像和音频**输入。kernel 的内容模型是一个类型化的 content part 联合体，因此一个 user turn 可以把文本和图像、音频混合在一起；宿主再把每个 part 序列化成目标厂商所期望的形态。图像在每个 provider 上都受支持；音频只在厂商 API 接受它的地方受支持，其他地方会被拒绝（绝不会被静默丢弃）。

**代码入口**：

- `node/src/types.ts` — `ContentPart`、`TextPart`、`ImagePart`、`AudioPart`
- `node/src/providers/base.ts` — per-vendor 序列化 + `UnsupportedModalityError`
- `node/src/runtime/kernel-step.ts` — content-part ↔ kernel serde
- `python/deepstrike/providers/base.py`、`python/deepstrike/runtime/kernel_step.py` — Python 对应实现

## 在 Agent OS 中的位置

| 职责 | 说明 |
|------|------|
| 对 kernel | kernel 携带类型化的 content part 并计入它们的 token 权重；它绝不接触厂商的 wire format |
| 对 host | provider 把每个 part 序列化成 vendor-native 块（Anthropic image block、OpenAI `image_url` / `input_audio`、Gemini `inlineData`） |
| 对 pressure | 图像和音频会为 ρ 贡献真实的 token 权重，因此压缩看到的是它们的成本，而不是把它们当作免费 |
| 对 replay | attachment 会持久化进 session log，并在恢复时还原，因此崩溃的多模态 run 能把图像找回来 |

多模态输入是内容形态问题，不是路由问题：[Provider 路由](./provider-routing) 决定*哪个*厂商来回答，而本指南讲的是非文本内容*如何*到达那个厂商。

## Level 1：随 run 发送图像

传入一个 `ContentPart[]` 作为 `attachments`。它们会在首次渲染前被 seed 进 history，因此模型在第一轮就能看到它们。

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

`run({ attachments })` 入口在**全部四个 SDK**（Node、Python、Rust `run_streaming_with_attachments`、WASM）中都可用。

## Level 2：content-part 形态

| Part | 字段 | 说明 |
|------|------|------|
| `image` | `url?` \| `data?`（base64）、`mediaType?`、`detail?` | `url` 与 `data` 互斥；`detail` 为 `"low" \| "auto" \| "high"` |
| `audio` | `data`（base64）、`mediaType` | 仅 base64——音频没有 URL 形式 |

在 data image 上省略 `mediaType` 时默认为 `image/png`。既没有 `url` 也没有 `data` 的 part 不携带任何内容。

## Level 3：per-vendor 序列化

每个 provider 把 part 映射到自己的 wire format，或抛出 `UnsupportedModalityError`——它绝不会发送静默的占位符：

| 厂商 | image | audio |
|------|-------|-------|
| Anthropic | `source` 块（base64 / url） | `UnsupportedModalityError` |
| OpenAI (chat) | `image_url` | `input_audio`（`mp3`/`wav`） |
| OpenAI (Responses) | `input_image` | `UnsupportedModalityError` |
| Gemini | `inlineData` / `fileData` | `inlineData` |
| Ollama | `images[]` | `UnsupportedModalityError` |

模型在 `profiles.ts` 中声明的 `modalities.input` 只列出内容模型实际能交付的东西——不存在 `video`/`pdf` content part，因此它们永远不会被声明。

## Level 4：token 计量

context-pressure gate 并非对 attachment 成本视而不见。`ContentPart.estimate_tokens` 按图像的 detail 档位、按音频解码后的字节长度来加权：

| Part | token 估算 |
|------|-----------|
| image `detail: "low"` | 85 |
| image `detail: "auto"`（默认） | 255 |
| image `detail: "high"` | 680 |
| audio | ≈ 解码字节数 / 1600 |

因此一张大图会真实抬高 ρ 并可能触发压缩，而不是被算作单个结构 token。

## Level 5：attachment 在恢复后仍在

attachment 会持久化进 `run_started` 事件。在崩溃并恢复时，实时 seed 会被跳过（run 正处于半途），因此 history 从日志重建——重建过程会把 attachment 还原为一个 `Content::Parts` turn，而不是拍平成文本。恢复后的多模态 run 仍然看得到它的图像。参见 [Session、Replay 与恢复](./session-replay-and-recovery)。

## 边界

- **没有 video 或 document part。** 内容模型只有 text / image / audio；任何 SDK 里都没有 `video` 或 `pdf`/`document` content part。
- **仅输入。** provider 序列化的是多模态*请求*；解析多模态*输出*（模型返回的图像）并未接入——assistant turn 是文本 + tool call。
- **不支持 ⇒ 报错，不丢弃。** 把音频发给无法接受它的厂商会抛出 `UnsupportedModalityError`，让失败可见，而不是变成静默的 `[audio: …]` 占位符。

## Kernel / Host 边界

| Kernel (`deepstrike-core`) | Host SDK |
|----------------------------|----------|
| 类型化 `Content::Parts`；token 加权；attachment 持久化 + 重建 | 厂商序列化；`run({ attachments })` 入口；base64 编码 |

## 延伸阅读

- [Provider 路由](./provider-routing) — 选择回答的厂商
- [Session、Replay 与恢复](./session-replay-and-recovery) — attachment 如何在恢复后存活
