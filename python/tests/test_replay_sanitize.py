from deepstrike.runtime.replay_sanitize import (
  REPLAY_CONTENT_MAX_BYTES,
  sanitize_replay_text,
  truncate_bytes_at_char_boundary,
)


def test_truncate_bytes_at_char_boundary_cjk():
  text = "你好世界"
  assert truncate_bytes_at_char_boundary(text, 5) == "你"
  assert truncate_bytes_at_char_boundary(text, 12) == text


def test_sanitize_replay_text_under_cap():
  text = "短文本"
  assert sanitize_replay_text(text) == text


def test_sanitize_replay_text_over_cap():
  text = "你" * (REPLAY_CONTENT_MAX_BYTES // 3 + 100)
  out = sanitize_replay_text(text)
  assert out.endswith("… [replay truncated]")
  assert out.encode("utf-8")  # valid UTF-8
