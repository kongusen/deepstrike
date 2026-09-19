# README visuals

English and Chinese diagrams for the repository READMEs. These are conceptual
architecture diagrams, not screenshots or recordings of a live agent run.

- `architecture*.svg`: execution, verification, evaluation, and evolution.
- `execution*.gif`: four-step execution animation; 7.2 seconds per loop.
- `execution*.svg`: static alternative to the animation.
- `evaluation*.svg`: executed context input and evaluation evidence binding.
- `evolution*.svg`: proposal through activation at a new operation boundary.

Regenerate from the repository root with Python 3 and Pillow installed:

```sh
python3 scripts/render-readme-visuals.py --font /path/to/CJK-capable-font.ttf
```

The renderer also detects Arial Unicode on macOS, Noto Sans CJK on Linux,
and Microsoft YaHei on Windows, or accepts `README_VISUAL_FONT`.
SVGs contain selectable text and have a fixed background for light/dark pages.
Font rendering can vary across machines. GIFs embed their rendered text.

Semantic references: `docs/en/architecture/agent-process-runtime.md`,
`evaluation-context.md`, `verifiable-runtime.md`, and `evolution-runtime.md`.
When changing runtime contracts, update the renderer and regenerate both languages.
