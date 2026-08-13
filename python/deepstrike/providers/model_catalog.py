"""Static model facts and host-owned dynamic discovery cache.

Catalogs describe available models only. Capability selection remains in
``capability_router`` so discovery cannot become an implicit routing policy.
"""
from __future__ import annotations

from typing import Protocol

from .model_registry import ModelRegistration


class ModelCatalog(Protocol):
    async def list(self) -> tuple[ModelRegistration, ...]: ...
    async def get(self, model_id: str) -> ModelRegistration | None: ...


class ModelCatalogSource(Protocol):
    async def list(self) -> tuple[ModelRegistration, ...] | list[ModelRegistration]: ...


class StaticModelCatalog:
    """Immutable deterministic facts supplied by the SDK or application."""

    def __init__(self, registrations: tuple[ModelRegistration, ...] | list[ModelRegistration]) -> None:
        by_id: dict[str, ModelRegistration] = {}
        for registration in registrations:
            identifier = registration.descriptor.id
            if identifier in by_id:
                raise ValueError(f"Duplicate model catalog entry: {identifier}")
            by_id[identifier] = registration
        self._by_id = by_id
        self._registrations = tuple(by_id[key] for key in sorted(by_id))

    async def list(self) -> tuple[ModelRegistration, ...]:
        return self._registrations

    async def get(self, model_id: str) -> ModelRegistration | None:
        return self._by_id.get(model_id)


class DynamicModelCatalog:
    """Refreshable discovery snapshot with static fallback and last-good retention."""

    def __init__(self, source: ModelCatalogSource, fallback: ModelCatalog | None = None) -> None:
        self._source = source
        self._fallback = fallback or StaticModelCatalog(())
        self._snapshot: dict[str, ModelRegistration] = {}

    async def list(self) -> tuple[ModelRegistration, ...]:
        merged = {item.descriptor.id: item for item in await self._fallback.list()}
        merged.update(self._snapshot)
        return tuple(merged[key] for key in sorted(merged))

    async def get(self, model_id: str) -> ModelRegistration | None:
        return self._snapshot.get(model_id) or await self._fallback.get(model_id)

    async def refresh(self) -> dict[str, object]:
        try:
            next_snapshot: dict[str, ModelRegistration] = {}
            for registration in await self._source.list():
                identifier = registration.descriptor.id
                if identifier in next_snapshot:
                    raise ValueError("duplicate dynamic model catalog entry")
                next_snapshot[identifier] = registration
            self._snapshot = next_snapshot
            return {"ok": True}
        except Exception:
            return {"ok": False, "error_code": "refresh_failed"}
