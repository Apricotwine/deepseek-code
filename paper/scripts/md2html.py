#!/usr/bin/env python3
"""Minimal Markdown -> styled HTML renderer for the paper preview.

Handles the subset used by paper.md: ATX headings, fenced code blocks, tables,
bullet/numbered lists, blockquotes, images, links, bold/italic/inline code.
"""

import html
import os
import re

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "paper.md")
DST = os.path.join(ROOT, "preview.html")


def inline(text: str) -> str:
    text = html.escape(text)
    # images
    text = re.sub(
        r"!\[([^\]]*)\]\(([^)]+)\)",
        lambda m: f'<img src="{m.group(2)}" alt="{m.group(1)}" />',
        text,
    )
    # links
    text = re.sub(
        r"\[([^\]]+)\]\(([^)]+)\)",
        lambda m: f'<a href="{m.group(2)}">{m.group(1)}</a>',
        text,
    )
    # inline code
    text = re.sub(r"`([^`]+)`", r"<code>\1</code>", text)
    # bold
    text = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", text)
    # italic
    text = re.sub(r"(?<!\*)\*([^*]+)\*(?!\*)", r"<em>\1</em>", text)
    return text


def render(md: str) -> str:
    lines = md.splitlines()
    out: list[str] = []
    i = 0
    n = len(lines)
    while i < n:
        line = lines[i]
        # fenced code
        if line.startswith("```"):
            lang = line[3:].strip()
            buf = []
            i += 1
            while i < n and not lines[i].startswith("```"):
                buf.append(lines[i])
                i += 1
            i += 1
            out.append(
                f'<pre class="code"><code class="{html.escape(lang)}">'
                + html.escape("\n".join(buf))
                + "</code></pre>"
            )
            continue
        # headings
        m = re.match(r"^(#{1,6})\s+(.*)$", line)
        if m:
            level = len(m.group(1))
            out.append(f"<h{level}>{inline(m.group(2))}</h{level}>")
            i += 1
            continue
        # horizontal rule
        if re.match(r"^\s*(---|\*\*\*)\s*$", line):
            out.append("<hr />")
            i += 1
            continue
        # table block
        if line.lstrip().startswith("|") and i + 1 < n and re.match(r"^\s*\|[\s:|-]+\|\s*$", lines[i + 1]):
            rows = []
            while i < n and lines[i].lstrip().startswith("|"):
                cells = [c.strip() for c in lines[i].strip().strip("|").split("|")]
                rows.append(cells)
                i += 1
            tbl = ["<table>"]
            for r, cells in enumerate(rows):
                tag = "th" if r == 0 else "td"
                tbl.append("<tr>" + "".join(f"<{tag}>{inline(c)}</{tag}>" for c in cells) + "</tr>")
            tbl.append("</table>")
            out.append("".join(tbl))
            continue
        # lists
        if re.match(r"^\s*[-*+]\s+", line):
            buf = []
            while i < n and re.match(r"^\s*[-*+]\s+", lines[i]):
                item = re.sub(r"^\s*[-*+]\s+", "", lines[i])
                buf.append(f"<li>{inline(item)}</li>")
                i += 1
            out.append("<ul>" + "".join(buf) + "</ul>")
            continue
        if re.match(r"^\s*\d+\.\s+", line):
            buf = []
            while i < n and re.match(r"^\s*\d+\.\s+", lines[i]):
                item = re.sub(r"^\s*\d+\.\s+", "", lines[i])
                buf.append(f"<li>{inline(item)}</li>")
                i += 1
            out.append("<ol>" + "".join(buf) + "</ol>")
            continue
        # blockquote
        if line.startswith(">"):
            buf = []
            while i < n and lines[i].startswith(">"):
                buf.append(lines[i][1:].strip())
                i += 1
            out.append(f"<blockquote>{inline(' '.join(buf))}</blockquote>")
            continue
        # blank
        if not line.strip():
            i += 1
            continue
        # paragraph
        buf = [line]
        i += 1
        while i < n and lines[i].strip() and not lines[i].startswith(("#", "```", ">", "|", "-", "*", "+")):
            if re.match(r"^\s*\d+\.\s+", lines[i]):
                break
            buf.append(lines[i])
            i += 1
        out.append(f"<p>{inline(' '.join(buf))}</p>")
    return "\n".join(out)


def main() -> None:
    md = open(SRC, encoding="utf-8").read()
    body = render(md)
    page = f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>Time Awareness Layer — Paper Preview</title>
<style>
  :root {{
    --ink: #0b2f4f; --blue: #1d5f9e; --cyan: #38b6d8; --sand: #f2ede4; --gray: #6b7c8f;
  }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0; background: #0b1c2e; color: var(--ink);
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", sans-serif;
    line-height: 1.65;
  }}
  .sheet {{
    max-width: 860px; margin: 32px auto; background: #fffdf9; padding: 56px 64px;
    border-radius: 10px; box-shadow: 0 18px 60px rgba(0,0,0,.45);
  }}
  h1 {{ font-size: 26px; color: var(--ink); border-bottom: 3px solid var(--cyan); padding-bottom: 12px; }}
  h2 {{ font-size: 20px; color: var(--blue); margin-top: 42px; border-bottom: 1px solid #dce8f2; padding-bottom: 6px; }}
  h3 {{ font-size: 16px; color: var(--blue); margin-top: 28px; }}
  h4 {{ font-size: 14px; margin-top: 22px; }}
  blockquote {{ border-left: 4px solid var(--cyan); margin: 18px 0; padding: 4px 18px; background: #eef8fb; color: #23465f; }}
  table {{ border-collapse: collapse; width: 100%; margin: 18px 0; font-size: 14px; }}
  th, td {{ border: 1px solid #d5e2ee; padding: 8px 12px; text-align: left; }}
  th {{ background: #eaf3fb; color: var(--blue); }}
  td:nth-child(2), td:nth-child(3), td:nth-child(4) {{ text-align: right; font-variant-numeric: tabular-nums; }}
  code {{ background: #eef3f8; padding: 1px 6px; border-radius: 4px; font-size: 13px; }}
  pre.code {{ background: #10293f; color: #d7e7f5; padding: 14px 18px; border-radius: 8px; overflow-x: auto; }}
  pre.code code {{ background: none; color: inherit; padding: 0; }}
  img {{ max-width: 100%; border-radius: 8px; border: 1px solid #dce8f2; margin: 14px 0; }}
  a {{ color: var(--blue); }}
  ul, ol {{ padding-left: 26px; }}
  li {{ margin: 4px 0; }}
  @media (max-width: 700px) {{ .sheet {{ padding: 28px 20px; margin: 0; }} }}
</style>
</head>
<body>
<div class="sheet">
{body}
</div>
</body>
</html>"""
    open(DST, "w", encoding="utf-8").write(page)
    print("wrote", DST)


if __name__ == "__main__":
    main()
