"""Embedding data-plane adapters.

Embedding requests are intentionally separate from the generation ``ProtocolAdapter``
lifecycle: they do not render turns, stream tool calls, or maintain replay state. A
transport owns its client/retry policy and delegates only request construction and
response validation to these adapters.
"""
from __future__ import annotations

from dataclasses import dataclass
import math
from typing import Any, Sequence

from .model_registry import ResolvedProviderRuntime
from .protocol_adapter import ProtocolResponseError
from .usage import ProviderUsage, normalize_usage


@dataclass(frozen=True)
class EmbeddingRequestPlan:
    params: dict[str, Any]


@dataclass(frozen=True)
class EmbeddingResult:
    vectors: tuple[tuple[float, ...], ...]
    usage: ProviderUsage | None = None


def _get(raw: Any, field: str) -> Any:
    return raw.get(field) if isinstance(raw, dict) else getattr(raw, field, None)


class OpenAIEmbeddingAdapter:
    """OpenAI ``/embeddings`` request/response conversion.

    OpenAI, GLM and Qwen compatible endpoints share this adapter. Other embedding
    protocols must define their own data-plane adapter instead of approximating this
    wire format through the generation protocol surface.
    """

    protocol = "openai-embeddings"
    _COMPATIBLE_ENDPOINTS = frozenset({
        "openai.embeddings",
        "glm.openai.embeddings",
        "qwen.dashscope.embeddings",
    })

    def build_request(
        self,
        texts: Sequence[str],
        *,
        resolved: ResolvedProviderRuntime,
        dimensions: int | None = None,
        user: str | None = None,
    ) -> EmbeddingRequestPlan:
        if resolved.model is None or resolved.model.kind != "embedding":
            raise ValueError("OpenAI embeddings require an embedding model runtime")
        if resolved.endpoint_id not in self._COMPATIBLE_ENDPOINTS:
            raise ValueError(
                f"OpenAI embeddings do not support endpoint {resolved.endpoint_id!r}"
            )
        values = list(texts)
        if not values or any(not isinstance(text, str) or not text for text in values):
            raise ValueError("embedding input must be a non-empty sequence of non-empty strings")
        if dimensions is not None and (
            isinstance(dimensions, bool) or not isinstance(dimensions, int) or dimensions <= 0
        ):
            raise ValueError("embedding dimensions must be a positive integer")
        if user is not None and (not isinstance(user, str) or not user):
            raise ValueError("embedding user must be a non-empty string")

        params: dict[str, Any] = {"model": resolved.model_id, "input": values}
        if dimensions is not None:
            params["dimensions"] = dimensions
        if user is not None:
            params["user"] = user
        return EmbeddingRequestPlan(params=params)

    def decode_complete(self, raw: Any, *, expected_count: int) -> EmbeddingResult:
        if isinstance(expected_count, bool) or not isinstance(expected_count, int) or expected_count <= 0:
            raise ValueError("expected_count must be a positive integer")
        data = _get(raw, "data")
        if not isinstance(data, list) or len(data) != expected_count:
            raise ProtocolResponseError(self.protocol, "embedding data must match request input count")

        vectors: dict[int, tuple[float, ...]] = {}
        for item in data:
            index = _get(item, "index")
            values = _get(item, "embedding")
            if isinstance(index, bool) or not isinstance(index, int) or index in vectors:
                raise ProtocolResponseError(self.protocol, "embedding data indexes must be unique integers")
            if not isinstance(values, (list, tuple)) or not values:
                raise ProtocolResponseError(self.protocol, "embedding vector must be a non-empty numeric array")
            vector: list[float] = []
            for value in values:
                if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
                    raise ProtocolResponseError(self.protocol, "embedding vector contains a non-finite number")
                vector.append(float(value))
            vectors[index] = tuple(vector)
        if set(vectors) != set(range(expected_count)):
            raise ProtocolResponseError(self.protocol, "embedding data indexes do not match request input")

        try:
            usage = normalize_usage(_get(raw, "usage"))
        except ValueError as exc:
            raise ProtocolResponseError(self.protocol, str(exc)) from exc
        return EmbeddingResult(
            vectors=tuple(vectors[index] for index in range(expected_count)),
            usage=usage,
        )
