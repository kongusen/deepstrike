# Node Examples

## Long-running stability demo

This demo validates Node SDK stability under many LLM/tool turns. It exercises:

- provider calls through the normal `RuntimeRunner`
- ordered tool execution and checkpoint verification
- skill loading through the `skill` meta-tool
- memory and knowledge retrieval
- large-result spooling
- session log replay / wake
- M2 resource quota setup

Set up:

```sh
cp node/examples/.env.example node/examples/.env
$EDITOR node/examples/.env
npm run build --prefix node
```

Check local wiring without an API call:

```sh
node node/examples/long-running-stability.mjs --dry-run
```

Run the validation with the configured LLM API:

```sh
node node/examples/long-running-stability.mjs
```

Resume a non-terminal session:

```sh
DEEPSTRIKE_SESSION_ID=node-stability-manual DEEPSTRIKE_WAKE=1 node node/examples/long-running-stability.mjs
```

Artifacts are written under `node/examples/.stability-runs/<session-id>/`.
