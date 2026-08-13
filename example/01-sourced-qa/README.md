# L1 · Sourced Q&A Agent

The smallest useful Agent: it searches a local source index, reads relevant material, and answers with citations.

## What you learn

| Capability | What to observe |
| --- | --- |
| Tools | The Agent chooses `search` and `read_source` instead of inventing facts. |
| Provider | A real model drives the conversation and tool calls. |
| Session | A named session keeps evidence so an interrupted answer can continue. |

## Run

```bash
npm run build --prefix ../node
npm install
npx tsx 01-sourced-qa/main.ts --dry-run
ANTHROPIC_API_KEY=sk-ant-... npx tsx 01-sourced-qa/main.ts "How does prompt caching work? Cite sources."
```

Python mirror:

```bash
pip install -e ../python
python 01-sourced-qa/main.py --dry-run
```

## Try resume

```bash
npx tsx 01-sourced-qa/main.ts --session demo "Explain agent memory with sources."
# interrupt it, then run the same command again
npx tsx 01-sourced-qa/main.ts --session demo "Explain agent memory with sources."
```

## Next

[L2 adds durable memory](../02-memory-assistant/), so the Agent can recall useful facts in a later session.
