"""Typed provider-protocol lifecycle contracts.

Adapters convert a validated canonical input to one wire protocol. Transport ownership
(SDK clients, retries, circuit breakers, and durable replay/state stores) stays with
the provider runtime.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Generic, Protocol, TypeVar

from deepstrike._kernel import Message
from deepstrike.providers.stream import StreamEvent
from deepstrike.types.content import CanonicalAdapterInput

RequestT = TypeVar("RequestT")
CompleteT = TypeVar("CompleteT")
ChunkT = TypeVar("ChunkT")
StateT = TypeVar("StateT")
FinalT = TypeVar("FinalT")


@dataclass(frozen=True)
class AdapterOutput:
    events: list[StreamEvent] = field(default_factory=list)
    replay: dict[str, Any] | None = None
    run_state_patch: dict[str, Any] | None = None


@dataclass(frozen=True)
class AdapterDecodeInput:
    input: CanonicalAdapterInput


@dataclass(frozen=True)
class AdapterStreamInput:
    input: CanonicalAdapterInput


class ProtocolResponseError(ValueError):
    def __init__(self, protocol: str, message: str):
        self.protocol = protocol
        super().__init__(f"{protocol} protocol response error: {message}")


class ProtocolAdapter(Protocol, Generic[RequestT, CompleteT, ChunkT, StateT, FinalT]):
    protocol: str

    def build_request(self, input: CanonicalAdapterInput) -> RequestT: ...
    def decode_complete(self, raw: CompleteT, input: CanonicalAdapterInput) -> Message: ...
    def create_stream_state(self, input: CanonicalAdapterInput) -> StateT: ...
    def push_stream_chunk(self, chunk: ChunkT, state: StateT) -> AdapterOutput: ...
    def finish_stream(self, state: StateT, final: FinalT) -> AdapterOutput: ...
