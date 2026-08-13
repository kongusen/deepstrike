# Research Brief Studio

Research Brief Studio is a hands-on Agent curriculum. It grows one small research assistant into a coordinated editorial team, adding one practical capability at a time.

The domain stays the same across all levels, so the learning progression is easy to see. The tools are local mocks over a small source corpus; the Agent loop can use a real provider. Every level has TypeScript and Python code and supports `--dry-run`.

## The learning path

| Level | Agent you build | New capability |
| --- | --- | --- |
| [L1](./01-sourced-qa/) | Sourced Q&A Agent | Tools, citations, provider calls, and resumable sessions |
| [L2](./02-memory-assistant/) | Memory Assistant | Durable memory, recall, deduplication, and session learning |
| [L3](./03-skills-handbook/) | Skill-based Assistant | On-demand skills, knowledge, and task-specific tool access |
| [L4](./04-reactive-desk/) | Reactive Assistant | Webhooks, scheduled signals, host notes, and changing input |
| [L5](./05-governed-studio/) | Governed Assistant | Tool policies, approvals, quotas, and observable decisions |
| [L6](./06-daily-digest/) | Long-running Digest Agent | Bounded rounds, self-pacing, sleep, wake, and completion checks |
| [L7](./07-brief-pipeline/) | Specialist Pipeline | Parallel specialists, structured output, reducers, and verification |
| [L8](./08-editorial-room/) | Editorial Agent Team | Reactive peers, shared blackboard, shared budget, and nested workflows |

## What you will learn

By the end of the curriculum you will know how to give an Agent:

- tools and external integrations;
- durable memory and reusable knowledge;
- skills that load only when a task needs them;
- permissions, approvals, quotas, and cancellation rules;
- child Agents with focused roles and handoff artifacts;
- workflows with parallel work, dependencies, loops, and verification;
- signals, shared state, and peer-to-peer reactions;
- durable sessions that can pause, resume, and be replayed in tests.

## Run the examples

From this directory:

```bash
npm install
npm run build --prefix ../node
npx tsx 01-sourced-qa/main.ts --dry-run
```

For a live run, configure one supported provider in the environment or in `example/.env`:

```bash
ANTHROPIC_API_KEY=sk-ant-...
# or
OPENAI_API_KEY=sk-...
OPENAI_BASE_URL=https://your-endpoint/v1
OPENAI_MODEL=gpt-5-mini
```

Run any level with `npx tsx <level>/main.ts`. Python mirrors use the same level directory and can be run with `python <level>/main.py` after installing the local Python SDK with `pip install -e ../python`.

## Suggested checkpoints

1. Finish L1 before adding memory. Make sure you can interrupt and resume a named session.
2. Finish L3 before L4. Skills and knowledge explain how an Agent can change its working context without changing its identity.
3. Finish L5 before L7. Policies and quotas should be in place before you fan out work.
4. Treat L8 as a composition exercise. Its peers combine the workflow and long-running patterns from earlier levels.

Each level's README contains the exact behavior to observe, the commands to run, and the next step in the path.
