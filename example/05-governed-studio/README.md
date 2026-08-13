# L5 · Governed Assistant

The Agent now works inside an explicit application policy. A prompt may request an action, but policy decides whether the action is visible, allowed, or needs approval.

## What you learn

| Capability | What to observe |
| --- | --- |
| Deny | `publish_public` is removed from the available tools, so the Agent cannot call it. |
| Approval | `email_editor` pauses until the application approves or rejects it. |
| Limits | Quotas bound how much delegation and memory writing an Agent may request. |
| Evidence | The session summary shows which decisions happened during the run. |

## Run

```bash
npx tsx 05-governed-studio/main.ts
npx tsx 05-governed-studio/main.ts --dry-run
python 05-governed-studio/main.py
```

Try changing the host approval result and observe that the same prompt produces a different permitted outcome.

## Next

[L6 turns the bounded run into a long-running Agent](../06-daily-digest/), with self-pacing, sleep, wake, and completion checks.
