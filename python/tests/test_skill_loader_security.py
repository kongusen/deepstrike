import pytest

from deepstrike.skills.loader import read_skill_file


def test_read_skill_file_rejects_path_traversal(tmp_path):
    skill_dir = tmp_path / "skills"
    skill_dir.mkdir()
    (tmp_path / "secret.md").write_text("must not be readable", encoding="utf-8")

    with pytest.raises(ValueError, match="invalid skill name"):
        read_skill_file(skill_dir, "../secret")
