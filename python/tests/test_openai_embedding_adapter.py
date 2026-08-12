"""OpenAI-compatible embedding adapter contracts."""
from __future__ import annotations

import pytest

from deepstrike.providers.model_registry import model_registry
from deepstrike.providers.protocol_adapter import ProtocolResponseError


def test_openai_embedding_adapter_builds_compatible_request_for_registry_runtime() -> None:
    from deepstrike.providers.embedding import OpenAIEmbeddingAdapter

    runtime = model_registry.resolve_provider_runtime("glm", "embedding-3")

    plan = OpenAIEmbeddingAdapter().build_request(
        ["alpha", "beta"], resolved=runtime, dimensions=1024, user="user-1",
    )

    assert plan.params == {
        "model": "embedding-3",
        "input": ["alpha", "beta"],
        "dimensions": 1024,
        "user": "user-1",
    }


def test_openai_embedding_adapter_rejects_generation_runtime_and_invalid_text_input() -> None:
    from deepstrike.providers.embedding import OpenAIEmbeddingAdapter

    adapter = OpenAIEmbeddingAdapter()
    generation = model_registry.resolve_provider_runtime("openai", "gpt-4o")
    embedding = model_registry.resolve_provider_runtime("openai", "text-embedding-3-large")

    with pytest.raises(ValueError, match="embedding model runtime"):
        adapter.build_request(["text"], resolved=generation)
    with pytest.raises(ValueError, match="non-empty strings"):
        adapter.build_request([""], resolved=embedding)
    gemini = model_registry.resolve_provider_runtime("gemini", "gemini-embedding-2")
    with pytest.raises(ValueError, match="do not support endpoint"):
        adapter.build_request(["text"], resolved=gemini)


def test_openai_embedding_adapter_decodes_indexed_vectors_and_usage() -> None:
    from deepstrike.providers.embedding import OpenAIEmbeddingAdapter

    decoded = OpenAIEmbeddingAdapter().decode_complete({
        "data": [
            {"index": 1, "embedding": [0, 1.5]},
            {"index": 0, "embedding": [2.0, -3]},
        ],
        "usage": {"prompt_tokens": 7, "total_tokens": 7},
    }, expected_count=2)

    assert decoded.vectors == ((2.0, -3.0), (0.0, 1.5))
    assert decoded.usage is not None
    assert decoded.usage.input_tokens == 7
    assert decoded.usage.output_tokens == 0


@pytest.mark.parametrize("raw", [
    {"data": [{"index": 0, "embedding": []}]},
    {"data": [{"index": 0, "embedding": [float("nan")]}]},
    {"data": [{"index": 0, "embedding": [1]}, {"index": 0, "embedding": [2]}]},
    {"data": [{"index": 1, "embedding": [1]}]},
])
def test_openai_embedding_adapter_rejects_malformed_or_misaligned_response(raw: dict) -> None:
    from deepstrike.providers.embedding import OpenAIEmbeddingAdapter

    with pytest.raises(ProtocolResponseError, match="embedding"):
        OpenAIEmbeddingAdapter().decode_complete(raw, expected_count=1)
