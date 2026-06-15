"""#6 (0.5.0 fold): the `gen_eval` workflow template — a loop worker + a bias-resistant verify eval
node carrying the kernel's verdict output_schema (the EvalPipeline successor). Mirrors the kernel
`gen_eval` shape test + guards SDK/kernel verdict-schema single-sourcing."""
import json

from deepstrike import gen_eval
from deepstrike._kernel import verdict_output_schema


def test_gen_eval_builds_loop_worker_and_bias_resistant_eval():
    spec = gen_eval("implement feature", "score against criteria", max_iters=3, extract_skill_on_pass=True)
    assert len(spec.nodes) == 2

    worker = spec.nodes[0]
    assert worker.role == "implement"
    assert worker.loop == {"max_iters": 3}
    assert not worker.depends_on

    eval_node = spec.nodes[1]
    assert eval_node.role == "verify"
    assert eval_node.isolation == "read_only"
    assert eval_node.context_inheritance == "none"  # bias-resistant
    assert eval_node.depends_on == [0]
    assert eval_node.output_schema is not None
    assert "passed" in eval_node.output_schema["properties"]
    assert "skill" in eval_node.output_schema["properties"]  # extract_skill_on_pass=True


def test_gen_eval_floors_max_iters_and_drops_skill_when_off():
    spec = gen_eval("w", "e", max_iters=0, extract_skill_on_pass=False)
    assert spec.nodes[0].loop == {"max_iters": 1}
    assert "skill" not in spec.nodes[1].output_schema["properties"]


def test_gen_eval_uses_kernel_verdict_schema_verbatim():
    spec = gen_eval("w", "e", max_iters=2, extract_skill_on_pass=True)
    assert spec.nodes[1].output_schema == json.loads(verdict_output_schema(True))
