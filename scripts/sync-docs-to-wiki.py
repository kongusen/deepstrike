#!/usr/bin/env python3
"""Sync docs/ markdown (zh root + en/) to a GitHub Wiki checkout.

Usage:
    python3 scripts/sync-docs-to-wiki.py [WIKI_DIR]

WIKI_DIR defaults to .wiki-sync (clone of https://github.com/kongusen/deepstrike.wiki.git)

Page naming:
    zh: docs/architecture/overview.md → Architecture-Overview.md
    en: docs/en/architecture/overview.md → En-Architecture-Overview.md
    zh home: docs/index.md → Home.md
    en home: docs/en/index.md → En-Home.md
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"
DEFAULT_WIKI = ROOT / ".wiki-sync"

SKIP_PARTS = {".vitepress", "wiki", "public"}
SKIP_FILES = {"README.md"}


def doc_locale(relpath: str) -> str:
    return "en" if relpath.startswith("en/") else "zh"


def content_relpath(relpath: str) -> str:
    """Strip locale prefix for path logic."""
    if relpath.startswith("en/"):
        return relpath[3:]
    return relpath


def wiki_page_name(relpath: str) -> str:
    locale = doc_locale(relpath)
    rp = content_relpath(relpath)
    if rp.endswith(".md"):
        rp = rp[:-3]
    if rp == "index":
        base = "Home"
    else:
        parts = []
        for seg in rp.split("/"):
            if seg == "index":
                continue
            words = re.split(r"[-_]", seg)
            parts.append("-".join(w.capitalize() for w in words if w))
        base = "-".join(parts)
    if locale == "en":
        return f"En-{base}"
    return base


def strip_frontmatter(text: str) -> str:
    if text.startswith("---"):
        end = text.find("\n---", 3)
        if end != -1:
            return text[end + 4 :].lstrip("\n")
    return text


def build_link_maps(md_files: list[Path]) -> dict[str, dict[str, str]]:
    """Per-locale link target → wiki page name."""
    maps: dict[str, dict[str, str]] = {"zh": {}, "en": {}}
    for f in md_files:
        rel = f.relative_to(DOCS).as_posix()
        locale = doc_locale(rel)
        rp = content_relpath(rel)
        rp_no_ext = rp[:-3] if rp.endswith(".md") else rp
        wname = wiki_page_name(rel)
        m = maps[locale]

        m[rel] = wname
        m[rp_no_ext] = wname
        prefix = "/en" if locale == "en" else ""
        m[f"{prefix}/{rp_no_ext}"] = wname
        if prefix:
            m[f"/{rp_no_ext}"] = wname  # relative links in en pages

        if rp_no_ext.endswith("/index"):
            base = rp_no_ext[: -len("/index")] or "index"
            m[base] = wname
            m[f"{prefix}/{base}"] = wname
            m[f"{prefix}/{base}/"] = wname

    maps["zh"]["index"] = "Home"
    maps["zh"]["/"] = "Home"
    maps["en"]["index"] = "En-Home"
    maps["en"]["/en"] = "En-Home"
    maps["en"]["/en/"] = "En-Home"
    return maps


def rewrite_links(text: str, link_map: dict[str, str]) -> str:
    def repl(match: re.Match[str]) -> str:
        label, target = match.group(1), match.group(2)
        if target.startswith("http") or target.startswith("#"):
            return match.group(0)
        anchor = ""
        if "#" in target:
            target, anchor = target.split("#", 1)
            anchor = f"#{anchor}"
        clean = target.strip("/")
        if clean.endswith(".md"):
            clean = clean[:-3]
        if clean.endswith("/index"):
            clean = clean[: -len("/index")] or "index"
        if clean.startswith("en/"):
            clean = clean[3:]

        wname = (
            link_map.get(clean)
            or link_map.get(f"/{clean}")
            or link_map.get(f"/en/{clean}")
            or link_map.get(f"{clean}.md")
        )
        if wname:
            return f"[{label}]({wname}{anchor})"
        return match.group(0)

    return re.sub(r"\[([^\]]*)\]\(([^)]+)\)", repl, text)


SIDEBAR_ZH = """**[Home](Home)** · [English (En-Home)](En-Home)

### 入门
- [Getting-Started-Installation](Getting-Started-Installation)
- [Getting-Started-Hello-Agent](Getting-Started-Hello-Agent)
- [Getting-Started-Run-Agent-Vs-Runner](Getting-Started-Run-Agent-Vs-Runner)
- [Getting-Started-Providers](Getting-Started-Providers)

### 架构
- [Architecture-Agent-Os](Architecture-Agent-Os)
- [Architecture-Overview](Architecture-Overview)
- [Architecture-Execution-Model](Architecture-Execution-Model)
- [Architecture-Kernel-Abi](Architecture-Kernel-Abi)
- [Architecture-Session-Replay](Architecture-Session-Replay)

