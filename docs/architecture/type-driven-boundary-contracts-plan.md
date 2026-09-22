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

**Checkpoint:** the protocol model can describe the Skill projection without JSON duplication.

### Task 2: Type the Skill projection

- Add `KernelSkillMetadata`.
- Make `skillMetadataToKernel` return that type.
- Keep the returned object explicitly constructed so extra source fields cannot leak by spread.
- Add a serialization function only if the existing kernel call requires a record-shaped value.

**Checkpoint:** existing skill loader and kernel projection tests still pass, and the adapter has no `Record<string, unknown>` return type.

### Task 3: Build type inspection for one adapter

- Use the TypeScript compiler API, not regular expressions, to resolve the adapter declaration.
- Read its parameter and return types.
- Resolve the property names and optionality of the source and target types.
- Fail when the protocol declaration and adapter signature disagree.

**Checkpoint:** deliberately changing the adapter return type to an unrelated type causes the checker to fail.

### Task 4: Generate the first manifest and validator

- Generate the Skill projection manifest from the typed adapter and policy.
- Generate a strict runtime validator that checks allowed keys, required preserved keys, and forbidden keys.
- Add a test for a forbidden field leak and a test for an omitted preserved field.

**Checkpoint:** the validator rejects an unsafe projection even when TypeScript compilation succeeds.

### Task 5: Evaluate before expanding

- Compare the generated manifest with the old Skill contract only as a migration aid.
- Record which facts were inferred and which required explicit metadata.
- Decide whether the same mechanism is ready for Agent lowering and Usage settlement.

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
