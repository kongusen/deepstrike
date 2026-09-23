# Type-Driven Boundary Contracts

## Goal

Replace per-function semantic JSON contracts with boundary protocols whose structural facts are derived from typed adapters. Keep security policy explicit and reviewable.

The work starts from `v0.2.73` and uses `SkillMetadata → KernelSkillMetadata` as the first vertical slice.

## Design rules

- A boundary protocol describes a layer relationship; an adapter is one implementation of that protocol.
- Cross-layer business functions use named source and target types. `Record<string, unknown>` is allowed only at the final serialization boundary.
- Exact same-name, type-compatible fields are candidates for inferred preservation.
- Renames, derived fields, and semantic exceptions are explicit metadata.
- Forbidden fields are explicit security policy and are checked both statically and at runtime.
- Generated manifests and validators are outputs. The protocol definitions, typed adapter signatures, and security policies are the sources of truth.

## First slice: Skill host-to-kernel projection

### Source

`node/src/skills/loader.ts:SkillMetadata`

### Target

Introduce a named host-side kernel projection type, tentatively `KernelSkillMetadata`, beside the adapter boundary. Its fields must match the kernel wire vocabulary, including snake_case names where the wire requires them.

### Adapter

Change `skillMetadataToKernel` to return `KernelSkillMetadata`. Keep serialization separate from the typed projection.

### Policy

Declare the forbidden fields for this protocol in typed policy data. At minimum, verify that storage details, provider credentials, and activation authority cannot appear in the kernel projection.

### Generated outputs

Generate a small manifest containing:

- protocol family and layer direction;
- source and target type names;
- inferred exact preserves;
- explicit renames and derived fields;
- inferred drops;
- forbidden policy;
- adapter symbol.

The first slice does not attempt to migrate all existing crossings or all SDKs.

## Implementation sequence

### Task 1: Define the protocol vocabulary

- Add names for the protocol families: public-to-host lowering, host-to-kernel projection, kernel-to-host event decode, host-provider codec, and runtime materialization.
- Define the minimal policy shape for `preserves`, `renames`, `derived`, `drops`, and `forbidden`.
- Decide whether policy is expressed as TypeScript values or decorators. Prefer plain typed values unless compiler metadata is required.

