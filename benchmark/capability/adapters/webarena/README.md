# WebArena adapter (stub)

WebArena measures agents on realistic multi-site web tasks (shopping, CMS, Reddit, GitLab, maps).  
This folder currently ships only an **interface stub** so the capability CLI can list `webarena` and document the integration path. It does **not** run live evals yet.

## Why stubbed

- Needs a full Docker stack of the WebArena websites + evaluation harness
- Browser tools (Playwright / CDP) are not built into DeepStrike; they must be wrapped as `tool()` registrations or an MCP plane
- Runs are long and environment-heavy compared to BFCL/GAIA smoke

## Planned adapter surface

Align with BFCL/GAIA:

| hook | role |
|------|------|
| `loadTasks({ limit, dataset })` | Load WebArena task configs (intent, start URL, eval criteria) |
| `mkTools(task, sdk)` | Browser actions: `goto`, `click`, `type`, `scroll`, `get_axtree` / accessibility snapshot |
| `grade({ task, finalText, toolCalls, status })` | Prefer official WebArena reward / string-match evaluators |

## Suggested next steps

1. Clone and start [WebArena](https://github.com/web-arena-x/webarena) Docker environments.
2. Implement Playwright-backed tools via `tool()` (or `McpProxyPlane` if tools live in another process).
3. Drive tasks with `RuntimeRunner` the same way [`../bfcl`](../bfcl) and [`../gaia`](../gaia) do.
4. Call the official evaluator for scoring; map into `CapGrade`.

Until then:

```bash
node capability/cli/capability.mjs webarena
# → exits with stub error / usage pointing here
```
