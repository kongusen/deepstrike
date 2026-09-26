import json
from pathlib import Path


def test_spc_028_shared_semantic_contract_fixture():
    fixture = json.loads(
        (Path(__file__).parents[2] / "tests" / "fixtures" / "runtime-language" / "semantic-contract.json").read_text()
    )
    assert fixture["version"] == "0.2.75"
    assert "Agent" in fixture["public"]
    assert fixture["authorities"]["AgentDefinition"] == "public-agent"
