# Migrating to DeepStrike 0.2.61

DeepStrike 0.2.61 changes default routing for dual-protocol vendors and makes cache and textual
tool-call reporting fail-safe. The provider APIs remain available; applications that depended on an
implicit Anthropic-compatible route must pin it explicitly.

## Default provider routes

| Vendor | 0.2.61 default | Explicit compatibility path |
| --- | --- | --- |
| DeepSeek | OpenAI Chat-compatible | `protocol="anthropic"` / `deepseek.anthropic` |
| Kimi | region OpenAI Chat-compatible | region Anthropic endpoint |
| Qwen | DashScope OpenAI-compatible | `qwen.anthropic` where available |
| GLM | region OpenAI Chat-compatible | region Anthropic endpoint |
| MiniMax | Anthropic Messages-compatible | `protocol="openai"` / `minimax.openai` |

Resolution precedence is unchanged:

```text
explicit endpoint > explicit factory protocol > provider/model default
```

Node example:

```ts
const provider = deepseek({ apiKey, protocol: "anthropic" })
```

Python example:

```python
provider = deepseek(api_key=api_key, protocol="anthropic")
```

## Resume old Sessions safely

A Session with Anthropic `native_blocks` cannot be resumed through a newly selected OpenAI-compatible
route. DeepStrike does not translate provider-native reasoning between protocols. For a historical
tool-call turn, recovery fails before network dispatch with code
`provider_replay_protocol_mismatch` and asks the host to pin the previous protocol endpoint.

The diagnostic contains provider/protocol identifiers only. It does not include thinking text, tool
names, tool arguments, or provider response bodies. Pin the endpoint used to create the Session, or
start a new Session on the new default route.

## Cache telemetry changes

Consumers of turn metrics should read `cache_telemetry_status` before interpreting cache token counts:

- `measured`: zero is a confirmed zero and non-zero values came from a recognized provider field.
- `unavailable`: the response did not expose interpretable cache telemetry.

DeepSeek OpenAI responses use `prompt_cache_hit_tokens` and `prompt_cache_miss_tokens`; prompt hits are
part of the full prompt-token count and are not added twice. `cache_read_tokens_by_slot` is absent when
the provider supplies aggregate usage only. Use `stable_prefix_fingerprint` to correlate prefix drift
without persisting prompt content.

Only endpoint capabilities backed by evidence are marked supported. Custom and unverified compatible
endpoints remain unknown. A live cache-hit baseline still requires provider credentials and is not
inferred from unit fixtures.

## Token measurement and SDK requirements

Assistant history `token_count` now records output tokens only (the Python runner previously wrote
provider `total_tokens`, double-counting each turn's input into context pressure). The full prompt is
reported independently as `observed_input_tokens` / `observed_output_tokens` on the provider result.
Applications that asserted `token_count == total_tokens` on assistant messages must switch to the
observed fields.

Provider SDK floors moved: Node `openai ^7.5.0` (from 5.23.2) and Python `openai>=2.6` — the release
that introduced `responses.input_tokens.count`. With an older SDK the Responses capability reads
unavailable and preflight degrades to a heuristic estimate instead of failing the run.

Measurement records carry provenance. `RecordedPromptMeasurement.source` gains the additive
`postflight` variant for observed usage fed back after execution; consumers switching on the source
kind should treat unrecognized kinds as opaque facts rather than errors. Durable measurements are
keyed by the full request fingerprint (endpoint, model, turns + state turn, tools, material options),
so replay reuses a recorded fact only for a byte-identical wire plan.

## Textual tool-call rejection

When tools are exposed, Anthropic-compatible and custom Anthropic endpoints reject the exact DSML
tool-call sentinel instead of returning it as visible assistant text. The error contract is:

```text
kind=protocol
providerCode=textual_tool_call
retryable=true
message="Provider emitted a tool call as text instead of a native tool block"
```

Official Anthropic leaves this policy off by default. Set the per-call host extension
`textualToolCallPolicy` to `"off"` or `"reject"` to override the endpoint default. The extension is not
sent to the provider. Rejection does not parse or execute textual tool calls; recovery remains outside
the 0.2.61 P0-P1 scope.
