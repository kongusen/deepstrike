#!/usr/bin/env python3
"""Render README SVG diagrams and a GIF from the same layout. Requires Pillow.

Run: python3 scripts/render-readme-visuals.py
Optional: --font /path/to/a/CJK-capable.ttf (or README_VISUAL_FONT).
"""
import argparse
import math
import os
from pathlib import Path
from xml.sax.saxutils import escape
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'docs/public/readme'
BG, PANEL, TEXT, MUTED, LINE = '#111714', '#1c2520', '#f4f1e8', '#adb9ae', '#405348'
ACCENTS = ['#b9e77a', '#80d7d0', '#b8afff', '#ffbb85']
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--font', default=os.environ.get('README_VISUAL_FONT'))
args = parser.parse_args()
fonts = [args.font, '/Library/Fonts/Arial Unicode.ttf',
         '/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc',
         'C:/Windows/Fonts/msyh.ttc']
FONT = next((p for p in fonts if p and Path(p).is_file()), None)
if not FONT:
    parser.error('Supply a CJK-capable font with --font or README_VISUAL_FONT.')
OUT.mkdir(parents=True, exist_ok=True)


class Canvas:
    def __init__(self, title, subtitle, height=650):
        self.im = Image.new('RGB', (1200, height), BG)
        self.d = ImageDraw.Draw(self.im)
        self.svg = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1200 {height}" role="img" aria-labelledby="title desc">',
                    f'<title id="title">{escape(title)}</title><desc id="desc">{escape(subtitle)}</desc>',
                    '<g font-family="Arial, Noto Sans CJK SC, Microsoft YaHei, sans-serif">']
        self.box(0, 0, 1200, height, BG, BG, 0)
        self.text(42, 28, 'DEEPSTRIKE  /  RUNTIME NOTES  /  0.2.70', 15, ACCENTS[0])
        self.text(42, 65, title, 34)
        self.text(42, 117, subtitle, 19, MUTED)

    def box(self, x, y, w, h, fill=PANEL, stroke=LINE, radius=18):
        self.d.rounded_rectangle((x, y, x+w, y+h), radius, fill, stroke, 2)
        self.svg.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{radius}" fill="{fill}" stroke="{stroke}" stroke-width="2"/>')

    def text(self, x, y, s, size=22, fill=TEXT):
        f = ImageFont.truetype(FONT, size)
        assert self.d.textlength(s, font=f) <= 1160-x, f'Text overflows: {s}'
        self.d.text((x, y), s, fill, font=f)
        self.svg.append(f'<text x="{x}" y="{y+size}" font-size="{size}" fill="{fill}">{escape(s)}</text>')

    def arrow(self, x1, y1, x2, y2, color=LINE):
        self.d.line((x1, y1, x2, y2), color, 3)
        a = math.atan2(y2-y1, x2-x1)
        pts = [(x2, y2)] + [(x2-10*math.cos(a+t), y2-10*math.sin(a+t)) for t in (-.5, .5)]
        self.d.polygon(pts, color)
        self.svg.append(f'<path d="M{x1} {y1} L{x2} {y2}" stroke="{color}" stroke-width="3"/>')
        self.svg.append(f'<polygon points="{" ".join(f"{x},{y}" for x,y in pts)}" fill="{color}"/>')

    def save(self, name):
        (OUT / f'{name}.svg').write_text('\n'.join(self.svg + ['</g></svg>'])+'\n')


def pick(zh, en, cn):
    return cn if zh else en


