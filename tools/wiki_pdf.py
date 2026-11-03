#!/usr/bin/env python3
"""Turn the XBTune wiki into one PDF guide (shipped in the Windows zip).

Usage (repo root):
    python3 -I tools/wiki_pdf.py WIKI_DIR OUT.pdf [--label "0.3.1 (bef2765)"]

WIKI_DIR is a clone of https://github.com/kz6fittycent/XBTune.wiki.git.
Pages come in the order of _Sidebar.md (its bold lines become the groups
of the contents page); any page not in the sidebar goes at the end.
[[Label|Page]] links and links to the wiki's own URLs jump inside the PDF;
screenshots are read from WIKI_DIR/images.

Needs Python 3.10+ with markdown 3.9 and weasyprint 70.0
(`pip install markdown==3.9 weasyprint==70.0`; WeasyPrint needs Pango).
Build tools only: nothing of theirs ends up in XBTune itself.
"""
import argparse
import datetime
import html
import pathlib
import re
import sys

import markdown
import weasyprint
import weasyprint.text.fonts

REPO = pathlib.Path(__file__).resolve().parent.parent
FONTS = REPO / "crates/xbtune-app/assets/fonts"
WIKI_URL = "https://github.com/kz6fittycent/XBTune/wiki"
IMG_URL = "https://raw.githubusercontent.com/wiki/kz6fittycent/XBTune/images/"
LINK = re.compile(r"\[\[([^\]|]+)(?:\|([^\]]+))?\]\]")


def anchor(page: str) -> str:
    return "page-" + re.sub(r"[^A-Za-z0-9-]", "-", page)


def sidebar(wiki: pathlib.Path):
    """[(group or None, [(label, page)])] in sidebar order."""
    groups, current = [], (None, [])
    for line in (wiki / "_Sidebar.md").read_text().splitlines():
        line = line.strip()
        bold = re.fullmatch(r"\*\*([^*\[]+)\*\*", line)
        if bold:
            if current[1] or current[0]:
                groups.append(current)
            current = (bold.group(1), [])
            continue
        for m in LINK.finditer(line):
            label, page = m.group(1), m.group(2) or m.group(1)
            current[1].append((label.strip(), page.strip().replace(" ", "-")))
    groups.append(current)
    return groups


def convert(text: str, pages: set) -> str:
    """Wiki markdown -> HTML with in-PDF links and local images."""
    def wiki_link(m):
        label, page = m.group(1), (m.group(2) or m.group(1)).strip().replace(" ", "-")
        return f"[{label.strip()}](#{anchor(page)})" if page in pages else label

    text = LINK.sub(wiki_link, text)
    text = re.sub(
        re.escape(WIKI_URL) + r"/([A-Za-z0-9-]+)",
        lambda m: f"#{anchor(m.group(1))}" if m.group(1) in pages else m.group(0),
        text,
    )
    text = text.replace(IMG_URL, "images/")
    return markdown.markdown(text, extensions=["tables", "fenced_code", "sane_lists"])


