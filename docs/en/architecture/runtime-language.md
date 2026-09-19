# DeepStrike Runtime Language

This glossary is normative for 0.2.68. It names what data means, who owns it, how it crosses a
boundary, and how it becomes durable history. The paired Chinese page is
[运行时语言](../../architecture/runtime-language.md).

## Kernel boundary

| Term | Meaning |
| --- | --- |
| **Intent** | A host-to-kernel request expressing what should happen, such as `ConfigureOperation`, `StartOperation`, or `HostControl`. |
| **Fact** | A report of what happened outside the kernel, such as a `ResolveEffect` outcome or `DeliverExternalEvent`. |
| **Decision** | A kernel-to-host adjudication of what may or must happen, such as `KernelEffect` or `Terminal`. |
| **derive** | Derive a legal semantic intent from an admitted Fact. A provider `ToolCall` is Fact payload, never authority. |
| **resolve** | Answer one previously published effect with its outcome. It does not directly mutate kernel state. |

The complete boundary sequence is `Intent → Decision → encode → Execution → decode → Fact → normalize → Settlement → State Transition`.

## Representations and authority

| Term | Meaning |
| --- | --- |
| **Canonical** | Provider-neutral semantic meaning. |
| **Wire** | A cross-boundary ABI or provider protocol representation. |
| **Internal** | A representation used by an algorithm or projection inside a layer. |
| **Durable** | A crash- and replay-stable representation. |
| **Authority** | The one owner of a fact. Every other representation must be derivable, reference it, or record evidence about it. |
| **Reference** | An ID, digest, handle, or locator that points to authority. |
| **Projection** | A read-only derivation that can be rebuilt from authority. |
| **Cache** | A temporary copy with a verifiable key, fingerprint, or version. |
| **Measurement** | A provenance-bearing observation of an authority object. |
| **Evidence** | An immutable record that an external event occurred. |
| **Mirror** | An ABI or SDK serialization mapping. A mirror cannot add semantic authority. |

`CoreMessage` is an Internal runtime representation. `ProviderMessage` is Kernel Wire. Provider
JSON is Provider Wire. `StoredMessageState` and checkpoint DTOs are Durable representations.

## Provider boundary

| Term | Meaning |
| --- | --- |
| **encode** | Map Canonical semantics to a provider protocol request. It may translate roles, schemas, and materialize references at the adapter boundary. |
| **decode** | Extract Canonical output, Measurement, and Wire Evidence from a provider response without discarding required raw evidence. |
| **normalize** | Map vendor semantics to a controlled vocabulary without turning unavailable information into zero. |
| **render** | Produce provider-facing context from semantic state. Rendering is a Projection. |
| **materialize** | Resolve a durable reference into bytes required by an external adapter. Materialization belongs outside the kernel. |

## Accounting and execution

| Term | Meaning |
| --- | --- |
| **Settlement** | The policy result that converts a provider Measurement into the accounting fact accepted by the kernel budget ledger. |
| **Route** | The resolved physical execution target: provider, protocol, model, endpoint, adapter version, and capabilities. |
| **Attempt** | One physical execution of a kernel effect, keyed by `effect_id + attempt_seq`. |
| **Invocation** | One logical model call and its effect/attempt chain. One Invocation may contain retries. |
| **bind** | Establish and freeze an identity relationship. |
| **replay** | Deterministically rebuild decisions from durable inputs and verify their digests. |

Provider usage is Measurement. It becomes kernel accounting only after `normalize → policy →
Settlement`. SessionLog records the evidence; it is not recovery authority.

## Persistence truth

| Store | Truth role | Question answered |
| --- | --- | --- |
| **Journal** | State Truth | Which durable input sequence produced this state? |
| **Checkpoint** | State Snapshot | What restore state was captured at a step boundary? |
| **SessionLog** | Evidence Truth | What did the outside world observe? |

Every new runtime object should document its Domain, Authority, Durability, Identity, Causation,
and Replay behavior before it crosses a layer boundary.

## 0.2.69 Verifiable Runtime

0.2.69 adds the Framework Verifiable Runtime Foundation. The host-side `deepstrike` command surface
is only one adapter:

```text
deepstrike inspect <operation>
deepstrike verify <operation>
deepstrike replay <operation>
deepstrike fork <operation> --at <step>
```

The framework accepts an adapter-owned evidence bundle, delegates C1–C8 to the chain validator, and
never creates a second semantic authority or invokes a live provider. See
[ADR-009](../../decisions/009-framework-verifiable-runtime).