def execution(zh, active=-1):
    p = lambda en, cn: pick(zh, en, cn)
    c = Canvas(p('How an agent takes its next step', 'Agent 如何迈出下一步'),
               p('The host reports intent and facts. The kernel owns decisions and state transitions.',
                 'Host 提交意图与事实，Kernel 负责决策与状态转换。'), 560)
    labels = [('Intent', p('Request work', '提出任务')), ('Kernel', p('Admit + decide', '准入与决策')),
              ('Effect', p('Host performs I/O', 'Host 执行外部操作')), ('Fact', p('Report the result', '报告执行结果'))]
    for i, (title, sub) in enumerate(labels):
        x = 42+i*288
        color = ACCENTS[i] if active in (-1, i) else LINE
        c.box(x, 203, 252, 142, stroke=color)
        c.text(x+20, 218, f'0{i+1}', 16, color)
        c.text(x+20, 251, title, 30)
        c.text(x+20, 300, sub, 19, MUTED)
        if i < 3:
            c.arrow(x+258, 276, x+281, 276, color)
    c.arrow(1032, 353, 1032, 389, ACCENTS[3])
    c.arrow(1032, 389, 456, 389, ACCENTS[3])
    c.arrow(456, 389, 456, 353, ACCENTS[3])
    notes = [p('An application asks for work through an explicit Intent.', '应用通过明确的 Intent 请求执行任务。'),
             p('The kernel checks capabilities, budgets and lifecycle rules.', 'Kernel 检查权限、预算与生命周期规则。'),
             p('The host executes admitted effects through providers and tools.', 'Host 通过模型与工具执行已获准的 Effect。'),
             p('Returned facts drive the next kernel transition.', '返回的 Fact 驱动 Kernel 进入下一状态。')]
    c.text(42, 432, notes[max(active, 0)], 22, ACCENTS[max(active, 0)])
    c.text(42, 481, p('Conceptual sequence • execution evidence can be retained in a durable host log.',
                       '概念流程示意 · 执行证据可保存在 Host 的持久化日志中。'), 18, MUTED)
    return c


def architecture(zh):
    p = lambda en, cn: pick(zh, en, cn)
    c = Canvas(p('One runtime, four connected responsibilities', '一个 Runtime，四项相互衔接的职责'),
               p('Application: agents, skills, tools, memory and workflows', '应用层 · Agent、Skill、工具、记忆与工作流'), 775)
    rows = [
        ('01  Execute', 'Agent Process Runtime', p('Lifecycle · scheduling · capabilities · budget', '生命周期 · 调度 · 权限 · 预算')),
        ('02  Verify', 'Verifiable Runtime', p('Inspect · verify · evidence-only replay · read-only fork', '检查 · 验证 · 基于证据重放 · 只读分叉')),
        ('03  Evaluate', 'Evaluation context', p('Bind executed inputs to host-provided evaluation evidence', '把实际执行输入绑定到应用提供的评估证据')),
        ('04  Evolve', 'EvolutionRuntime', p('Validate proposal, evaluation, promotion and activation', '验证提案、评估、批准与激活之间的关系')),
    ]
    for i, (label, title, body) in enumerate(rows):
        y = 177+i*126
        c.box(42, y, 1116, 104)
        c.text(64, y+33, label, 25, ACCENTS[i])
        c.text(320, y+15, title, 24)
        c.text(320, y+55, body, 20, MUTED)
        if i < 3:
            c.arrow(600, y+108, 600, y+121)
    c.text(42, 707, p('Shared Rust core + SDKs: Node.js / Python / Rust / WASM',
                       '共享 Rust 核心 + SDK · Node.js / Python / Rust / WASM'), 21, ACCENTS[0])
    c.save('architecture'+('-zh' if zh else ''))
    return c


