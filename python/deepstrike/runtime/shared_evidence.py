"""Host-side shared evidence read model (mirror of node ``runtime/shared-evidence.ts``).

Memory records, memory candidates, knowledge entries and execution evidence share one reference
shape, :class:`EvidenceRef`, assembled read-only from the existing logs. Nothing here writes:
knowledge and memory keep their own write semantics. The cross-SDK behaviour is pinned by the
``shared_evidence_snapshot`` conformance fixture.
"""

from __future__ import annotations

import dataclasses
import hashlib
import inspect
import json
import re
from collections.abc import Iterable, Sequence
from dataclasses import dataclass, field
from typing import Any, Literal, Protocol

from deepstrike.runtime.verifiable_report import VerifiableEvidence

EvidenceDomain = Literal[
    "journal_record", "session_event", "checkpoint", "context_binding", "provider_attempt",
    "prompt_measurement", "wire_replay", "knowledge_entry", "memory_record", "memory_candidate",
]
EvidenceAuthority = Literal["kernel_fact", "host_observation", "derived_projection"]
EvidenceAvailability = Literal["resident", "external", "missing", "redacted"]
EvidenceCompleteness = Literal["complete", "partial", "unavailable"]
MemorySettlement = Literal["pending", "accepted", "rejected"]
#: ``recalled_from``: a knowledge entry is the in-context copy of a recalled memory record.
#: ``settled_as``: a memory candidate was accepted as this memory record.
EvidenceRelation = Literal["recalled_from", "settled_as"]

DOMAINS: tuple[str, ...] = (
    "journal_record", "session_event", "checkpoint", "context_binding", "provider_attempt",
    "prompt_measurement", "wire_replay", "knowledge_entry", "memory_record", "memory_candidate",
)
AUTHORITIES: tuple[str, ...] = ("kernel_fact", "host_observation", "derived_projection")
AVAILABILITIES: tuple[str, ...] = ("resident", "external", "missing", "redacted")
SETTLEMENTS: tuple[str, ...] = ("pending", "accepted", "rejected")
RELATIONS: tuple[str, ...] = ("recalled_from", "settled_as")
COMPLETENESS: tuple[str, ...] = ("complete", "partial", "unavailable")
_MEMORY_DOMAINS = frozenset({"memory_record", "memory_candidate"})
_EXECUTION_DOMAINS = frozenset({"provider_attempt", "prompt_measurement", "wire_replay", "context_binding"})
_MAX_DIAGNOSTIC_MESSAGE = 200


@dataclass(frozen=True)
class EvidenceScope:
    """The host scope in which an evidence reference is meaningful."""

    operation_id: str
    session_id: str | None = None
    agent_id: str | None = None
    tenant_id: str | None = None
    namespace: str | None = None


@dataclass(frozen=True)
class EvidenceDiagnostic:
    code: str
    message: str
    domain: str | None = None
    ref_id: str | None = None


@dataclass(frozen=True)
class EvidenceRetentionObservation:
    """Lifecycle observations copied from the source; never a shared retention policy."""

    pinned: bool | None = None
    observed_ttl_days: int | None = None
    prunable: bool | None = None


@dataclass(frozen=True)
class EvidenceLink:
    """A domain-qualified edge to another ref; resolved within the snapshot."""

    relation: str
    domain: str
    ref_id: str


@dataclass(frozen=True)
class EvidenceRef:
    ref_id: str
    domain: str
    authority: str
    scope: EvidenceScope
    availability: str
    content_digest: str | None = None
    payload_ref: str | None = None
    size_bytes: int | None = None
    version: str | None = None
    provenance: str | None = None
    retention: EvidenceRetentionObservation | None = None
    settlement: str | None = None
    #: Opaque source ids (e.g. memory ``provenance.evidence_refs``); not resolved here.
    evidence_refs: tuple[str, ...] | None = None
    #: Edges to other refs in this snapshot, sorted; a link whose target is absent is a gap.
    links: tuple[EvidenceLink, ...] | None = None


@dataclass(frozen=True)
class EvidenceCursor:
    """Source positions a snapshot was read at; ``-1`` means read and empty."""

    session_log_seq: int | None = None
    journal_step_seq: int | None = None
    checkpoint_ordinal: int | None = None


@dataclass(frozen=True)
class SharedEvidenceIdentity:
    operation_id: str
    scope: EvidenceScope


