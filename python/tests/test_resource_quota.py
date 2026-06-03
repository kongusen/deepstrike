import json

import pytest

import deepstrike.runtime.runner as runner_mod
from deepstrike._kernel import KernelRuntime, LoopPolicy
from deepstrike.providers.stream import TextDelta
from deepstrike.runtime import (
  InMemorySessionLog,
  LocalExecutionPlane,
  MemoryWriteRateLimit,
  ResourceQuota,
  RuntimeOptions,
  RuntimeRunner,
  SchedulerBudget,
  collect_text,
)


class Provider:
  async def complete(self, context, tools, extensions=None):
    raise NotImplementedError

  async def stream(self, context, tools, extensions=None, state=None):
    yield TextDelta(delta="ok")


class CapturingKernelRuntime:
  events: list[dict] = []

  def __init__(self, policy):
    self._terminal = False
    self._turn = 0

  def step(self, input_json: str) -> str:
    event = json.loads(input_json)["event"]
    self.events.append(event)
    if event["kind"] == "start_run":
      self._turn = 1
      return json.dumps({
        "version": 1,
        "actions": [{
          "kind": "call_provider",
          "context": {"system_text": "", "turns": []},
          "tools": [],
        }],
        "observations": [],
      })
    if event["kind"] == "provider_result":
      self._terminal = True
      return json.dumps({
        "version": 1,
        "actions": [{
          "kind": "done",
          "result": {"termination": "completed", "turns_used": 1, "total_tokens_used": 0},
        }],
        "observations": [],
      })
    return json.dumps({"version": 1, "actions": [], "observations": []})

  def is_terminal(self) -> bool:
    return self._terminal

  def turn(self) -> int:
    return self._turn

  def recovery_content_bytes(self) -> int:
    return 4096

  def drain_new_messages(self):
    return []

  def preserved_refs(self):
    return []


@pytest.mark.asyncio
async def test_runtime_options_resource_quota_emits_set_resource_quota(monkeypatch):
  CapturingKernelRuntime.events = []
  monkeypatch.setattr(runner_mod, "KernelRuntime", CapturingKernelRuntime)

  runner = RuntimeRunner(RuntimeOptions(
    provider=Provider(),
    session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane(),
    max_tokens=1024,
    scheduler_budget=SchedulerBudget(max_wall_ms=1234),
    resource_quota=ResourceQuota(
      max_concurrent_subagents=2,
      max_spawn_depth=1,
      memory_writes_per_window=MemoryWriteRateLimit(max_writes=3, window_ms=1000),
    ),
  ))

  assert await collect_text(runner.run(session_id="quota-py", goal="go")) == "ok"

  quota_event = next(e for e in CapturingKernelRuntime.events if e["kind"] == "set_resource_quota")
  assert quota_event["quota"] == {
    "max_concurrent_subagents": 2,
    "max_spawn_depth": 1,
    "memory_writes_per_window": [3, 1000],
  }
  budget_event = next(e for e in CapturingKernelRuntime.events if e["kind"] == "set_scheduler_budget")
  assert budget_event["max_wall_ms"] == 1234


def test_native_kernel_accepts_set_resource_quota_event():
  runtime = KernelRuntime(LoopPolicy(max_tokens=1024, max_turns=4))

  output = runtime.step(json.dumps({
    "version": 1,
    "event": {
      "kind": "set_resource_quota",
      "quota": {
        "max_concurrent_subagents": 2,
        "max_spawn_depth": 1,
        "memory_writes_per_window": [3, 1000],
      },
    },
  }))

  decoded = json.loads(output)
  assert decoded["version"] == 1
  assert decoded["actions"] == []
  assert decoded["observations"] == []
