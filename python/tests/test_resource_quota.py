import json

import pytest

import deepstrike.runtime.runner as runner_mod
from deepstrike._kernel import KernelRuntime, LoopPolicy
from deepstrike.providers.stream import TextDelta
from deepstrike.runtime import (
  InMemorySessionLog,
  KernelReliability,
  LocalExecutionPlane,
  MemoryWriteRateLimit,
  PromptBudget,
  ResourceQuota,
  RuntimeOptions,
  RuntimeRunner,
  SchedulerPolicy,
  SignalPolicy,
  collect_text,
)


class Provider:
  async def complete(self, context, tools, extensions=None):
    raise NotImplementedError

  async def stream(self, context, tools, extensions=None, state=None):
    yield TextDelta(delta="ok")


@pytest.mark.asyncio
async def test_runtime_options_resource_quota_emits_set_resource_quota(monkeypatch):
  captured: list[dict] = []
  real_apply_host = runner_mod.apply_host

  async def capture_apply(runtime, observations, event):
    captured.append(event)
    return await real_apply_host(runtime, observations, event)

  monkeypatch.setattr(runner_mod, "apply_host", capture_apply)

  runner = RuntimeRunner(RuntimeOptions(
    provider=Provider(),
    session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane(),
    max_tokens=1024,
    scheduler_policy=SchedulerPolicy(
      version=1,
      critical_path_weight=1_000_000,
      fanout_weight=10_000,
      age_weight=1_000,
      token_cost_weight=1,
    ),
    signal_policy=SignalPolicy(queue_max=8, ttl_ms=500, deadline_escalation=False),
    prompt_budget=PromptBudget(
      prompt_overhead_tokens=20,
      output_reserve_tokens=100,
      safety_margin_tokens=10,
    ),
    kernel_reliability=KernelReliability(
      event_replay_capacity=512,
      host_effect_retry_attempts=4,
      spool_threshold_bytes=2048,
      spool_preview_bytes=256,
      max_input_bytes=1024 * 1024,
      snapshot_journal_bytes_limit=16 * 1024 * 1024,
    ),
    resource_quota=ResourceQuota(
      max_concurrent_subagents=2,
      max_spawn_depth=1,
      max_workflow_nodes=7,
      memory_writes_per_window=MemoryWriteRateLimit(max_writes=3, window_ms=1000),
    ),
  ))

  assert await collect_text(runner.run(session_id="quota-py", goal="go")) == "ok"

  quota_event = next(e for e in captured if e["kind"] == "set_resource_quota")
  assert quota_event["quota"] == {
    "max_concurrent_subagents": 2,
    "max_spawn_depth": 1,
    "max_workflow_nodes": 7,
    "memory_writes_per_window": [3, 1000],
  }
  signal_event = next(
    e for e in captured
    if e["kind"] == "configure_run" and "signal_policy" in e["config"]
  )
  assert signal_event["config"]["scheduler_policy"] == {
    "version": 1,
    "critical_path_weight": 1_000_000,
    "fanout_weight": 10_000,
    "age_weight": 1_000,
    "token_cost_weight": 1,
  }
  assert "scheduler_max_wall_ms" not in signal_event["config"]
  assert not any(e["kind"] == "set_scheduler_budget" for e in captured)
  assert signal_event["config"]["signal_policy"] == {
    "version": 1,
    "queue_max": 8,
    "ttl_ms": 500,
    "deadline_escalation": False,
  }
  assert signal_event["config"]["prompt_budget"] == {
    "prompt_overhead_tokens": 20,
    "output_reserve_tokens": 100,
    "safety_margin_tokens": 10,
  }
  reliability_event = next(
    e for e in captured
    if e["kind"] == "configure_run" and "reliability" in e["config"]
  )
  assert reliability_event["config"]["reliability"] == {
    "event_replay_capacity": 512,
    "host_effect_retry_attempts": 4,
    "spool_threshold_bytes": 2048,
    "spool_preview_bytes": 256,
    "max_input_bytes": 1024 * 1024,
    "snapshot_journal_bytes_limit": 16 * 1024 * 1024,
  }


def test_scheduler_policy_dict_rejects_camel_case_aliases():
  with pytest.raises(ValueError, match="unknown scheduler policy field"):
    runner_mod._scheduler_policy_to_kernel({
      "version": 1,
      "criticalPathWeight": 1_000_000,
      "fanoutWeight": 10_000,
      "ageWeight": 1_000,
      "tokenCostWeight": 1,
    })


def test_scheduler_policy_dict_rejects_retired_wall_budget():
  with pytest.raises(ValueError, match="max_wall_ms"):
    runner_mod._scheduler_policy_to_kernel({
      "version": 1,
      "critical_path_weight": 1,
      "fanout_weight": 1,
      "age_weight": 1,
      "token_cost_weight": 1,
      "max_wall_ms": 1234,
    })


def test_native_kernel_accepts_set_resource_quota_event():
  runtime = KernelRuntime(LoopPolicy(max_tokens=1024, max_turns=4))

  from deepstrike.runtime.kernel_step import _kernel_step
  decoded = _kernel_step(runtime, {
    "kind": "set_resource_quota",
    "quota": {
      "max_concurrent_subagents": 2,
      "max_spawn_depth": 1,
      "memory_writes_per_window": [3, 1000],
    },
  })
  assert decoded["version"] == 2
  assert decoded["actions"] == []
  assert decoded["observations"] == []


def test_native_kernel_rejects_out_of_bounds_sdk_reliability_config():
  from deepstrike.runtime.kernel_step import _kernel_step

  runtime = KernelRuntime(LoopPolicy(max_tokens=1024, max_turns=4))
  with pytest.raises(RuntimeError, match="invalid_config"):
    _kernel_step(runtime, {
      "kind": "configure_run",
      "config": {"reliability": {"event_replay_capacity": 0}},
    })
