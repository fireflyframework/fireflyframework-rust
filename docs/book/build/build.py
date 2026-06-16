"""Build *Firefly for Rust by Example* into a designed PDF + EPUB from book.yaml.

Pipeline (ported and adapted from the PyFly book build):
  1. Read book.yaml -> front matter, parts, chapters (each referencing a src/
     markdown file + an opener SVG).
  2. Render each chapter's markdown to HTML (md.py), peel off its leading H1,
     and reassemble it under a styled chapter head (number eyebrow + title)
     preceded by the inline opener SVG.
  3. Assemble cover + front matter + Contents + part dividers + chapters into a
     single HTML document, render it with WeasyPrint + the print theme -> PDF.
  4. Assemble the same items as discrete EPUB3 documents -> EPUB.

Chapters whose src file does not yet exist are skipped (so future chapters can
be listed in book.yaml before they are written).
"""
from __future__ import annotations
import re
import sys
from pathlib import Path
from xml.sax.saxutils import escape

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parent))
import os                               # noqa: E402
import md                              # noqa: E402  (to set md.LANG for localized callout labels)
from md import render_markdown          # noqa: E402
from epub import EpubBuilder, Doc       # noqa: E402
from pdf import render_pdf              # noqa: E402

BOOK = Path(__file__).resolve().parents[1]
THEME = BOOK / "theme"
DIST = BOOK / "dist"

PDF_NAME = "firefly-rust-by-example.pdf"
EPUB_NAME = "firefly-rust-by-example.epub"

# Localizable structural labels; overridden per-language via book.yaml `labels:`.
LABELS = {"chapter": "Chapter", "appendix": "Appendix", "contents": "Contents"}

_H1_RE = re.compile(r'<h1[^>]*>.*?</h1>', re.DOTALL)


def _src_dir(cfg: dict) -> Path:
    return BOOK / cfg.get("src_dir", "src")


def _split_part(part_title: str) -> tuple[str, str]:
    """'Part I — Foundations' -> ('Part I', 'Foundations')."""
    m = re.match(r"\s*(.+?)\s*[—–-]\s*(.+?)\s*$", part_title)
    if m:
        return m.group(1).strip(), m.group(2).strip()
    return "", part_title.strip()


def _inline_svg(path: Path) -> str:
    """Return an SVG's markup with any XML prolog stripped (safe to inline)."""
    svg = path.read_text(encoding="utf-8").strip()
    return re.sub(r'^<\?xml[^>]*\?>\s*', "", svg)


def _front_class(fid: str) -> str:
    return {"title": "frontmatter title-page",
            "copyright": "frontmatter copyright-page",
            "dedication": "frontmatter dedication-page"}.get(fid, "frontmatter")


def _items_from_manifest(cfg: dict) -> list[dict]:
    """Ordered build items, each tagged ``kind``.

    kinds: front | toc | divider | chapter
    """
    src = _src_dir(cfg)
    items: list[dict] = []

    # 1) front matter, collected first
    front_items: list[dict] = []
    for fm in cfg.get("front", []):
        p = src / fm["file"]
        if not p.exists():
            continue
        front_items.append({
            "kind": "front",
            "id": fm["id"],
            "title": fm.get("title", fm["id"].title()),
            "path": str(p),
            "fclass": _front_class(fm["id"]),
            "in_nav": bool(fm.get("nav", True)) and "title" in fm,
        })

    # 2) Contents up front: only the cover-adjacent pages (title, copyright —
    # not in the nav) precede it; the readable sections (preface, conventions,
    # the Rust primer) follow it and are listed *in* it.
    items.extend(f for f in front_items if not f["in_nav"])
    items.append({"kind": "toc", "id": "toc", "title": LABELS["contents"]})
    items.extend(f for f in front_items if f["in_nav"])

    # 3) parts
    for part in cfg.get("parts", []):
        eyebrow, ptitle = _split_part(part["title"])
        chapters = [ch for ch in part["chapters"] if (src / ch["file"]).exists()]
        if not chapters:
            continue
        slug = re.sub(r"[^a-z0-9]+", "-", eyebrow.lower()).strip("-") if eyebrow else ""
        did = slug if slug.startswith("part") else f"part-{slug or len(items)}"
        items.append({"kind": "divider", "id": did, "eyebrow": eyebrow,
                      "ptitle": ptitle, "part": part["title"]})
        for ch in chapters:
            num = ch.get("num")
            display = f'{num}. {ch["title"]}' if num not in (None, "") else ch["title"]
            items.append({
                "kind": "chapter",
                "id": ch["id"],
                "title": display,
                "raw_title": ch["title"],
                "num": num,
                "path": str(src / ch["file"]),
                "opener": ch.get("opener"),
                "part": part["title"],
            })
    return items