def evaluation(zh):
    p = lambda en, cn: pick(zh, en, cn)
    c = Canvas(p('What exactly did you evaluate?', '这次评估，究竟评了什么？'),
               p('EvaluationContextBinding anchors findings to the executed context input.',
                 'EvaluationContextBinding 将评估关联到实际执行的上下文输入。'), 655)
    labels = [('ContextState + Policy', p('State and resolved controls', '上下文状态与已解析策略')),
              ('ContextPlan + Snapshot', p('Selection and rendered context', '选择计划与渲染快照')),
              ('Measurement + Route', p('Prompt count and provider route', '提示词计量与模型路由'))]
    for i, (title, detail) in enumerate(labels):
        y = 184+i*117
        c.box(42, y, 410, 96)
        c.text(62, y+15, title, 23, ACCENTS[1])
        c.text(62, y+54, detail, 19, MUTED)
        c.arrow(462, y+48, 496, y+48, ACCENTS[1])
    c.box(507, 184, 651, 330, stroke=ACCENTS[1])
    c.text(535, 211, 'ContextExecutionInput', 30)
    c.text(535, 263, p('Frozen input identity', '冻结的执行输入身份'), 23, MUTED)
    c.arrow(831, 308, 831, 344, ACCENTS[1])
    c.text(535, 355, 'EvaluationContextBinding', 29, ACCENTS[1])
    c.text(535, 411, p('References included in host evaluation evidence', '引用纳入应用提供的评估证据'), 20, MUTED)
    c.text(535, 452, p('+ optional cache prefix', '+ 可选的缓存前缀'), 20, MUTED)
    c.text(42, 548, p('Artifact-set lineage is bound separately at operation genesis.',
                       'ArtifactSet 的版本沿革在 operation genesis 单独绑定。'), 21, ACCENTS[2])
    c.text(42, 592, p('Binding integrity and coverage are checked; answer quality depends on your evaluation.',
                       '验证范围包括绑定完整性与证据覆盖，答案质量仍由你的评估标准判断。'), 18, MUTED)
    c.save('evaluation'+('-zh' if zh else ''))
    return c


def evolution(zh):
    p = lambda en, cn: pick(zh, en, cn)
    c = Canvas(p('A change becomes active at a new operation', '经过检查的改动，在新任务中生效'),
               p('Propose a candidate, supply evaluation evidence, then validate the promotion and activation.',
                 '提出候选改动，提供评估证据，再验证批准决定与激活关系。'), 690)
    cards = [
        (42, 183, '01  Proposal', p('Candidate artifact set', '候选 ArtifactSet')),
        (438, 183, '02  Evaluation', p('Host tests + evidence', '应用执行评估并提供证据')),
        (834, 183, '03  Promotion', p('Explicit decision + gates', '明确的批准决定与准入检查')),
        (834, 410, '04  Activation', p('Validated binding', '通过验证的激活绑定')),
        (438, 410, '05  Next genesis', p('Selected artifact-set digest', '选定的 ArtifactSet 摘要')),
        (42, 410, '06  Execution', p('Run the next operation', '启动下一次任务')),
    ]
    for i, (x,y,title,sub) in enumerate(cards):
        c.box(x,y,324,136,stroke=ACCENTS[min(i//2,3)])
        c.text(x+19,y+24,title,25)
        c.text(x+19,y+78,sub,19,MUTED)
    for coords in [(374,251,428,251),(770,251,824,251),(996,328,996,400),
                   (824,478,770,478),(428,478,374,478)]:
        c.arrow(*coords, ACCENTS[3])
    c.text(42, 588, p('A running operation keeps its artifact set. Activation starts a successor.',
                       '运行中的任务保留原 ArtifactSet，激活从下一次任务边界开始。'), 22, ACCENTS[3])
    c.text(42, 632, p('Evidence supports the decision; evaluation does not grant activation authority.',
                       '评估为决定提供证据；评估本身不授予激活权限。'), 19, MUTED)
    c.save('evolution'+('-zh' if zh else ''))
    return c


for zh in (False, True):
    suffix = '-zh' if zh else ''
    execution(zh).save('execution'+suffix)
    frames = [execution(zh, i).im for i in range(4)]
    frames[0].save(OUT / f'execution{suffix}.gif', save_all=True,
                   append_images=frames[1:], duration=[1800]*4, loop=0, optimize=True)
    architecture(zh)
    evaluation(zh)
    evolution(zh)
print(f'Rendered 8 SVGs and 2 GIFs in {OUT}')
