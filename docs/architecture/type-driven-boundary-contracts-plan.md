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

- Add names for the four protocol families: public-to-host lowering, host-to-kernel projection, host-provider codec, and runtime materialization.
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
