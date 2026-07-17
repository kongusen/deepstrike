"""Wire-state identity is lifetime-bound to the runtime object (mirrors the Node/WASM WeakMap).

The old module dict keyed by ``id(runtime)`` aliased recycled addresses — a new runtime could
inherit a dead one's ``(operation_id, sequence)``, which is fatal once a durable session log
keys kernel genesis/transaction chains by ``(session_id, operation_id)`` — and it leaked one
entry per runtime for the life of the process.
"""
import gc
import json
import re
import weakref

from deepstrike._kernel import KernelRuntime, LoopPolicy
from deepstrike.runtime.kernel_step import _step_input, _wire_states


class _FakeRuntime:
  pass


def test_each_runtime_mints_a_unique_uuid_operation_identity():
  operation_ids = []
  for _ in range(2):
    runtime = _FakeRuntime()
    envelope = json.loads(_step_input(runtime, {"kind": "noop"}))
    operation_ids.append(envelope["operation_id"])

  assert operation_ids[0] != operation_ids[1]
  for operation_id in operation_ids:
    # Random per runtime, never a resettable ordinal — restarted or replicated hosts must not
    # re-enter a persisted chain on the same session.
    assert re.fullmatch(
      r"python-operation-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
      operation_id,
    )


def test_wire_state_dies_with_its_runtime():
  baseline = len(_wire_states)
  runtime = _FakeRuntime()
  _step_input(runtime, {"kind": "noop"})
  assert len(_wire_states) == baseline + 1

  del runtime
  gc.collect()
  assert len(_wire_states) == baseline


def test_pyo3_kernel_runtime_supports_weakref():
  # Pins the `#[pyclass(weakref)]` declaration the WeakKeyDictionary depends on.
  runtime = KernelRuntime(LoopPolicy(max_tokens=1000))
  ref = weakref.ref(runtime)
  assert ref() is runtime
