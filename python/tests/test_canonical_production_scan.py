"""Task 20: production path must not call the legacy direct-step API."""

from __future__ import annotations

from pathlib import Path

RUNTIME = Path(__file__).resolve().parents[1] / "deepstrike" / "runtime"

# Modules that form the production host surface after Task 20 cutover.
PRODUCTION = (
    RUNTIME / "runner.py",
    RUNTIME / "canonical_kernel_step.py",
    RUNTIME / "sub_agent_orchestrator.py",
)

FORBIDDEN = (
    ".step(",
    "_kernel_step(",
    "kernel_apply(",
    "kernel_action(",
    "kernel_maybe_action(",
    '"start_run"',
    '"load_workflow"',
    '"complete_run"',
    "ABI-v1",
    "ABI-v2",
    "CANONICAL_KERNEL_ABI_VERSION = 3",
    "kernel_transaction_log",
    "append_kernel_genesis",
    "compare_and_append_kernel_transaction",
    "submit_workflow_nodes_to_kernel",
    "submit_workflow_to_kernel",
    "bootstrap_workflow",
    "skill_activated",
)


def test_production_modules_do_not_call_legacy_direct_step() -> None:
    missing = [path for path in PRODUCTION if not path.is_file()]
    # canonical_kernel_step.py is created by the cutover; until then skip its absence.
    required = [path for path in PRODUCTION if path.name != "canonical_kernel_step.py" or path.is_file()]
    assert RUNTIME.joinpath("runner.py").is_file()
    hits: list[str] = []
    for path in required:
        text = path.read_text(encoding="utf-8")
        for needle in FORBIDDEN:
            if needle in text:
                # Allow imports of symbols only when commented as deprecated/test — production
                # callsites are the failure mode. Import lines for the wrappers themselves are OK
                # only inside the legacy kernel_step module (not scanned here).
                for line_no, line in enumerate(text.splitlines(), start=1):
                    if needle in line and not line.lstrip().startswith("#"):
                        # Import of wrappers is itself forbidden on production modules.
                        hits.append(f"{path.name}:{line_no}: {line.strip()}")
    assert hits == [], "legacy direct-step surface still present:\n" + "\n".join(hits)
    assert not missing or all(path.name == "canonical_kernel_step.py" for path in missing)
