"""A#2: SDK-side execution of the kernel's control-flow workflow node kinds (Loop / Classify /
Tournament).

The kernel owns the scheduling — it re-arms loops, prunes classify branches, and runs the tournament
bracket — and tells the SDK *which* kind a spawn is via the spawn descriptor (``loop_max_iters`` /
``classify_labels`` / ``judge_match``). This module is the SDK half of the "one agent per node + one
additive result field" contract: it builds the prompt that solicits the decision from the node's agent
and extracts the matching result signal (``loop_continue`` / ``classify_branch`` /
``tournament_winner``) the kernel reads back.
"""

from __future__ import annotations

import json
from typing import Any

from deepstrike.runtime.output_schema import extract_json_value


def loop_instruction(max_iters: int) -> str:
  """Instruction appended to a loop node's goal: do the next increment, and signal when done."""
  return (
    f"This task runs as a LOOP (up to {max_iters} iterations total). Do the next increment of work "
    'now. When you judge the overall task COMPLETE and no further iterations are needed, end your '
    'response with a JSON object {"loop_continue": false}. To request another iteration, omit it or '
    'return {"loop_continue": true}.'
  )


def classify_instruction(labels: list[str]) -> str:
  """Instruction appended to a classify node's goal: pick exactly one of the kernel's branch labels."""
  joined = ", ".join(json.dumps(lbl) for lbl in labels)
  return (
    f"Classify the input and choose EXACTLY ONE label from: {joined}. "
    'Respond with ONLY a JSON object: {"branch": "<one of the labels>"}.'
  )


def judge_goal(criterion: str, left_output: str, right_output: str) -> str:
  """Build a tournament judge's goal: the controller's criterion + the two candidates to compare."""
  return (
    f"{criterion}\n\nCompare the two candidate outputs below and decide which one better satisfies "
    f"the criterion above.\n\n[CANDIDATE left]\n{left_output}\n\n[CANDIDATE right]\n{right_output}\n\n"
    'Respond with ONLY a JSON object: {"winner": "left"} or {"winner": "right"}.'
  )


def extract_loop_continue(text: str) -> bool | None:
  """Extract a loop stop signal from a loop iteration's output. Returns the ``loop_continue`` value,
  or None when the agent gave no clear signal (⇒ the kernel runs the loop to ``max_iters``). Accepts
  ``{"loop_continue": bool}`` or, leniently, ``{"done": bool}`` (continue = not done)."""
  v = extract_json_value(text)
  if isinstance(v, dict):
    if isinstance(v.get("loop_continue"), bool):
      return v["loop_continue"]
    if isinstance(v.get("loopContinue"), bool):
      return v["loopContinue"]
    if isinstance(v.get("done"), bool):
      return not v["done"]
  return None


def extract_classify_branch(text: str, labels: list[str]) -> str | None:
  """Extract the chosen branch label from a classifier's output. Prefers ``{"branch": "..."}``; falls
  back to a bare label string that exactly matches one of the valid labels. Returns None when no
  recognizable choice was made (the kernel then prunes every branch — a safe "none matched")."""
  v = extract_json_value(text)
  if isinstance(v, dict):
    if isinstance(v.get("branch"), str):
      return v["branch"]
    if isinstance(v.get("label"), str):
      return v["label"]
  if isinstance(v, str) and v in labels:
    return v
  trimmed = (text or "").strip()
  if trimmed in labels:
    return trimmed
  return None


def extract_judge_winner(text: str) -> str:
  """Extract a tournament judge's verdict ("left" or "right"). Defaults to "left" when the verdict is
  unparseable, so the bracket always advances to a champion rather than stalling with no winner."""
  v: Any = extract_json_value(text)
  if isinstance(v, dict):
    w = v.get("winner")
    if w == "right":
      return "right"
    if w == "left":
      return "left"
  lowered = (text or "").lower()
  if "right" in lowered and "left" not in lowered:
    return "right"
  return "left"
