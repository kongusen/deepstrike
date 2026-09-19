"""Framework-facing API and report ABI for the Rust 0.2.69 verifiable runtime."""

from dataclasses import dataclass
import json
from typing import Callable, Literal, Mapping, Protocol, Sequence

VERIFIABLE_REPORT_SCHEMA = "verifiable-report/v1"
VERIFIABLE_FORK_SCHEMA = "verifiable-fork/v1"
VerifiableCommand = Literal["inspect", "verify", "replay", "fork"]
CheckVerdict = Literal["pass", "degraded", "fail", "unavailable"]
ReplayVerdict = Literal["pass", "fail", "unavailable"]
VerifiableReport = Mapping[str, object]


@dataclass(frozen=True)
class VerifiableEvidence:
    """Storage-neutral evidence supplied by a host adapter."""

    journal: Sequence[bytes]
    session_logs: Sequence[Sequence[bytes]] = ()
    checkpoints: Sequence[bytes] = ()


@dataclass(frozen=True)
class VerifyOptions:
    strict: bool = False
    require_complete: bool = False


@dataclass(frozen=True)
class ReplayOptions:
    strict: bool = False
    at_step: int | None = None


class VerifiableRuntimeAdapter(Protocol):
    def inspect(self, operation_id: str, evidence: VerifiableEvidence, strict: bool) -> VerifiableReport: ...

    def verify(self, operation_id: str, evidence: VerifiableEvidence, options: VerifyOptions) -> VerifiableReport: ...

    def replay(self, operation_id: str, evidence: VerifiableEvidence, options: ReplayOptions) -> VerifiableReport: ...

    def prepare_fork(
        self, operation_id: str, evidence: VerifiableEvidence, at_step: int, strict: bool
    ) -> "VerifiableForkManifest": ...


@dataclass(frozen=True)
class VerifiableForkManifest:
    schema: Literal["verifiable-fork/v1"]
    operation_id: str
    at_step: str
    parent_record_digest: str
    parent_input_id: str
    source_records: int


def create_verifiable_runtime_adapter(
    operation_json: Callable[[str], str],
) -> VerifiableRuntimeAdapter:
    """Create an adapter backed by the PyO3 Rust-core JSON bridge."""

    def call(
        operation_id: str,
        evidence: VerifiableEvidence,
        command: VerifiableCommand,
        *,
        strict: bool = False,
        require_complete: bool = False,
        at_step: int | None = None,
    ) -> VerifiableReport | VerifiableForkManifest:
        request = {
            "operation_id": operation_id,
            "command": command,
            "evidence": {
                "journal": [list(blob) for blob in evidence.journal],
                "session_logs": [[list(blob) for blob in stream] for stream in evidence.session_logs],
                "checkpoints": [list(blob) for blob in evidence.checkpoints],
            },
            "strict": strict,
            "require_complete": require_complete,
            "at_step": at_step,
        }
        result = json.loads(operation_json(json.dumps(request)))
        if command == "fork":
            return VerifiableForkManifest(**result)
        assert_verifiable_report_schema(result)
        return result

    class Adapter:
        def inspect(self, operation_id: str, evidence: VerifiableEvidence, strict: bool) -> VerifiableReport:
            return call(operation_id, evidence, "inspect", strict=strict)  # type: ignore[return-value]

        def verify(self, operation_id: str, evidence: VerifiableEvidence, options: VerifyOptions) -> VerifiableReport:
            return call(
                operation_id,
                evidence,
                "verify",
                strict=options.strict,
                require_complete=options.require_complete,
            )  # type: ignore[return-value]

        def replay(self, operation_id: str, evidence: VerifiableEvidence, options: ReplayOptions) -> VerifiableReport:
            return call(operation_id, evidence, "replay", strict=options.strict, at_step=options.at_step)  # type: ignore[return-value]

        def prepare_fork(
            self, operation_id: str, evidence: VerifiableEvidence, at_step: int, strict: bool
        ) -> VerifiableForkManifest:
            return call(operation_id, evidence, "fork", strict=strict, at_step=at_step)  # type: ignore[return-value]

    return Adapter()


def create_native_verifiable_runtime_adapter() -> VerifiableRuntimeAdapter:
    """Create the adapter from the installed PyO3 binding."""

    from deepstrike._kernel import verifiable_operation_json

    return create_verifiable_runtime_adapter(verifiable_operation_json)


class VerifiableOperation:
    """Storage-neutral framework handle delegating semantics to the Rust-core adapter."""

    def __init__(
        self, operation_id: str, evidence: VerifiableEvidence, adapter: VerifiableRuntimeAdapter
    ) -> None:
        self.operation_id = operation_id
        self.evidence = evidence
        self._adapter = adapter

    def inspect(self, strict: bool = False) -> VerifiableReport:
        return self._adapter.inspect(self.operation_id, self.evidence, strict)

    def verify(self, options: VerifyOptions | None = None) -> VerifiableReport:
        return self._adapter.verify(self.operation_id, self.evidence, options or VerifyOptions())

    def replay(self, options: ReplayOptions | None = None) -> VerifiableReport:
        return self._adapter.replay(self.operation_id, self.evidence, options or ReplayOptions())

    def prepare_fork(self, at_step: int, strict: bool = False) -> VerifiableForkManifest:
        return self._adapter.prepare_fork(self.operation_id, self.evidence, at_step, strict)


def assert_verifiable_report_schema(value: Mapping[str, object]) -> None:
    if value.get("schema") != VERIFIABLE_REPORT_SCHEMA:
        raise ValueError(f"unsupported verifiable report schema: {value.get('schema')!r}")
