"""Pythonic dataset evaluation over the normal Agent/Kernel execution path."""
from __future__ import annotations

import asyncio
import inspect
from dataclasses import dataclass, field
from typing import Any, Protocol, Sequence


@dataclass(frozen=True)
class DatasetCase:
    id: str
    input: str
    expected: Any = None
    metadata: dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class Dataset:
    cases: tuple[DatasetCase, ...]
    name: str | None = None

    @classmethod
    def from_cases(cls, cases: Sequence[DatasetCase], *, name: str | None = None) -> "Dataset":
        return cls(tuple(cases), name=name)


class Evaluator(Protocol):
    name: str

    def evaluate(self, *, test_case: DatasetCase, output: str) -> float: ...


@dataclass
class CaseEvaluation:
    case: DatasetCase
    output: str
    scores: dict[str, float]
    trace: Any = None


@dataclass
class EvaluationRun:
    dataset: Dataset
    cases: list[CaseEvaluation]
    run_id: str | None = None

    @property
    def mean_scores(self) -> dict[str, float]:
        names = {name for case in self.cases for name in case.scores}
        return {
            name: sum(case.scores.get(name, 0.0) for case in self.cases) / len(self.cases)
            for name in names
        } if self.cases else {}


async def evaluate_dataset(
    agent: Any,
    dataset: Dataset,
    evaluators: Sequence[Evaluator],
    *,
    concurrency: int = 4,
    include_trace: bool = False,
) -> EvaluationRun:
    if concurrency < 1:
        raise ValueError("concurrency must be positive")
    semaphore = asyncio.Semaphore(concurrency)

    async def one(case: DatasetCase) -> CaseEvaluation:
        async with semaphore:
            result = await agent.run(case.input)
        output = result.get("output", "") if isinstance(result, dict) else str(result)
        scores: dict[str, float] = {}
        for evaluator in evaluators:
            value = evaluator.evaluate(test_case=case, output=output)
            if inspect.isawaitable(value):
                value = await value
            scores[evaluator.name] = float(value)
        return CaseEvaluation(case, output, scores, result if include_trace else None)

    return EvaluationRun(dataset, list(await asyncio.gather(*(one(case) for case in dataset.cases))))
