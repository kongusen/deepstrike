"""
VerificationContract — first-class type for contract-driven development.

Contracts travel in the executor's system partition (never compressed) and
are given to the verifier alongside the artifact. The verifier never sees
the executor's implementation history — only the goal, contract, and artifact.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Optional


@dataclass
class AcceptanceCriterion:
    """A single verifiable acceptance criterion."""
    id: str
    text: str
    required: bool = True
    weight: float = 1.0
    machine_checkable: bool = False

    def __post_init__(self) -> None:
        self.weight = max(0.0, min(1.0, self.weight))


@dataclass
class VerificationContract:
    """
    First-class contract type: defines what correct looks like before execution starts.

    A VerificationContract is injected into the executor's system partition so it
    survives context renewal and compression. The verifier receives the contract
    alongside the artifact and checks each criterion independently, without access
    to the executor's implementation history.
    """
    id: str
    goal: str
    acceptance: list[AcceptanceCriterion] = field(default_factory=list)
    anti_patterns: list[str] = field(default_factory=list)
    evidence_required: list[str] = field(default_factory=list)


@dataclass
class ContractCheckResult:
    criterion_id: str
    passed: bool
    evidence: Optional[str] = None


class ContractBuilder:
    """Fluent builder for VerificationContract."""

    def __init__(self, id: str, goal: str) -> None:
        self._contract = VerificationContract(id=id, goal=goal)

    def criterion(
        self,
        id: str,
        text: str,
        *,
        required: bool = True,
        weight: float = 1.0,
        machine_checkable: bool = False,
    ) -> "ContractBuilder":
        self._contract.acceptance.append(
            AcceptanceCriterion(
                id=id, text=text, required=required,
                weight=weight, machine_checkable=machine_checkable,
            )
        )
        return self

    def anti_pattern(self, pattern: str) -> "ContractBuilder":
        self._contract.anti_patterns.append(pattern)
        return self

    def evidence(self, item: str) -> "ContractBuilder":
        self._contract.evidence_required.append(item)
        return self

    def build(self) -> VerificationContract:
        import copy
        return copy.deepcopy(self._contract)


def format_contract_for_system_prompt(contract: VerificationContract) -> str:
    """Render the contract as a markdown block for injection into a system prompt."""
    lines = [
        f"## Verification Contract: {contract.id}",
        "",
        f"Goal: {contract.goal}",
        "",
        "### Acceptance Criteria",
    ]
    for i, c in enumerate(contract.acceptance, 1):
        req = "[REQUIRED]" if c.required else "[OPTIONAL]"
        lines.append(f"{i}. {req} {c.text} (id: `{c.id}`, weight: {c.weight:.1f})")

    if contract.anti_patterns:
        lines += ["", "### Anti-Patterns (must avoid)"]
        lines += [f"- {p}" for p in contract.anti_patterns]

    if contract.evidence_required:
        lines += ["", "### Required Evidence"]
        lines += [f"- {e}" for e in contract.evidence_required]

    return "\n".join(lines)


def contract_to_criteria_strings(contract: VerificationContract) -> list[str]:
    """Derive criterion text for kernel in-run gates and compatibility-free body adapters."""
    return [c.text for c in contract.acceptance]
