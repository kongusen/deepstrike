from deepstrike._kernel import SkillMetadata
from deepstrike.runtime.kernel_step import skill_metadata_to_kernel


def test_skill_metadata_forwards_explicit_capability_grants_without_frontmatter_parsing() -> None:
  grant = {
    "id": "read-src",
    "kind": "tool",
    "resource": "/repo/src/**",
    "actions": ["read"],
    "constraints": [],
    "lease": None,
    "delegatable": False,
    "issuer": "skill:review",
  }
  skill = SkillMetadata(
    name="review",
    description="Review source files",
    capability_grants=[grant],
  )

  assert skill.capability_grants == [grant]
  assert skill_metadata_to_kernel(skill)["capability_grants"] == [grant]
  assert "capability_grants" not in skill_metadata_to_kernel(
    SkillMetadata(name="plain", description="No grants declared")
  )
