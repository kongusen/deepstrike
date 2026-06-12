"""G2 deterministic compute: the host-side reducer registry.

A ``NodeKind::Reduce`` workflow node runs no LLM agent — the kernel hands the SDK a reducer name +
its dependency outputs, and the SDK runs the named pure function here. This is the "ordinary code
between stages" (dedupe / filter / merge / early-exit) of the code-orchestration model, expressed
deterministically as a DAG node.
"""

from __future__ import annotations

import json
from typing import Callable

from .output_schema import extract_json_value

# A reducer input is (agent_id, output); a reducer maps a list of them → the node's output string.
ReducerInput = dict  # {"agent_id": str, "output": str}
Reducer = Callable[[list[dict]], str]
ReducerRegistry = dict[str, Reducer]


def _lines(s: str) -> list[str]:
  return [ln.strip() for ln in (s or "").split("\n") if ln.strip()]


def _concat(inputs: list[dict]) -> str:
  return "\n\n".join(i.get("output", "") for i in inputs)


def _dedupe_lines(inputs: list[dict]) -> str:
  seen: set[str] = set()
  out: list[str] = []
  for i in inputs:
    for line in _lines(i.get("output", "")):
      if line not in seen:
        seen.add(line)
        out.append(line)
  return "\n".join(out)


def _merge_json_arrays(inputs: list[dict]) -> str:
  seen: set[str] = set()
  merged: list = []
  for i in inputs:
    v = extract_json_value(i.get("output", ""))
    arr = v if isinstance(v, list) else ([v] if v is not None else [])
    for el in arr:
      key = json.dumps(el, sort_keys=True)
      if key not in seen:
        seen.add(key)
        merged.append(el)
  return json.dumps(merged)


def _count(inputs: list[dict]) -> str:
  return str(sum(1 for i in inputs if (i.get("output", "") or "").strip()))


#: Built-in reducers available to every workflow; a user registry is merged over these.
builtin_reducers: ReducerRegistry = {
  "concat": _concat,
  "dedupe_lines": _dedupe_lines,
  "merge_json_arrays": _merge_json_arrays,
  "count": _count,
}


def resolve_reducer(name: str, user: ReducerRegistry | None = None) -> Reducer | None:
  """Resolve a reducer by name from the built-ins overlaid with a user registry."""
  if user and name in user:
    return user[name]
  return builtin_reducers.get(name)
