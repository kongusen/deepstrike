# DeepStrike README animations

The octopus marks the active step. In the parallel workflow it splits into two readers, then meets at the editor. Each animation loops and holds on each step so the caption can be read. The first four diagrams show how context, task notes, memory and Skills change during a release-brief task.

| Story | English | 中文 |
| --- | --- | --- |
| Build the next model request | [SVG](turn-view.svg) · [GIF](turn-view.gif) | [SVG](turn-view-zh.svg) · [GIF](turn-view-zh.gif) |
| Keep a task note during compaction | [SVG](pressure-notes.svg) · [GIF](pressure-notes.gif) | [SVG](pressure-notes-zh.svg) · [GIF](pressure-notes-zh.gif) |
| Propose, review and recall a memory | [SVG](memory-cycle.svg) · [GIF](memory-cycle.gif) | [SVG](memory-cycle-zh.svg) · [GIF](memory-cycle-zh.gif) |
| Activate a Skill and change the tool list | [SVG](skill-cycle.svg) · [GIF](skill-cycle.gif) | [SVG](skill-cycle-zh.svg) · [GIF](skill-cycle-zh.gif) |
| Read a source with a tool | [SVG](tool-loop.svg) · [GIF](tool-loop.gif) | [SVG](tool-loop-zh.svg) · [GIF](tool-loop-zh.gif) |
| Reopen a saved result and keep a note | [SVG](context.svg) · [GIF](context.gif) | [SVG](context-zh.svg) · [GIF](context-zh.gif) |
| Read in parallel and combine the results | [SVG](team.svg) · [GIF](team.gif) | [SVG](team-zh.svg) · [GIF](team-zh.gif) |
| Revise a declined draft and ask again | [SVG](approval.svg) · [GIF](approval.gif) | [SVG](approval-zh.svg) · [GIF](approval-zh.gif) |

## Edit and export

Edit the SVG for each language. Every file contains its own mascot, text, paths and SMIL animation. Open it directly in a browser to preview. The GIF exporter samples the SVG at 10 fps and combines identical frames into reading pauses. It supports the linear translations and discrete opacity changes used here.

From the repository root, with Python 3.9+, Node.js 22.12+ and a Chinese font such as PingFang SC or Noto Sans CJK SC installed:

```sh
# Install the image encoder outside the project dependency tree.
tools_dir="$(mktemp -d)"
npm install --prefix "$tools_dir" sharp@0.35.5
NODE_PATH="$tools_dir/node_modules" python3 scripts/export-readme-gifs.py
```

Pass one or more SVG paths to export only those files. The older architecture, execution, evaluation and evolution illustrations can still be generated with `scripts/render-readme-visuals.py`.