### 功能指南
- [Guides-Execution-Plane-And-Tools](Guides-Execution-Plane-And-Tools)
- [Guides-Context-Engineering](Guides-Context-Engineering)
- [Guides-Skills](Guides-Skills)
- [Guides-Memory](Guides-Memory)
- [Guides-Workflow](Guides-Workflow)
- [Guides-Structured-Output-And-Reducers](Guides-Structured-Output-And-Reducers)
- [Guides-Governance](Guides-Governance)
- [Guides-Provider-Routing](Guides-Provider-Routing)
- [Guides-Session-Replay-And-Recovery](Guides-Session-Replay-And-Recovery)
- [Guides-Os-Profile-And-Snapshots](Guides-Os-Profile-And-Snapshots)
- [Guides-Signals-And-Reactive](Guides-Signals-And-Reactive)
- [Guides-Sub-Agents-And-Collaboration](Guides-Sub-Agents-And-Collaboration)
- [Guides-Harness-And-Eval](Guides-Harness-And-Eval)
- [Guides-Milestones](Guides-Milestones)

### 概念
- [Concepts](Concepts)
- [Concepts-Roles-And-Isolation](Concepts-Roles-And-Isolation)
- [Concepts-Prompt-Cache-Design](Concepts-Prompt-Cache-Design)
- [Concepts-Run-Group-Budget](Concepts-Run-Group-Budget)

### 参考
- [Reference-Runtime-Options](Reference-Runtime-Options)
- [Reference-Workflow-Node-Spec](Reference-Workflow-Node-Spec)
- [Reference-Python-Api](Reference-Python-Api)
"""

SIDEBAR_EN = """
-----

**[En-Home](En-Home)** · [简体中文 (Home)](Home)

### Getting Started
- [En-Getting-Started-Installation](En-Getting-Started-Installation)
- [En-Getting-Started-Hello-Agent](En-Getting-Started-Hello-Agent)
- [En-Getting-Started-Run-Agent-Vs-Runner](En-Getting-Started-Run-Agent-Vs-Runner)
- [En-Getting-Started-Providers](En-Getting-Started-Providers)

### Architecture
- [En-Architecture-Agent-Os](En-Architecture-Agent-Os)
- [En-Architecture-Overview](En-Architecture-Overview)
- [En-Architecture-Execution-Model](En-Architecture-Execution-Model)
- [En-Architecture-Kernel-Abi](En-Architecture-Kernel-Abi)
- [En-Architecture-Session-Replay](En-Architecture-Session-Replay)

### Guides
- [En-Guides-Execution-Plane-And-Tools](En-Guides-Execution-Plane-And-Tools)
- [En-Guides-Context-Engineering](En-Guides-Context-Engineering)
- [En-Guides-Skills](En-Guides-Skills)
- [En-Guides-Memory](En-Guides-Memory)
- [En-Guides-Workflow](En-Guides-Workflow)
- [En-Guides-Structured-Output-And-Reducers](En-Guides-Structured-Output-And-Reducers)
- [En-Guides-Governance](En-Guides-Governance)
- [En-Guides-Provider-Routing](En-Guides-Provider-Routing)
- [En-Guides-Session-Replay-And-Recovery](En-Guides-Session-Replay-And-Recovery)
- [En-Guides-Os-Profile-And-Snapshots](En-Guides-Os-Profile-And-Snapshots)
- [En-Guides-Signals-And-Reactive](En-Guides-Signals-And-Reactive)
- [En-Guides-Sub-Agents-And-Collaboration](En-Guides-Sub-Agents-And-Collaboration)
- [En-Guides-Harness-And-Eval](En-Guides-Harness-And-Eval)
- [En-Guides-Milestones](En-Guides-Milestones)

### Concepts
- [En-Concepts](En-Concepts)
- [En-Concepts-Roles-And-Isolation](En-Concepts-Roles-And-Isolation)
- [En-Concepts-Prompt-Cache-Design](En-Concepts-Prompt-Cache-Design)
- [En-Concepts-Run-Group-Budget](En-Concepts-Run-Group-Budget)

### Reference
- [En-Reference-Runtime-Options](En-Reference-Runtime-Options)
- [En-Reference-Workflow-Node-Spec](En-Reference-Workflow-Node-Spec)
- [En-Reference-Python-Api](En-Reference-Python-Api)
"""


def collect_md_files() -> list[Path]:
    files: list[Path] = []
    for f in sorted(DOCS.rglob("*.md")):
        if f.name in SKIP_FILES:
            continue
        if any(part in SKIP_PARTS for part in f.parts):
            continue
        files.append(f)
    return files


def main() -> int:
    wiki_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_WIKI
    wiki_dir.mkdir(parents=True, exist_ok=True)

    md_files = collect_md_files()
    link_maps = build_link_maps(md_files)

    for src in md_files:
        rel = src.relative_to(DOCS).as_posix()
        locale = doc_locale(rel)
        wname = wiki_page_name(rel)
        text = strip_frontmatter(src.read_text(encoding="utf-8"))
        text = rewrite_links(text, link_maps[locale])
        dst = wiki_dir / f"{wname}.md"
        dst.write_text(text, encoding="utf-8")
        print(f"  [{locale}] {rel} → {wname}.md")

    sidebar = SIDEBAR_ZH + SIDEBAR_EN
    (wiki_dir / "_Sidebar.md").write_text(sidebar, encoding="utf-8")
    print(f"\nWiki sync complete → {wiki_dir} ({len(md_files)} pages, zh+en)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
