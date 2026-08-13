# L8 · Editorial Agent Team

The capstone is a small team of peer Agents. A writer, editor, and fact-checker share a blackboard, react to relevant updates, and work under one cumulative application budget. The writer's reaction itself runs the L7 specialist pipeline.

## What you learn

| Capability | What to observe |
| --- | --- |
| Reactive peers | A new event selects which persona should react. |
| Shared blackboard | Reviewers read the writer's draft through an explicit `read_recent` tool. |
| Shared budget | The application can inspect cumulative usage and team membership. |
| Nested composition | One peer can run a complete workflow as its response to a board event. |

## Run

```bash
npx tsx 08-editorial-room/main.ts
npx tsx 08-editorial-room/main.ts --dry-run
python 08-editorial-room/main.py
```

Round 1 creates a draft. Round 2 lets the editor and fact-checker respond to it. The final output includes the shared usage summary for the whole team.

## The full path

L1 → L2 → L3 → L4 → L5 → L6 builds one Agent from tools to durable, reactive, governed work. L7 and L8 extend that Agent into specialist workflows and peer teams.
