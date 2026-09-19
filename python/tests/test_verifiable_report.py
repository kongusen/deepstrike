import json

from deepstrike.runtime.verifiable_report import (
    ReplayOptions,
    VERIFIABLE_FORK_SCHEMA,
    VERIFIABLE_REPORT_SCHEMA,
    VerifiableEvidence,
    VerifiableForkManifest,
    VerifiableOperation,
    create_verifiable_runtime_adapter,
    VerifyOptions,
    assert_verifiable_report_schema,
)


def test_report_and_fork_schema_names_match_rust_contract():
    assert VERIFIABLE_REPORT_SCHEMA == "verifiable-report/v2"
    assert VERIFIABLE_FORK_SCHEMA == "verifiable-fork/v2"
    assert_verifiable_report_schema({"schema": VERIFIABLE_REPORT_SCHEMA})


def test_unknown_report_schema_is_rejected():
    try:
        assert_verifiable_report_schema({"schema": "other/v1"})
    except ValueError:
        pass
    else:
        raise AssertionError("unknown report schema must be rejected")


def test_framework_operation_delegates_to_one_adapter():
    calls = []

    class Adapter:
        def inspect(self, operation_id, evidence, strict):
            calls.append("inspect")
            return {"command": "inspect"}

        def verify(self, operation_id, evidence, options):
            calls.append("verify")
            assert isinstance(options, VerifyOptions)
            return {"command": "verify"}

        def replay(self, operation_id, evidence, options):
            calls.append("replay")
            assert isinstance(options, ReplayOptions)
            return {"command": "replay"}

        def prepare_fork(self, operation_id, evidence, at_step, strict):
            calls.append("fork")
            return VerifiableForkManifest(
                VERIFIABLE_FORK_SCHEMA, operation_id, str(at_step), "sha256:p", "in", 1
            )

    operation = VerifiableOperation("op", VerifiableEvidence(journal=[]), Adapter())
    assert operation.inspect() == {"command": "inspect"}
    assert operation.verify() == {"command": "verify"}
    assert operation.replay() == {"command": "replay"}
    assert operation.prepare_fork(1).operation_id == "op"
    assert calls == ["inspect", "verify", "replay", "fork"]


def test_adapter_encodes_evidence_for_the_rust_json_bridge():
    requests = []

    def operation_json(request):
        requests.append(request)
        return '{"schema":"verifiable-report/v2","command":"inspect","operation_id":"op"}'

    adapter = create_verifiable_runtime_adapter(operation_json)
    report = VerifiableOperation("op", VerifiableEvidence(journal=[b"\x01\x02"]), adapter).inspect()
    assert report["operation_id"] == "op"
    assert json.loads(requests[0])["evidence"]["journal"] == [[1, 2]]