@dataclass(frozen=True)
class SharedEvidenceFacets:
    execution: tuple[EvidenceRef, ...] | None = None
    references: tuple[EvidenceRef, ...] | None = None


@dataclass(frozen=True)
class SharedEvidenceSnapshot:
    identity: SharedEvidenceIdentity
    verification: VerifiableEvidence
    refs: tuple[EvidenceRef, ...]
    facets: SharedEvidenceFacets
    completeness: str
    diagnostics: tuple[EvidenceDiagnostic, ...]
    as_of: EvidenceCursor | None = None


SharedEvidence = SharedEvidenceSnapshot


def _require_text(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise TypeError(f"{label} must be non-empty")
    return value


def _assert_member(value: Any, allowed: Sequence[str], label: str) -> None:
    if value not in allowed:
        raise TypeError(f"{label} is invalid: {value}")


def _safe_integer(value: Any, minimum: int) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and minimum <= value <= 2**53 - 1


def _utf16(text: str) -> bytes:
    """Sort key matching JavaScript's ordinal (UTF-16 code unit) string comparison."""
    return text.encode("utf-16-be", "surrogatepass")


def _ref_key(domain: str, ref_id: str) -> str:
    return json.dumps([domain, ref_id], ensure_ascii=False, separators=(",", ":"))


def _link_key(link: EvidenceLink) -> str:
    return json.dumps([link.domain, link.ref_id, link.relation], ensure_ascii=False, separators=(",", ":"))


def _copy_scope(scope: EvidenceScope) -> EvidenceScope:
    _require_text(scope.operation_id, "scope.operation_id")
    for name in ("session_id", "agent_id", "tenant_id", "namespace"):
        value = getattr(scope, name)
        if value is not None:
            _require_text(value, f"scope.{name}")
    return scope


def _copy_links(links: Iterable[EvidenceLink]) -> tuple[EvidenceLink, ...]:
    by_key: dict[str, EvidenceLink] = {}
    for link in links:
        _assert_member(link.relation, RELATIONS, "evidence link.relation")
        _assert_member(link.domain, DOMAINS, "evidence link.domain")
        _require_text(link.ref_id, "evidence link.ref_id")
        copy = EvidenceLink(relation=link.relation, domain=link.domain, ref_id=link.ref_id)
        by_key[_link_key(copy)] = copy
    return tuple(by_key[key] for key in sorted(by_key, key=_utf16))


def _copy_ref(ref: EvidenceRef, expected_scope: EvidenceScope | None) -> EvidenceRef:
    if not isinstance(ref, EvidenceRef):
        raise TypeError("evidence ref must be an EvidenceRef")
    _require_text(ref.ref_id, "evidence ref.ref_id")
    _assert_member(ref.domain, DOMAINS, "evidence ref.domain")
    _assert_member(ref.authority, AUTHORITIES, "evidence ref.authority")
    _assert_member(ref.availability, AVAILABILITIES, "evidence ref.availability")
    scope = _copy_scope(ref.scope)
    if expected_scope is not None and scope != expected_scope:
        raise ValueError(f"evidence ref {ref.ref_id} crosses the shared evidence scope")
    for name in ("content_digest", "payload_ref", "version", "provenance"):
        if getattr(ref, name) is not None:
            _require_text(getattr(ref, name), f"evidence ref.{name}")
    if ref.size_bytes is not None and not _safe_integer(ref.size_bytes, 0):
        raise TypeError("evidence ref.size_bytes must be a non-negative safe integer")
    if ref.settlement is not None:
        _assert_member(ref.settlement, SETTLEMENTS, "evidence ref.settlement")
        if ref.domain not in _MEMORY_DOMAINS:
            raise TypeError(f"evidence ref.settlement is only valid on memory refs: {ref.domain}")
    retention = ref.retention
    if retention is not None:
        if retention.observed_ttl_days is not None and not _safe_integer(retention.observed_ttl_days, 0):
            raise TypeError("evidence ref.retention.observed_ttl_days must be a non-negative safe integer")
        retention = EvidenceRetentionObservation(
            pinned=None if retention.pinned is None else bool(retention.pinned),
            observed_ttl_days=retention.observed_ttl_days,
            prunable=None if retention.prunable is None else bool(retention.prunable),
        )
    evidence_refs = None if ref.evidence_refs is None else tuple(
        _require_text(item, "evidence ref.evidence_refs[]") for item in ref.evidence_refs)
    links = None if ref.links is None else (_copy_links(ref.links) or None)
    return dataclasses.replace(ref, scope=scope, retention=retention, evidence_refs=evidence_refs, links=links)


def _copy_cursor(cursor: EvidenceCursor) -> EvidenceCursor:
    for name, minimum in (("session_log_seq", -1), ("journal_step_seq", -1), ("checkpoint_ordinal", 0)):
        value = getattr(cursor, name)
        if value is not None and not _safe_integer(value, minimum):
            raise TypeError(f"as_of.{name} must be a safe integer >= {minimum}")
    return cursor


def _copy_diagnostic(diagnostic: EvidenceDiagnostic) -> EvidenceDiagnostic:
    _require_text(diagnostic.code, "diagnostic.code")
    _require_text(diagnostic.message, "diagnostic.message")
    if diagnostic.domain is not None:
        _assert_member(diagnostic.domain, DOMAINS, "diagnostic.domain")
    return diagnostic


_RANK = {"complete": 0, "partial": 1, "unavailable": 2}


def _worse(left: str, right: str) -> str:
    return right if _RANK[right] > _RANK[left] else left


def _derive_completeness(
    refs: Sequence[EvidenceRef], verification: VerifiableEvidence,
) -> tuple[str, list[EvidenceDiagnostic]]:
    diagnostics: list[EvidenceDiagnostic] = []
    completeness = "complete"
    if not refs and not verification.journal:
        diagnostics.append(EvidenceDiagnostic("evidence_empty", "snapshot has no references and no journal bytes"))
        return "unavailable", diagnostics
    present = {_ref_key(ref.domain, ref.ref_id) for ref in refs}
    for ref in refs:
        for link in ref.links or ():
            if _ref_key(link.domain, link.ref_id) in present:
                continue
            completeness = _worse(completeness, "partial")
            diagnostics.append(EvidenceDiagnostic(
                "evidence_link_unresolved",
                f"{link.relation} target {link.domain}/{link.ref_id} is not in the snapshot",
                ref.domain, ref.ref_id,
            ))
        if ref.domain == "knowledge_entry" and ref.content_digest is None:
            completeness = _worse(completeness, "partial")
            diagnostics.append(EvidenceDiagnostic(
                "knowledge_digest_missing", "knowledge reference has no content digest", ref.domain, ref.ref_id))
        if ref.domain == "knowledge_entry" and ref.provenance is None:
            completeness = _worse(completeness, "partial")
            diagnostics.append(EvidenceDiagnostic(
                "knowledge_provenance_missing", "knowledge reference has no provenance from its sidecar",
                ref.domain, ref.ref_id))
        if ref.availability == "missing":
            completeness = _worse(completeness, "partial")
            diagnostics.append(EvidenceDiagnostic(
                "evidence_missing", "referenced evidence is not available from its source", ref.domain, ref.ref_id))
    return completeness, diagnostics


def _split_facets(refs: Sequence[EvidenceRef]) -> SharedEvidenceFacets:
    execution = tuple(ref for ref in refs if ref.domain in _EXECUTION_DOMAINS)
    references = tuple(ref for ref in refs if ref.domain not in _EXECUTION_DOMAINS)
    return SharedEvidenceFacets(execution=execution or None, references=references or None)


def _copy_verification(evidence: VerifiableEvidence) -> VerifiableEvidence:
    return VerifiableEvidence(
        journal=tuple(bytes(item) for item in evidence.journal),
        session_logs=tuple(tuple(bytes(item) for item in stream) for stream in evidence.session_logs),
        checkpoints=tuple(bytes(item) for item in evidence.checkpoints),
    )


class SharedEvidenceBuilder:
    def __init__(
        self,
        *,
        operation_id: str,
        verification: VerifiableEvidence,
        scope: EvidenceScope | None = None,
        as_of: EvidenceCursor | None = None,
        refs: Iterable[EvidenceRef] = (),
        completeness: str = "complete",
        diagnostics: Iterable[EvidenceDiagnostic] = (),
    ) -> None:
        _require_text(operation_id, "operation_id")
        base = scope or EvidenceScope(operation_id=operation_id)
        self._scope = _copy_scope(dataclasses.replace(base, operation_id=operation_id))
        self._identity = SharedEvidenceIdentity(operation_id=operation_id, scope=self._scope)
        self._as_of = None if as_of is None else _copy_cursor(as_of)
        self._verification = _copy_verification(verification)
        self._diagnostics = [_copy_diagnostic(item) for item in diagnostics]
        _assert_member(completeness, COMPLETENESS, "completeness")
        self._completeness = completeness
        self._refs: dict[str, EvidenceRef] = {}
        for ref in refs:
            self.add_ref(ref)

    def add_ref(self, ref: EvidenceRef) -> SharedEvidenceBuilder:
        copy = _copy_ref(ref, self._scope)
        key = _ref_key(copy.domain, copy.ref_id)
        existing = self._refs.get(key)
        if existing is not None:
            if existing != copy:
                raise ValueError(f"conflicting evidence ref: {copy.domain}/{copy.ref_id}")
            self._diagnostics.append(EvidenceDiagnostic(
                "duplicate_evidence_ref", "identical evidence ref was supplied more than once", copy.domain, copy.ref_id))
            return self
        self._refs[key] = copy
        return self

    def mark_partial(self, diagnostic: EvidenceDiagnostic) -> SharedEvidenceBuilder:
        self._completeness = _worse(self._completeness, "partial")
        self._diagnostics.append(_copy_diagnostic(diagnostic))
        return self

    def build(self) -> SharedEvidenceSnapshot:
        refs = tuple(self._refs[key] for key in sorted(self._refs, key=_utf16))
        derived, derived_diagnostics = _derive_completeness(refs, self._verification)
        return SharedEvidenceSnapshot(
            identity=self._identity,
            as_of=self._as_of,
            verification=self._verification,
            refs=refs,
            facets=_split_facets(refs),
            completeness=_worse(self._completeness, derived),
            diagnostics=tuple(self._diagnostics) + tuple(derived_diagnostics),
        )


def create_shared_evidence(**kwargs: Any) -> SharedEvidenceSnapshot:
    """Build a deterministic, immutable host-side evidence snapshot without performing I/O."""
    return SharedEvidenceBuilder(**kwargs).build()


class SessionEvidenceReader:
    """Read-only query facade over one immutable snapshot."""

    def __init__(self, snapshot: SharedEvidenceSnapshot) -> None:
        self.snapshot = snapshot

    @classmethod
    async def read(cls, **sources: Any) -> SessionEvidenceReader:
        return cls(await read_session_evidence(**sources))

    def refs(self, domain: str | None = None) -> tuple[EvidenceRef, ...]:
        if domain is None:
            return self.snapshot.refs
        return tuple(ref for ref in self.snapshot.refs if ref.domain == domain)

    def by_ref_id(self, domain: str, ref_id: str) -> EvidenceRef | None:
        return next((ref for ref in self.snapshot.refs if ref.domain == domain and ref.ref_id == ref_id), None)

    def by_digest(self, content_digest: str) -> tuple[EvidenceRef, ...]:
        return tuple(ref for ref in self.snapshot.refs if ref.content_digest == content_digest)

    def linked(self, ref: EvidenceRef, relation: str | None = None) -> tuple[EvidenceRef, ...]:
        """The refs ``ref`` links to; unresolved links are skipped."""
        targets = (self.by_ref_id(link.domain, link.ref_id)
                   for link in ref.links or () if relation is None or link.relation == relation)
        return tuple(target for target in targets if target is not None)

    def linking(self, ref: EvidenceRef, relation: str | None = None) -> tuple[EvidenceRef, ...]:
        """The refs that link to ``ref``: e.g. the knowledge copies of a memory record."""
        return tuple(source for source in self.snapshot.refs if any(
            link.domain == ref.domain and link.ref_id == ref.ref_id and (relation is None or link.relation == relation)
            for link in source.links or ()))


class EvidenceSidecar(Protocol):
    """Read-only host lookup for facts the logs do not carry. Every method is optional and may be
    sync or async; an absent or failing lookup degrades the snapshot instead of inventing the fact.
    ``memory_candidate_receipt`` is a lookup, never ``MemoryCandidateSink.submit``."""

    def knowledge_provenance(self, entry_id: str, content_digest: str) -> Any: ...

    def memory_candidate_receipt(self, agent_id: str, candidate_id: str) -> Any: ...


# --------------------------------------------------------------------------------------------- #
# Reading the logs
# --------------------------------------------------------------------------------------------- #


def _canonical_value(value: Any) -> Any:
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        return _canonical_value(dataclasses.asdict(value))
    if isinstance(value, dict):
        return {key: _canonical_value(item) for key, item in value.items() if item is not None}
    if isinstance(value, (list, tuple)):
        return [_canonical_value(item) for item in value]
    if isinstance(value, float) and value.is_integer() and abs(value) <= 2**53:
        return int(value)
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    if hasattr(value, "__dict__"):
        return _canonical_value({key: item for key, item in vars(value).items() if not key.startswith("_")})
    raise TypeError(f"session event cannot serialize {type(value).__name__}")


def _canonical_text(value: Any) -> str:
    if isinstance(value, dict):
        items = sorted(value.items(), key=lambda item: _utf16(item[0]))
        return "{" + ",".join(f"{json.dumps(key, ensure_ascii=False)}:{_canonical_text(item)}" for key, item in items) + "}"
    if isinstance(value, list):
        return "[" + ",".join(_canonical_text(item) for item in value) + "]"
    return json.dumps(value, ensure_ascii=False)


def canonical_event_json(value: Any) -> str:
    """Canonical JSON for a session event: keys sorted by UTF-16 code unit, no whitespace, members
    that are absent or ``None`` dropped, integral numbers without a fraction. Matches the node,
    wasm and Rust SDKs byte for byte, so one event has one digest everywhere."""
    return _canonical_text(_canonical_value(value))


def _sha256(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


_KNOWLEDGE_ORIGINS = {"memory": "memory", "skill": "skill"}
_ORDINAL = re.compile(r"^\d+$")


@dataclass(frozen=True)
class KnowledgeIdentity:
    ref_id: str
    key: str
    origin: str | None = None
    links: tuple[EvidenceLink, ...] | None = None


def knowledge_identity(entry_id: str, content_digest: str) -> KnowledgeIdentity:
    """A knowledge entry's identity is ``<key>@<digest>``. The kernel's ``entry_id`` is
    ``knowledge:<ordinal>:<key>:<digest>``; the ordinal shifts when an earlier entry is swept, so it
    is dropped. Another shape is used whole as the key. ``memory:<record_id>`` keys link the entry to
    the recalled record; ``skill:<name>`` names a skill body."""
    prefix, suffix = "knowledge:", f":{content_digest}"
    key = entry_id
    if entry_id.startswith(prefix) and entry_id.endswith(suffix):
        body = entry_id[len(prefix):len(entry_id) - len(suffix)]
        separator = body.find(":")
        if separator > 0 and _ORDINAL.match(body[:separator]) and separator < len(body) - 1:
            key = body[separator + 1:]
    ref_id = f"{key}@{content_digest}"
    separator = key.find(":")
    origin = _KNOWLEDGE_ORIGINS.get(key[:separator]) if separator > 0 else None
    if origin is None:
        return KnowledgeIdentity(ref_id=ref_id, key=key)
    target = key[separator + 1:]
    if origin == "memory" and target:
        return KnowledgeIdentity(ref_id, key, origin, (EvidenceLink("recalled_from", "memory_record", target),))
    return KnowledgeIdentity(ref_id, key, origin)


class _ObservedRefs:
    """Refs read from logs; a later observation of the same ref refines the earlier one."""

    def __init__(self, scope: EvidenceScope) -> None:
        self._scope = scope
        self._by_key: dict[str, dict[str, Any]] = {}

    def put(self, **fields: Any) -> None:
        key = _ref_key(fields["domain"], fields["ref_id"])
        merged = dict(self._by_key.get(key, {}))
        merged.update({name: value for name, value in fields.items() if value is not None})
        self._by_key[key] = merged

    def refine(self, domain: str, ref_id: str, **fields: Any) -> None:
        if _ref_key(domain, ref_id) in self._by_key:
            self.put(domain=domain, ref_id=ref_id, **fields)

    def has(self, domain: str, ref_id: str) -> bool:
        return _ref_key(domain, ref_id) in self._by_key

    def get(self, domain: str, ref_id: str) -> dict[str, Any] | None:
        return self._by_key.get(_ref_key(domain, ref_id))

    def candidates(self) -> list[dict[str, Any]]:
        return [fields for fields in self._by_key.values() if fields["domain"] == "memory_candidate"]

    def values(self) -> list[EvidenceRef]:
        return [EvidenceRef(scope=self._scope, **fields) for fields in self._by_key.values()]


def _memory_in_scope(memory: dict[str, Any], scope: EvidenceScope) -> bool:
    return ((scope.tenant_id is None or scope.tenant_id == memory.get("tenant_id"))
            and (scope.namespace is None or scope.namespace == memory.get("namespace")))


def _describe(error: BaseException) -> str:
    text = str(error)
    return text if len(text) <= _MAX_DIAGNOSTIC_MESSAGE else text[:_MAX_DIAGNOSTIC_MESSAGE] + "…"


def _observe_event(
    seq: int,
    event: dict[str, Any],
    data: bytes,
    refs: _ObservedRefs,
    scope: EvidenceScope,
    knowledge: dict[str, tuple[str, str]],
    diagnostics: list[EvidenceDiagnostic],
) -> None:
    payload_ref = f"sessionlog:{seq}"
    refs.put(ref_id=str(seq), domain="session_event", authority="host_observation",
             content_digest=_sha256(data), size_bytes=len(data), payload_ref=payload_ref, availability="resident")

    def out_of_scope(domain: str, ref_id: str) -> None:
        diagnostics.append(EvidenceDiagnostic(
            "memory_scope_mismatch", "memory event belongs to another tenant or namespace and was excluded",
            domain, ref_id))

    kind = event.get("kind")
    if kind == "provider_attempt":
        refs.put(ref_id=f"{event['effect_id']}#{event['attempt_seq']}", domain="provider_attempt",
                 authority="host_observation", payload_ref=payload_ref, availability="resident")
    elif kind == "prompt_measured":
        source = (event.get("measurement") or {}).get("source") or {}
        refs.put(ref_id=event.get("effect_id") or f"turn:{event['turn']}", domain="prompt_measurement",
                 authority="host_observation", payload_ref=payload_ref, availability="resident",
                 provenance=source.get("kind"))
    elif kind == "llm_completed":
        wire = event.get("wire_evidence")
        if wire is None:
            return
        refs.put(ref_id=event.get("invocation_id") or event.get("effect_id") or f"turn:{event['turn']}",
                 domain="wire_replay", authority="host_observation", payload_ref=payload_ref,
                 availability="resident", version=wire.get("response_id"))
    elif kind == "context_prepared":
        preparation = event["preparation"]
        refs.put(ref_id=event["effect_id"], domain="context_binding", authority="derived_projection",
                 content_digest=preparation["binding"]["digest"], payload_ref=payload_ref, availability="resident")
        for entry in preparation["state"]["knowledge"]:
            identity = knowledge_identity(entry["entry_id"], entry["content_digest"])
            knowledge[identity.ref_id] = (entry["entry_id"], entry["content_digest"])
            refs.put(ref_id=identity.ref_id, domain="knowledge_entry", authority="derived_projection",
                     content_digest=entry["content_digest"], payload_ref=payload_ref, availability="resident",
                     provenance=identity.origin, links=identity.links)
    elif kind == "memory_written":
        if not _memory_in_scope(event["scope"], scope):
            out_of_scope("memory_record", event["record_id"])
            return
        refs.put(ref_id=event["record_id"], domain="memory_record", authority="host_observation",
                 payload_ref=payload_ref, size_bytes=event.get("size_bytes"), availability="external",
                 settlement="accepted")
    elif kind == "memory_retrieval_result":
        for hit in event.get("hits", []):
            record = hit["record"]
            if not _memory_in_scope(record["scope"], scope):
                out_of_scope("memory_record", record["record_id"])
                continue
            provenance = record.get("provenance") or {}
            # Empty lists are omitted, as the Rust SessionLog never writes them.
            evidence_refs = tuple(provenance.get("evidence_refs") or ()) or None
            refs.put(ref_id=record["record_id"], domain="memory_record", authority="host_observation",
                     payload_ref=payload_ref, availability="external",
                     provenance=f"{provenance.get('author')}/{provenance.get('trust')}", settlement="accepted",
                     evidence_refs=evidence_refs,
                     retention=EvidenceRetentionObservation(
                         pinned=record.get("pinned"), observed_ttl_days=record.get("ttl_days")))
    elif kind in ("memory_candidate_pending", "memory_candidate_rejected"):
        refs.put(ref_id=event["candidate_id"], domain="memory_candidate", authority="host_observation",
                 payload_ref=payload_ref, availability="external", provenance=event.get("source"),
                 settlement="pending" if kind == "memory_candidate_pending" else "rejected",
                 version=event.get("receipt_id"))


async def _maybe_await(value: Any) -> Any:
    return await value if inspect.isawaitable(value) else value


def _receipt_conflict(candidate: dict[str, Any], receipt: dict[str, Any]) -> str | None:
    """``receipt_id`` names one sink answer (spec §2.3): a later review may answer again under a new
    id, but the same status under a different id contradicts the log; a logged rejection is final."""
    if receipt.get("candidate_id") != candidate["ref_id"]:
        return f"sink receipt names candidate {receipt.get('candidate_id')}"
    if candidate.get("settlement") == "rejected" and receipt.get("status") != "rejected":
        return f"sink receipt ({receipt.get('status')}) disagrees with the logged candidate (rejected)"
    version = candidate.get("version")
    if receipt.get("status") == candidate.get("settlement") and version is not None and receipt.get("receipt_id") != version:
        return f"sink receipt {receipt.get('receipt_id')} renames the logged {receipt.get('status')} answer {version}"
    return None


async def _refine_candidates(
    sidecar: Any, scope: EvidenceScope, refs: _ObservedRefs, diagnostics: list[EvidenceDiagnostic],
) -> None:
    lookup = getattr(sidecar, "memory_candidate_receipt", None) if sidecar is not None else None
    candidates = refs.candidates()
    if lookup is None or not candidates:
        return
    if scope.agent_id is None:
        diagnostics.append(EvidenceDiagnostic(
            "memory_candidate_agent_unscoped",
            "candidate receipts are keyed by agent_id, which the evidence scope does not carry",
            "memory_candidate"))
        return
    for candidate in candidates:
        ref_id = candidate["ref_id"]
        try:
            receipt = await _maybe_await(lookup(scope.agent_id, ref_id))
        except Exception as error:  # noqa: BLE001 - a failing sidecar degrades, never breaks the read
            diagnostics.append(EvidenceDiagnostic("sidecar_unreadable", _describe(error), "memory_candidate", ref_id))
            continue
        if receipt is None:
            continue
        conflict = _receipt_conflict(candidate, receipt)
        if conflict is not None:
            diagnostics.append(EvidenceDiagnostic("memory_candidate_receipt_conflict", conflict, "memory_candidate", ref_id))
            continue
        accepted = receipt.get("status") == "accepted"
        settled = accepted and refs.has("memory_record", receipt["record_ref"])
        refs.refine(
            "memory_candidate", ref_id,
            settlement=receipt["status"], version=receipt["receipt_id"],
            content_digest=receipt["digest"] if accepted else None,
            payload_ref=receipt["record_ref"] if accepted else None,
            links=(EvidenceLink("settled_as", "memory_record", receipt["record_ref"]),) if settled else None,
        )


async def read_session_evidence(
    *,
    operation_id: str,
    session_id: str,
    session_log: Any,
    scope: EvidenceScope | dict[str, Any] | None = None,
    kernel_journal: Any = None,
    sidecar: Any = None,
) -> SharedEvidenceSnapshot:
    """Read one operation's evidence from the existing logs into an immutable snapshot. Only read
    methods are called; a source that fails to read becomes a diagnostic and degrades completeness.
    ``scope`` carries ``agent_id``/``tenant_id``/``namespace``; operation and session come from the
    arguments."""
    _require_text(operation_id, "operation_id")
    _require_text(session_id, "session_id")
    extra = scope if isinstance(scope, dict) else (
        {} if scope is None else {name: getattr(scope, name) for name in ("agent_id", "tenant_id", "namespace")})
    evidence_scope = _copy_scope(EvidenceScope(
        operation_id=operation_id, session_id=session_id,
        agent_id=extra.get("agent_id"), tenant_id=extra.get("tenant_id"), namespace=extra.get("namespace")))
    refs = _ObservedRefs(evidence_scope)
    diagnostics: list[EvidenceDiagnostic] = []
    knowledge: dict[str, tuple[str, str]] = {}

    session_seq: int | None = None
    session_blobs: list[bytes] = []
    try:
        last_seq = -1
        for entry in await session_log.read(session_id):
            last_seq = max(last_seq, entry.seq)
            data = canonical_event_json(entry.event).encode("utf-8")
            session_blobs.append(data)
            _observe_event(entry.seq, entry.event, data, refs, evidence_scope, knowledge, diagnostics)
        session_seq = last_seq
    except Exception as error:  # noqa: BLE001
        diagnostics.append(EvidenceDiagnostic("session_log_unreadable", _describe(error)))

    journal_step: int | None = None
    checkpoint_ordinal: int | None = None
    journal: list[bytes] = []
    checkpoints: list[bytes] = []
    if kernel_journal is None:
        diagnostics.append(EvidenceDiagnostic("journal_not_provided", "no KernelJournal was supplied"))
    else:
        try:
            entries = await kernel_journal.read_from(operation_id)
            last_step = -1
            for entry in entries:
                last_step = entry.step_seq
                journal.append(bytes(entry.record_bytes))
                refs.put(ref_id=str(entry.step_seq), domain="journal_record", authority="kernel_fact",
                         content_digest=entry.record_digest, size_bytes=len(entry.record_bytes), availability="resident")
            checkpoint = await kernel_journal.latest_checkpoint(operation_id)
            journal_step = last_step
            if checkpoint is not None:
                checkpoint_ordinal = checkpoint.ordinal
                checkpoints.append(bytes(checkpoint.checkpoint_bytes))
                refs.put(ref_id=checkpoint.checkpoint_id, domain="checkpoint", authority="kernel_fact",
                         content_digest=checkpoint.state_digest, size_bytes=len(checkpoint.checkpoint_bytes),
                         version=str(checkpoint.ordinal), availability="resident")
            first_step = entries[0].step_seq if entries else 0
            if first_step > 0 and (checkpoint is None or checkpoint.through_step_seq < first_step - 1):
                diagnostics.append(EvidenceDiagnostic(
                    "journal_prefix_pruned",
                    f"journal records before step {first_step} are pruned and no checkpoint covers them",
                    "journal_record"))
        except Exception as error:  # noqa: BLE001
            diagnostics.append(EvidenceDiagnostic("journal_unreadable", _describe(error)))

    lookup = getattr(sidecar, "knowledge_provenance", None) if sidecar is not None else None
    if lookup is not None:
        for ref_id, (entry_id, content_digest) in knowledge.items():
            try:
                provenance = await _maybe_await(lookup(entry_id, content_digest))
                if provenance is not None:
                    refs.refine("knowledge_entry", ref_id, provenance=provenance)
            except Exception as error:  # noqa: BLE001
                diagnostics.append(EvidenceDiagnostic("sidecar_unreadable", _describe(error), "knowledge_entry", ref_id))

    await _refine_candidates(sidecar, evidence_scope, refs, diagnostics)

    as_of = EvidenceCursor(session_log_seq=session_seq, journal_step_seq=journal_step,
                           checkpoint_ordinal=checkpoint_ordinal)
    has_cursor = any(value is not None for value in (session_seq, journal_step, checkpoint_ordinal))
    builder = SharedEvidenceBuilder(
        operation_id=operation_id,
        scope=evidence_scope,
        as_of=as_of if has_cursor else None,
        verification=VerifiableEvidence(
            journal=tuple(journal),
            session_logs=(tuple(session_blobs),) if session_blobs else (),
            checkpoints=tuple(checkpoints),
        ),
        refs=refs.values(),
    )
    for diagnostic in diagnostics:
        builder.mark_partial(diagnostic)
    return builder.build()


__all__ = [
    "EvidenceAuthority", "EvidenceAvailability", "EvidenceCompleteness", "EvidenceCursor", "EvidenceDiagnostic",
    "EvidenceDomain", "EvidenceLink", "EvidenceRef", "EvidenceRelation", "EvidenceRetentionObservation",
    "EvidenceScope", "EvidenceSidecar", "KnowledgeIdentity", "MemorySettlement", "SessionEvidenceReader",
    "SharedEvidence", "SharedEvidenceBuilder", "SharedEvidenceFacets", "SharedEvidenceIdentity",
    "SharedEvidenceSnapshot", "canonical_event_json", "create_shared_evidence", "knowledge_identity",
    "read_session_evidence",
]