**Status:** Completed (`9dc09c9b`)
- Defined protocol type system in [types.ts](file:///Users/shan/work/uploads/deepstrike/contracts/protocols/types.ts).
- Declared skill host-to-kernel projection protocol in [skill-host-to-kernel.ts](file:///Users/shan/work/uploads/deepstrike/contracts/protocols/skill-host-to-kernel.ts).

**Checkpoint:** Passed - Protocol model describes Skill projection without JSON duplication.

### Task 2: Type the Skill projection

- Add `KernelSkillMetadata`.
- Make `skillMetadataToKernel` return that type.
- Keep the returned object explicitly constructed so extra source fields cannot leak by spread.
- Add a serialization function only if the existing kernel call requires a record-shaped value.

**Status:** Completed (`810f5e43`)
- Added `KernelSkillMetadata` interface matching kernel wire vocabulary in [kernel-step.ts](file:///Users/shan/work/uploads/deepstrike/node/src/runtime/kernel-step.ts).
- Updated `skillMetadataToKernel` to return `KernelSkillMetadata` using explicit construction.

**Checkpoint:** Passed - Skill loader and kernel projection tests pass; no `Record<string, unknown>` return type.

### Task 3: Build type inspection for one adapter

- Use the TypeScript compiler API, not regular expressions, to resolve the adapter declaration.
- Read its parameter and return types.
- Resolve the property names and optionality of the source and target types.
- Fail when the protocol declaration and adapter signature disagree.

**Status:** Completed (`79905fd0`)
- Built contract checker script [check-boundary-contracts.mjs](file:///Users/shan/work/uploads/deepstrike/scripts/check-boundary-contracts.mjs).
- Added `npm run contracts:check` command to workspace root.

**Checkpoint:** Passed - Checker resolves signatures and properties through the TypeScript compiler API and validates the protocol declaration against the adapter signature.

### Task 4: Generate the first manifest and validator

- Generate the Skill projection manifest from the typed adapter and policy.
- Generate a strict runtime validator that checks allowed keys, required preserved keys, and forbidden keys.
- Add a test for a forbidden field leak and a test for an omitted preserved field.

**Status:** Completed (`e1d26641`)
- Generated crossing manifest [skill-host-to-kernel.json](file:///Users/shan/work/uploads/deepstrike/contracts/manifests/skill-host-to-kernel.json).
- Generates the runtime validator [skill-kernel-projection.ts](file:///Users/shan/work/uploads/deepstrike/node/src/runtime/validators/skill-kernel-projection.ts) from the protocol and target type.
- Generates runtime field-shape checks for primitive and array properties from the resolved target type.
- Integrated validator into `skillMetadataToKernel` adapter function.
- Added 12 validator unit tests in [skill-kernel-projection-validator.test.ts](file:///Users/shan/work/uploads/deepstrike/node/tests/skill-kernel-projection-validator.test.ts).

**Checkpoint:** Passed - Validator enforces forbidden field policy and required preserved keys at runtime.

### Task 5: Evaluate before expanding

- Compare the generated manifest with the old Skill contract only as a migration aid.
- Record which facts were inferred and which required explicit metadata.
- Decide whether the same mechanism is ready for Agent lowering and Usage settlement.

**Status:** Completed

---

## First Slice Execution Results & Evaluation

### Results Summary

1. **Protocol & Adapter Verification**: `skillMetadataToKernel` adapter correctly maps 7 source properties of `SkillMetadata` to 7 target properties of `KernelSkillMetadata`.
2. **Inferred vs Explicit Facts**:
   - Inferred Preserves: `name`, `description`, `effort`
   - Inferred Renames: `whenToUse` → `when_to_use`, `estimatedTokens` → `estimated_tokens`, `allowedTools` → `allowed_tools`, `capabilityGrants` → `capability_grants`
   - Inferred Drops: none (all fields preserved or renamed)
   - Explicit Security Policy: `provider_credentials`, `activation_authority`, `storage_backend`, `user_storage_path`, `source_adapter` explicitly forbidden.
   - No `preserves` list is authored in the protocol; exact compatible fields and required preserved fields come from the types.
3. **Runtime Enforcement**: Validator rejects unsafe projections even if TypeScript compilation succeeds (e.g. object spread leaks or dynamic properties).
4. **Shape Enforcement**: Generated validation rejects incompatible scalar and array element types for kernel fields.

### Migration comparison

The previous `skill-kernel-projection` contract described a broader `SkillDescriptor → SkillMetadata` crossing and listed instructions, resources, and storage as drops. The typed crossing uses the actual runtime adapter boundary, `SkillMetadata → KernelSkillMetadata`. The former drops therefore do not belong to this projection; they belong to the separate materialization boundary. The new manifest also makes the camelCase-to-snake_case wire renames explicit and expands the kernel security policy.

### Expansion decision

The mechanism is validated for one crossing, but it is not ready to copy directly to Agent lowering or Usage settlement. The next increment must make protocol registration and adapter discovery generic before a second crossing is added.

### Task 6: Register protocols before expanding

**Status:** Completed

- Added a single protocol registry containing the Skill crossing.
- Added stable protocol identity and derived the adapter source path and symbol from the registered adapter reference.
- The checker reads the TypeScript registry and protocol declarations directly through the compiler AST; no checked-in JavaScript protocol mirror is required.

**Checkpoint:** `contracts:check` discovers the registered Skill protocol and passes compiler-resolved verification. Adding another crossing now has a registry entry point instead of another checker branch.

### Follow-up Refinements Identified

1. Add a second crossing only after its transformations and security policy are independently reviewed.

## Live Agent path iterations (2026-09-23)

This track follows the convergence decision in the [analysis](type-driven-boundary-contracts-analysis.md): extract the executable facade path and retire the unused formal IR. The completed Skill slice above remains unchanged.

### First checkpoint: runtime options and provider behavior

- [x] Extract `buildAgentRuntimeOptions` as the live configuration adapter consumed by the facade (`a2ef8e39`). Keep provider resolution and execution-plane lifecycle in the facade.
- [x] Reproduce the missing tool surface, then bind the public Agent baseline after MCP discovery (`cfeefd8e`). Verify actual tool execution, custom planes, capability filtering, and MCP reuse.
- [x] Reproduce dropped provider extensions and overwritten guardrails, then fix them in the adapter (`c6b049b6`). Cover run, stream, session, workflow, resume, combined vetoes, and approval denial.
- [x] Replace vacuous negative-only tool assertions with exact visible-set assertions.
- [x] Validate the Node build and offline regression suite: 181 suites / 1139 tests pass. `contracts:check` and `contracts:verify` pass. Six live-provider suites were excluded; no new tests are skipped.

This checkpoint covers the tool/provider/governance part of iterations 0–2. It does not claim complete public Agent semantics: memory, target-agent dispatch, immutable declarations, and concurrent session handling still need their own reproduction tests and fixes.

### Remaining iterations and acceptance gates

| Iteration | Deliverable | Gate before moving on |
|---|---|---|
| 3 | Separate declarative configuration from runtime bindings; take an immutable declaration snapshot without freezing providers/stores | Caller mutation cannot alter later runs; serializable declaration excludes executable bindings; the facade keeps using the live adapter |
| 4 | Bind declarative memory and route public memory operations through validated, governed, audited runtime operations | Model-origin and host-origin provenance remain distinct; denied writes never reach the store; recall and audit behavior have integration coverage |
| 5 | Isolate active execution handles and result evidence by run/session | Interrupt affects only the intended session; concurrent sessions and resumed runs retain correct identity and evidence; define same-session admission semantics |
| 6 | Resolve handoff/workflow target Agents at the host spawn boundary | A two-provider test proves the requested target executes; unknown targets fail explicitly; existing kernel identity and quota authority remain intact |
| 7 | Extend the contract mechanism for multiple adapters, derived fields, and correlated protocol checks | Capability and configure-run families use the generic registry; structural validation is kept distinct from behavioral correlation tests |
| 8 | Register the actual Agent adapter, then kernel projections/decode, provider semantic points, and subsystem crossings in separate slices | Each registered adapter is consumed by the runtime; generated validators cover missing/forbidden/invalid fields; check/verify and corresponding behavior tests pass |
| 9 | Deprecate the formal IR and remove it at the agreed compatibility-window boundary | Node/WASM exports, conformance fixtures, vocabulary, and surface tests move together; no implementation is deleted before its consumers migrate |

Each iteration should be split into small commits with a green checkpoint. Registration must not be used as a substitute for repairing the runtime behavior it is intended to protect. Runtime-internal Eval, provider wire specialization, and a Rust ABI redesign remain outside this track.

Iteration 3 is complete in the current branch. `createAgent` now captures a deeply frozen `agent.declaration` containing JSON data, while provider, execution plane, memory store, vector retriever, and bound tool executors remain in private host bindings. `agent.declaration` is the sole public declarative view; runtime bindings are never re-materialized into a compatibility definition. The regression suite covers caller mutation, serialization, and reassignment of provider/tool inputs.

Iteration 4 is complete in the current branch. Public `remember` and `recall` now use the RuntimeRunner memory gateway, so policy validation, memory lifecycle updates, and session-log audit records apply to facade calls as well as kernel-driven paths. Host writes carry `host/user_asserted` provenance, model writes retain `model/untrusted` provenance, rejected writes stop before `MemoryStore.put`, retrieval breadth honors `memoryPolicy.retrievalTopK`, and memory-only Agents can operate without a provider binding.

Iteration 5 is complete in the current branch. The facade tracks active runners by session id, routes `AgentSession.interrupt` only to that session, and releases the matching runner after stream, resume, or workflow completion. Concurrent different sessions remain independent, while overlapping runs for the same session are rejected explicitly so session history and result evidence cannot interleave.

Iteration 6 has an initial host resolution checkpoint in the current branch. Every delegation now requires an explicit target declared in the source Agent's handoff allowlist and a host `resolveAgent` binding; the resolved target Agent executes with its own runtime provider, and a missing registry entry fails explicitly. `Agent.delegate` no longer falls back to an implicit workflow node; workflow execution remains an explicit `Agent.workflow` operation while workflow node target propagation is migrated to the same host spawn boundary.

Iteration 7 has its first structural checkpoint in the current branch. The boundary checker now processes every registered protocol, resolves each adapter independently through the TypeScript compiler, and takes manifest and validator destinations from protocol metadata instead of hardcoded Skill paths. The existing Skill projection keeps its generated validator and regression coverage; configure-run and correlated behavioral checks remain the next slice.

Iteration 7 now also registers the five capability host-to-kernel adapters: tool, skill, marker, mount, and unmount. Each adapter has a compiler-resolved source and target type, an independent generated manifest, and direct projection tests; field policies distinguish preserved, derived, and intentionally dropped values. Configure-run remains the next multi-adapter family.

The configure-run family is now registered as four typed child adapters for governance, context policy, reliability, and signal policy. Their existing runtime conversion functions are the checked crossings, while `buildConfigureRunPolicyConfig` is the named composite consumed by the runner; direct tests cover snake-case projection, policy defaults, and correlation into one `configure_run` config.

Iteration 8 has its first live-path checkpoint. `buildAgentRuntimeOptions` now accepts one typed `AgentRuntimeOptionsRequest`, and the actual facade call is registered as `agent.public-to-host` with a compiler-resolved `AgentRuntimeOptionsRequest → RuntimeOptions` signature. Existing facade behavior tests remain the behavioral gate for this adapter.

The same iteration now registers four live kernel projections: message, tool schema, tool result, and task update. Their return types are named kernel structures, the runner continues to consume the same functions, and direct tests cover parsed arguments, snake-case fields, optional error data, and task progress projection.

The next event-decode checkpoint registers the live `KernelObservation → SessionEvent | null` crossing. `kernelObservationToSessionEventAtBoundary` makes the turn and archive context explicit in a one-argument request, and `runner.appendObservations` consumes that adapter directly. The first behavior slice covers entropy samples, compressed archive context, and the intentionally non-persisted page-in request; the remaining observation branches stay behind the same adapter until their individual semantic tests are split out.

The first provider semantic checkpoint registers the four live wire-usage decoders as one `host-provider` decode family. OpenAI, Anthropic, Gemini, and Ollama each resolve through the same compiler-checked `unknown → ProviderUsage | undefined` target; their existing provider adapter call sites and usage-normalizer tests remain the behavioral gate. Request-body and stream-event codecs stay separate because they carry protocol-specific state and error semantics.

The request encoding checkpoint now supports class method adapters in the checker and registers the live Anthropic, Gemini, and Ollama `buildRequest` methods. Each crosses `CanonicalAdapterInput` into its protocol-specific request plan on the existing adapter instance; OpenAI builders remain separate because their dialect or continuation state adds a second method parameter.

The OpenAI request checkpoint now wraps those extra inputs in named boundary requests. Chat carries its resolved wire dialect, while Responses carries its optional continuation state; complete, stream, and native token-count paths use the new methods directly. Both OpenAI request plans are now registered without flattening their stateful semantics.

The stream decode checkpoint adds one typed `{ chunk, state }` request for each provider adapter. Anthropic, Gemini, Ollama, OpenAI Chat, and OpenAI Responses streaming paths now call their boundary methods directly, so state updates and emitted `StreamEvent` values are covered by the same registered provider decode family.

The stream finish checkpoint adds the corresponding finalization requests. Providers with a terminal response block keep that block in the request, while providers that only need accumulated state expose a state-only request; every live stream path now uses the typed finalizer before returning its terminal events.

### Review checkpoint after provider stream registration

The runtime migration is ahead of the contract quality gate. The registry now contains 35 compiler-checked adapters, and the Agent facade, kernel projections, kernel event persistence, provider usage decoding, request building, stream chunk decoding, and stream finalization all run through registered boundary methods. The full offline suite remains green at 189 suites / 1158 tests.

Three gaps remain before Iteration 8 can be called complete:

1. Generated manifests currently describe the outer request envelopes. For wrapper requests such as `{ chunk, state }` and `{ input, dialect }`, inferred drops describe envelope fields rather than the nested semantic mapping. The checker needs either nested path inspection or an explicit distinction between transport envelope fields and semantic projection fields.
2. Only the Skill projection has a generated runtime validator. The newly registered kernel, event, and provider adapters have structural manifests but no missing/forbidden/invalid field validators. Each protocol must either gain an appropriate validator or declare a reviewed reason why compiler checking plus behavioral tests are sufficient.
3. Union and open input boundaries remain weakly described. `SessionEvent | null` has no useful common field inference, and provider usage decoders intentionally accept `unknown`. These boundaries need explicit behavioral correlation tests and stronger named input types where the wire shape is known.

The next sequence is therefore contract quality hardening, one subsystem crossing family at a time, followed by the formal IR removal work. The no-compatibility constraint allows Iteration 9 to move forward as soon as remaining internal `agent-ir` consumers and public runtime exports are migrated; it does not require preserving the current IR surface.

The envelope distinction checkpoint now records transport wrapper fields separately from semantic drops. Kernel event, OpenAI request, stream chunk, and stream finish protocols validate their declared envelope fields and exclude them from inferred semantic drops. The first nested semantic path mappings now describe canonical input to provider params and stream input/state to emitted `AdapterOutput` events; deeper target shape checks remain open for provider-specific params and event unions.

The first subsystem crossing checkpoint registers `memoryPolicyToKernel` as `MemoryPolicy → KernelMemoryPolicy`. The live runner uses the named target type, the boundary declares all camelCase-to-snake_case renames, and direct tests cover omission and unknown-field rejection.

The signal subsystem checkpoint registers `signalToKernelEvent` as a data-only `KernelSignalDeliveryRequest → KernelSignalDeliveryEvent` projection. Lease acknowledgement callbacks remain host-owned, while delivery identity, signal payload, deadline, and coalescing metadata cross into the kernel event shape. Both live signal consumption paths use the projection and direct tests cover its mapping.

The first workflow subsystem checkpoint registers `workflowNodeSpecToKernel` and `workflowSpecToKernel` with named `KernelWorkflowNode` and `KernelWorkflowSpec` targets. Host-only node identity and agent bindings are dropped explicitly, control-flow kinds are derived, and the same node projection feeds both workflow start and dynamic submission paths.

---

## Explicit non-goals for the first slice

- Do not migrate all 15 existing contract files.
- Do not redesign the Rust kernel ABI.
- Do not infer semantic equivalence from matching property names alone.
- Do not add cross-SDK generation until the TypeScript boundary model is proven.
- Do not make activation a host-side Skill crossing.

## Acceptance criteria

- The new branch is based on `v0.2.73`.
- One real crossing has a named typed target and typed adapter.
- The checker uses compiler-resolved signatures for that crossing.
- Forbidden policy remains explicit and is enforced at runtime.
- Generated output is reproducible and is not hand-edited.
- Existing tests pass before expanding the scope.
