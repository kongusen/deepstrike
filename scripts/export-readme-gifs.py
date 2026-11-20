#!/usr/bin/env python3
"""Export the README SVG animations to GIF. Requires Python 3.9+, Node and sharp 0.35+."""
import argparse
from bisect import bisect_right
from copy import deepcopy
import json
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
import xml.etree.ElementTree as ET

SVG = "http://www.w3.org/2000/svg"
ET.register_namespace("", SVG)
ET.register_namespace("xlink", "http://www.w3.org/1999/xlink")
VISUALS = Path(__file__).resolve().parents[1] / "docs/public/readme"

# Rasterize the actual SVG frames with librsvg. Coalesce stationary frames before
# encoding so a two-second reading pause needs only one image.
ENCODE = """
const fs = require('node:fs');
const path = require('node:path');
const sharp = require('sharp');
async function encode(dir, output) {
  const frames = JSON.parse(fs.readFileSync(path.join(dir, 'frames.json'), 'utf8'));
  const images = [];
  for (const frame of frames) {
    images.push(await sharp(path.join(dir, frame.file)).png().toBuffer());
  }
  await sharp(images, { join: { animated: true } })
    .gif({ loop: 0, delay: frames.map(f => f.delay), colours: 128, dither: 0 })
    .toFile(output);
}
encode(...process.argv.slice(1)).catch(err => { console.error(err); process.exitCode = 1; });
"""


def snapshot(root, progress):
    """Seek the two SMIL operations used in these diagrams: visibility and translation."""
    frame = deepcopy(root)
    for group in frame.iter():
        for anim in list(group):
            if anim.tag not in (f"{{{SVG}}}animate", f"{{{SVG}}}animateTransform"):
                continue
            times = list(map(float, anim.attrib["keyTimes"].split(";")))
            values = anim.attrib["values"].split(";")
            i = min(bisect_right(times, progress) - 1, len(values) - 2)
            if anim.get("attributeName") == "opacity" and anim.get("calcMode") == "discrete":
                group.set("opacity", values[i])
            elif anim.get("type") == "translate" and anim.get("calcMode") == "linear":
                portion = (progress - times[i]) / (times[i+1] - times[i])
                start, end = (map(float, v.split()) for v in values[i:i+2])
                xy = [a + (b-a) * portion for a, b in zip(start, end)]
                group.set("transform", f"translate({xy[0]:.2f} {xy[1]:.2f})")
            else:
                raise ValueError(f"Unsupported README animation: {anim.attrib}")
            group.remove(anim)
    return ET.tostring(frame, encoding="utf-8")


def export(source):
    root = ET.parse(source).getroot()
    durations = {a.attrib["dur"] for a in root.iter() if "dur" in a.attrib}
    if len(durations) != 1:
        raise ValueError(f"{source} must have one shared loop duration")
    milliseconds = round(float(durations.pop().removesuffix("s")) * 1000)
    with TemporaryDirectory() as tmp:
        folder = Path(tmp)
        frames = []
        previous = None
        for time in range(0, milliseconds, 100):
            svg = snapshot(root, time / milliseconds)
            if svg != previous:
                file = f"{len(frames):03d}.svg"
                (folder / file).write_bytes(svg)
                frames.append({"file": file, "delay": 0})
                previous = svg
            frames[-1]["delay"] += min(100, milliseconds - time)
        (folder / "frames.json").write_text(json.dumps(frames))
        subprocess.run(["node", "-e", ENCODE, tmp, str(source.with_suffix(".gif"))], check=True)
    print(f"{source.name}: {len(frames)} frames, {milliseconds / 1000:g} s", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("files", type=Path, nargs="*")
    options = parser.parse_args()
    files = options.files or [
        VISUALS / f"{topic}{locale}.svg"
        for topic in ("turn-view", "pressure-notes", "memory-cycle", "skill-cycle",
                      "tool-loop", "context", "team", "approval") for locale in ("", "-zh")
    ]
    for file in files:
        export(file)