def _chapter_head(num, raw_title: str, opener: str | None) -> str:
    """Number eyebrow + title, preceded by the inline opener SVG."""
    if num in (None, ""):
        eyebrow = ""
    elif isinstance(num, str):
        eyebrow = f'<span class="ch-num">{LABELS["appendix"]} {escape(num)}</span>' \
            if len(num) == 1 and num.isalpha() else f'<span class="ch-num">{escape(num)}</span>'
    else:
        eyebrow = f'<span class="ch-num">{LABELS["chapter"]} {num}</span>'
    op_html = ""
    if opener:
        p = BOOK / opener
        if p.exists():
            op_html = f'<div class="ch-opener">{_inline_svg(p)}</div>'
    return (f'{op_html}<div class="ch-head">{eyebrow}'
            f'<h1 class="chtitle">{escape(raw_title)}</h1></div>')


def _chapter_body(it: dict) -> str:
    """Rendered chapter HTML with the styled head substituted for the raw H1."""
    rendered = render_markdown(Path(it["path"]).read_text(encoding="utf-8"), BOOK)
    rendered = _H1_RE.sub("", rendered, count=1).lstrip()
    head = _chapter_head(it.get("num"), it["raw_title"], it.get("opener"))
    return head + rendered


def _toc_html(items: list[dict], *, href_fmt: str) -> str:
    parts: list[str] = []
    # Front matter (Preface, Conventions, the Rust primer) listed first, so the
    # Contents reflects the whole book including the introductory material.
    front = [it for it in items if it["kind"] == "front" and it.get("in_nav")]
    if front:
        parts.append('<div class="toc-part-group">'
                     '<h2 class="toc-part-title">Front Matter</h2>'
                     '<ol class="toc-chapters">')
        for it in front:
            href = href_fmt.format(cid=it["id"])
            parts.append(f'<li><a class="toc-link" href="{escape(href)}">'
                         f'{escape(it["title"])}</a></li>')
        parts.append("</ol></div>")
    open_group = False
    for it in items:
        if it["kind"] == "divider":
            if open_group:
                parts.append("</ol></div>")
            eyebrow = (f'<span class="toc-part-eyebrow">{escape(it["eyebrow"])}</span> '
                       if it["eyebrow"] else "")
            parts.append('<div class="toc-part-group">'
                         f'<h2 class="toc-part-title">{eyebrow}{escape(it["ptitle"])}</h2>'
                         '<ol class="toc-chapters">')
            open_group = True
        elif it["kind"] == "chapter" and open_group:
            href = href_fmt.format(cid=it["id"])
            parts.append(f'<li><a class="toc-link" href="{escape(href)}">'
                         f'{escape(it["title"])}</a></li>')
    if open_group:
        parts.append("</ol></div>")
    return f'<h1 class="chtitle">{escape(LABELS["contents"])}</h1>{"".join(parts)}'


def _divider_html(eyebrow: str, ptitle: str) -> str:
    eb = f'<span class="eyebrow part-eyebrow">{escape(eyebrow)}</span>' if eyebrow else ""
    # A dark medallion holding the glowing firefly — the same motif as the
    # cover and the chapter-opener panels, so the parts read as one family.
    glyph = ('<svg class="part-glyph" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 120">'
             '<defs><linearGradient id="pgsky" x1="0" y1="0" x2="120" y2="120" '
             'gradientUnits="userSpaceOnUse">'
             '<stop offset="0" stop-color="#0e1217"/><stop offset="1" stop-color="#16100b"/>'
             '</linearGradient></defs>'
             '<circle cx="60" cy="60" r="54" fill="url(#pgsky)"/>'
             '<circle cx="60" cy="60" r="54" fill="none" stroke="#f6a821" stroke-width="1.5" opacity="0.4"/>'
             '<circle cx="60" cy="64" r="30" fill="#f6a821" opacity="0.12"/>'
             '<path d="M32,90 C44,72 52,62 58,55" fill="none" stroke="#ffd980" '
             'stroke-width="1.2" opacity="0.4" stroke-linecap="round"/>'
             '<g transform="translate(60,56) rotate(-16)">'
             '<circle cx="0" cy="20" r="15" fill="#f6a821" opacity="0.22"/>'
             '<circle cx="0" cy="20" r="9" fill="#ffc24a" opacity="0.5"/>'
             '<path d="M-2,-4 C-26,-20 -34,-1 -11,5 Z" fill="#ffd980" opacity="0.28"/>'
             '<path d="M2,-4 C26,-20 34,-1 11,5 Z" fill="#ffd980" opacity="0.28"/>'
             '<ellipse cx="0" cy="18" rx="7.5" ry="11" fill="#f6a821"/>'
             '<ellipse cx="0" cy="19" rx="4" ry="6.5" fill="#fff2cf"/>'
             '<ellipse cx="0" cy="1" rx="6" ry="9" fill="#1a130c" stroke="#c97e10" stroke-width="1.3"/>'
             '<ellipse cx="0" cy="-9" rx="3.6" ry="4.4" fill="#1a130c" stroke="#c97e10" stroke-width="1.1"/>'
             '<path d="M-2,-12 C-6,-19 -9,-20 -11,-22" fill="none" stroke="#c97e10" stroke-width="1.2" stroke-linecap="round"/>'
             '<path d="M2,-12 C6,-19 9,-20 11,-22" fill="none" stroke="#c97e10" stroke-width="1.2" stroke-linecap="round"/>'
             '</g>'
             '<circle cx="88" cy="34" r="1.6" fill="#ffd980" opacity="0.7"/>'
             '<circle cx="38" cy="40" r="1.3" fill="#bfe26a" opacity="0.7"/>'
             '</svg>')
    return (f'<div class="part-divider-inner">{glyph}{eb}'
            f'<h1 class="part-title">{escape(ptitle)}</h1></div>')