CSS = """
@font-face { font-family: Inter; src: url("{fonts}/Inter-Regular.ttf"); }
@font-face { font-family: Inter; font-weight: 600 700; src: url("{fonts}/Inter-SemiBold.ttf"); }
@page {
  size: A4; margin: 18mm 16mm 20mm 16mm;
  @bottom-left { content: "XBTune guide {label}"; font: 8pt Inter, sans-serif; color: #777; }
  @bottom-right { content: counter(page); font: 8pt Inter, sans-serif; color: #777; }
}
@page :first { @bottom-left { content: none; } @bottom-right { content: none; } }
html { font: 10pt/1.45 Inter, "DejaVu Sans", sans-serif; color: #1d1f23; }
section.page { break-before: page; }
h1 { font-size: 20pt; margin: 0 0 8pt; color: #d95600; }
h2 { font-size: 14pt; margin: 16pt 0 6pt; break-after: avoid; }
h3 { font-size: 11.5pt; margin: 12pt 0 4pt; break-after: avoid; }
p, li { orphans: 3; widows: 3; }
a { color: #b84800; text-decoration: none; }
img { max-width: 100%; display: block; margin: 6pt auto; border: 0.5pt solid #ccc; break-inside: avoid; }
code, pre { font-family: "DejaVu Sans Mono", monospace; font-size: 8.5pt; }
code { background: #f1f2f4; padding: 0 2pt; border-radius: 2pt; }
pre { background: #f1f2f4; padding: 6pt 8pt; border-radius: 4pt; white-space: pre-wrap; break-inside: avoid; }
pre code { background: none; padding: 0; }
blockquote { margin: 8pt 0; padding: 4pt 10pt; border-left: 3pt solid #d95600; background: #fff4ec; }
table { border-collapse: collapse; margin: 6pt 0; font-size: 9pt; width: 100%; }
th, td { border: 0.5pt solid #ccc; padding: 3pt 5pt; text-align: left; vertical-align: top; }
th { background: #f1f2f4; }
.cover { text-align: center; padding-top: 60mm; }
.cover h1 { font-size: 34pt; bookmark-level: none; }
.cover p { font-size: 12pt; color: #555; margin: 4pt 0; }
.cover .warn { margin: 30mm 15mm 0; font-size: 10pt; color: #8a3a00; }
.toc h2 { bookmark-level: none; }
.toc h3 { margin-top: 10pt; color: #555; bookmark-level: none; }
.toc ul { list-style: none; padding: 0; margin: 0; }
.toc li a { color: #1d1f23; }
.toc li a::after { content: leader(".") target-counter(attr(href), page); }
"""


def build(wiki: pathlib.Path, out: pathlib.Path, label: str) -> int:
    groups = sidebar(wiki)
    order = [p for _, items in groups for _, p in items]
    extra = sorted(
        p.stem for p in wiki.glob("*.md") if not p.stem.startswith("_") and p.stem not in order
    )
    order += extra
    if extra:
        groups.append(("More", [(p.replace("-", " "), p) for p in extra]))
    missing = [p for p in order if not (wiki / f"{p}.md").exists()]
    if missing:
        sys.exit(f"sidebar names pages that don't exist: {', '.join(missing)}")
    pages = set(order)

    today = datetime.date.today().isoformat()
    parts = [
        '<section class="cover"><h1>XBTune</h1>',
        "<p>Diagnostics, logging and tuning for Buell DDFI ECMs</p>",
        f"<p>The guide for version {html.escape(label)}, from the wiki on {today}</p>",
        f'<p><a href="{WIKI_URL}">{WIKI_URL}</a></p>',
        '<p class="warn"><b>A bad tune can damage your engine.</b> Check every change before you '
        "write it, change a little at a time and keep your original backup. XBTune comes with "
        "no warranty (GPL-3.0).</p></section>",
        '<section class="page toc"><h2>Contents</h2>',
    ]
    for group, items in groups:
        if group:
            parts.append(f"<h3>{html.escape(group)}</h3>")
        parts.append("<ul>")
        parts += [f'<li><a href="#{anchor(p)}">{html.escape(lbl)}</a></li>' for lbl, p in items]
        parts.append("</ul>")
    parts.append("</section>")
    for p in order:
        body = convert((wiki / f"{p}.md").read_text(), pages)
        parts.append(f'<section class="page" id="{anchor(p)}">{body}</section>')

    doc = "<!doctype html><html><head><meta charset='utf-8'><title>XBTune guide</title></head><body>"
    doc += "\n".join(parts) + "</body></html>"
    css = CSS.replace("{fonts}", FONTS.as_uri()).replace("{label}", label.replace('"', "'"))
    # @font-face fonts load only through a FontConfiguration.
    fonts = weasyprint.text.fonts.FontConfiguration()
    weasyprint.HTML(string=doc, base_url=str(wiki) + "/").write_pdf(
        out, stylesheets=[weasyprint.CSS(string=css, font_config=fonts)], font_config=fonts
    )
    return len(order)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("wiki", type=pathlib.Path)
    ap.add_argument("out", type=pathlib.Path)
    ap.add_argument("--label", default="(development build)")
    a = ap.parse_args()
    n = build(a.wiki, a.out, a.label)
    print(f"{a.out}: {n} pages of the wiki")


if __name__ == "__main__":
    main()
