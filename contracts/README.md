# Semantic Contract Registry

This directory is the machine-readable authority for semantic crossings between the public Agent language, host runtime language, kernel language, and provider membrane.

Every crossing records its source and target authority, named crossing function, preserved semantics, intentional lossiness, forbidden semantics, and kernel projection. A crossing may not mint an identity already present in its input.

The registry currently covers Agent, Model, Tool, Skill, Memory, Workflow, Context, Provider Call, Usage, and Eval. Run `npm run contracts:check` from the repository root. The checker validates the registry shape, approved crossing verbs, named implementation functions, forbidden policies, and identity remint rules. Documentation and SDK mirrors may describe these contracts, but they do not replace this registry.
