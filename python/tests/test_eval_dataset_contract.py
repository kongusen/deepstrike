from __future__ import annotations

from deepstrike import Dataset, DatasetCase, evaluate_dataset


class Agent:
    async def run(self, prompt: str):
        return {"output": prompt.upper()}


class LengthEvaluator:
    name = "length"

    def evaluate(self, *, test_case, output):
        return 1.0 if output == test_case.expected else 0.0


async def test_dataset_evaluation_runs_cases_and_collects_scores():
    run = await evaluate_dataset(
        Agent(),
        Dataset.from_cases([DatasetCase("a", "hello", expected="HELLO")]),
        [LengthEvaluator()],
        include_trace=True,
    )

    assert run.cases[0].scores == {"length": 1.0}
    assert run.mean_scores == {"length": 1.0}
    assert run.cases[0].trace["output"] == "HELLO"
