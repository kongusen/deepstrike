# L1 · Sourced Q&A assistant

The smallest real agent — and the base every later level builds on.

```
question ──▶ [ RuntimeRunner ] ──search──▶ [ Execution Plane ] ──▶ studio tools
                   │  ▲                          (kernel approves each call)
                   │  └── tool results
                   ▼
              answer with citations          every turn appended to the Session Log
```

## What you learn here

| Mechanism | Where it shows up |
|---|---|
| **Tools + Execution Plane** | `search` / `read_source` registered on a `LocalExecutionPlane`; the kernel approves each call before the plane runs it — the agent's only way to touch the world. |
| **Provider** | a real LLM drives the loop (`resolveProvider()` from env). |
| **Session log / replay & recovery** | a `FileSessionLog` persists every turn; re-running the same `--session` id **resumes** the transcript instead of starting over. |

## Run

```sh
# once: build the SDK the examples import, then link it + install tsx
npm run build --prefix ../node
npm install

# validate wiring without a key or a call
npx tsx 01-sourced-qa/main.ts --dry-run

# live (needs a provider key)
ANTHROPIC_API_KEY=sk-ant-... npx tsx 01-sourced-qa/main.ts "How does prompt caching work? Cite sources."
```

Python mirror:

```sh
cd ../python && pip install -e .
ANTHROPIC_API_KEY=sk-ant-... python ../example/01-sourced-qa/main.py "How does prompt caching work?"
python ../example/01-sourced-qa/main.py --dry-run
```

## Try the recovery mechanism

```sh
# start a named session, Ctrl-C partway through the answer
npx tsx 01-sourced-qa/main.ts --session demo "Explain agent memory with sources."
# re-run the SAME command — it prints "↻ resuming …" and continues from the transcript
npx tsx 01-sourced-qa/main.ts --session demo "Explain agent memory with sources."
```

The kernel detects a mid-run transcript and replays it — no special resume API, the session log **is** the recovery mechanism. (Sessions are written under `.sessions/`, git-ignored.)

## What's next

**L2 · Memory** gives this same assistant a `DreamStore`: it remembers sources and user
preferences across sessions, deduped through the one governed write gate, and recalls them at
run-start so the second question on a topic starts from what it already knew.