def main() -> int:
    # The manifest (default book.yaml) can be overridden for a localized build,
    # e.g. BOOK_CONFIG=book-es.yaml for the Spanish edition.
    cfg_name = os.environ.get("BOOK_CONFIG", "book.yaml")
    cfg = yaml.safe_load((BOOK / cfg_name).read_text())
    # Localize structural labels + callout labels for this edition's language.
    md.LANG = cfg.get("language", "en")
    LABELS.update(cfg.get("labels", {}))
    pdf_name = cfg.get("pdf_name", PDF_NAME)
    epub_name = cfg.get("epub_name", EPUB_NAME)
    css_text = [(THEME / "tokens.css").read_text(),
                (THEME / "pygments.css").read_text(),
                (THEME / "book.css").read_text()]
    items = _items_from_manifest(cfg)

    only = sys.argv[1] if len(sys.argv) > 1 else ""
    do_pdf = only in ("", "--pdf")
    do_epub = only in ("", "--epub")

    DIST.mkdir(parents=True, exist_ok=True)
    cover_svg = BOOK / cfg.get("cover_svg", "art/cover.svg")

    # ---- EPUB ----
    if do_epub:
        epub = EpubBuilder(title=cfg["title"], author=cfg["author"],
                           language=cfg["language"], identifier=cfg["identifier"],
                           css=css_text)
        if cover_svg.exists():
            epub.add_file(cover_svg, "art/cover.svg", "cover-img", properties="cover-image")
            epub.add_doc(Doc(id="cover", title="Cover",
                             xhtml_body='<div class="cover-page">'
                                        '<img src="art/cover.svg" alt="Cover"/></div>',
                             in_nav=False, kind="front"))
        for it in items:
            if it["kind"] == "toc":
                body = _toc_html(items, href_fmt="{cid}.xhtml")
                epub.add_doc(Doc(id=it["id"], title=it["title"], xhtml_body=body,
                                 in_nav=True, kind="toc"))
            elif it["kind"] == "divider":
                body = _divider_html(it["eyebrow"], it["ptitle"])
                epub.add_doc(Doc(id=it["id"], title=it["part"], xhtml_body=body,
                                 in_nav=False, kind="divider", part=it["part"]))
            elif it["kind"] == "front":
                body = render_markdown(Path(it["path"]).read_text(encoding="utf-8"), BOOK)
                epub.add_doc(Doc(id=it["id"], title=it["title"], xhtml_body=body,
                                 in_nav=it.get("in_nav", True), kind="front"))
            else:  # chapter
                body = _chapter_body(it)
                epub.add_doc(Doc(id=it["id"], title=it["title"], xhtml_body=body,
                                 in_nav=True, kind="chapter",
                                 part=it.get("part"), num=it.get("num")))
        epub.build(DIST / epub_name)

    # ---- PDF (single concatenated document) ----
    if do_pdf:
        parts_html: list[str] = []
        if cover_svg.exists():
            parts_html.append(f'<div class="cover-page">{_inline_svg(cover_svg)}</div>')
        for it in items:
            if it["kind"] == "toc":
                body = _toc_html(items, href_fmt="#{cid}")
                parts_html.append(f'<section class="toc" id="{it["id"]}">{body}</section>')
            elif it["kind"] == "divider":
                body = _divider_html(it["eyebrow"], it["ptitle"])
                parts_html.append(f'<section class="part-divider" id="{it["id"]}">{body}</section>')
            elif it["kind"] == "front":
                body = render_markdown(Path(it["path"]).read_text(encoding="utf-8"), BOOK)
                parts_html.append(f'<section class="{it["fclass"]}" id="{it["id"]}">{body}</section>')
            else:  # chapter
                body = _chapter_body(it)
                parts_html.append(f'<section class="chapter" id="{it["id"]}">{body}</section>')
        full = ("<!DOCTYPE html><html><head><meta charset='utf-8'></head><body>"
                + "\n".join(parts_html) + "</body></html>")
        render_pdf(full, base_url=BOOK,
                   css_paths=[THEME / "tokens.css", THEME / "pygments.css",
                              THEME / "book.css", THEME / "print.css"],
                   out=DIST / pdf_name)

    nch = sum(1 for it in items if it["kind"] == "chapter")
    nfr = sum(1 for it in items if it["kind"] == "front")
    ndv = sum(1 for it in items if it["kind"] == "divider")
    print(f"Built {nch} chapter(s) + {nfr} front-matter + {ndv} part divider(s) "
          f"+ cover + TOC -> {'PDF ' if do_pdf else ''}{'EPUB' if do_epub else ''} in {DIST}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
