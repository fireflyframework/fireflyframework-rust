"""Markdown -> HTML for *Firefly for Rust by Example*.

The src/ chapters are authored in mdBook-flavoured Markdown, so this renderer
is tuned to that dialect rather than python-markdown's admonition syntax:

  * Fenced code (```rust, ```rust,ignore, ```bash, ...) is highlighted with
    Pygments and wrapped in a captioned listing with a language/file tab. The
    mdBook fence suffixes (,ignore / ,no_run / ,should_panic) are stripped so
    the language is recognised.
  * Blockquote callouts in the form

        > **Note** — body ...
        > continued ...

    are lifted into styled callout boxes (note / tip / warning / spring /
    reactor) with an inline SVG icon. A blockquote whose first line is NOT a
    known callout leader stays an ordinary blockquote.
  * mdBook include/playground directives ({{#include}}, {{#playground}}, ...)
    are stripped defensively (none are used today).

Everything else goes through python-markdown ("extra" + "sane_lists").
The output is XHTML so it is valid inside EPUB3.
"""
from __future__ import annotations
import html
import html.entities as _htmlent
import re
from pathlib import Path

import markdown
from markdown.preprocessors import Preprocessor
from markdown.extensions import Extension
from pygments import highlight
from pygments.lexers import get_lexer_by_name, guess_lexer
from pygments.util import ClassNotFound
from pygments.formatters import HtmlFormatter

_FMT = HtmlFormatter(nowrap=True)  # tokens only; we supply <pre class="code">

# fence info-string (first token) -> (pygments lexer name, tab label)
_LANG = {
    "rust": ("rust", "Rust"), "rs": ("rust", "Rust"),
    "bash": ("bash", "Shell"), "sh": ("bash", "Shell"), "shell": ("bash", "Shell"),
    "console": ("console", "Console"),
    "toml": ("toml", "TOML"), "yaml": ("yaml", "YAML"), "yml": ("yaml", "YAML"),
    "json": ("json", "JSON"), "sql": ("sql", "SQL"),
    "java": ("java", "Java"), "kotlin": ("kotlin", "Kotlin"),
    "xml": ("xml", "XML"), "html": ("html", "HTML"),
    "dockerfile": ("dockerfile", "Dockerfile"), "docker": ("dockerfile", "Dockerfile"),
    "text": ("text", "Text"), "txt": ("text", "Text"), "": ("text", "Text"),
}

# callout leader -> (css class, ICON title)
_CALLOUTS = {
    "note": "note", "tip": "tip", "warning": "warning", "warn": "warning",
    "spring parity": "spring", "spring": "spring",
    "reactor parity": "reactor", "reactor": "reactor",
}
_CALLOUT_LABEL = {
    "note": "Note", "tip": "Tip", "warning": "Warning",
    "spring": "Spring parity", "reactor": "Reactor parity",
}

# Professional inline SVG icons injected into callout titles (no emoji).
_ADM_ICON = {
    "note": '<svg class="adm-ico" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" aria-hidden="true">'
            '<circle cx="10" cy="10" r="8.3" fill="none" stroke="#2563c9" stroke-width="1.6"/>'
            '<circle cx="10" cy="6.1" r="1.25" fill="#2563c9"/>'
            '<rect x="9.1" y="8.7" width="1.8" height="5.6" rx="0.9" fill="#2563c9"/></svg>',
    "tip": '<svg class="adm-ico" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" aria-hidden="true">'
           '<path d="M10 2.4a5.6 5.6 0 0 0-3.3 10.1c.45.33.72.8.78 1.32l.09.78h4.86l.09-.78'
           'c.06-.52.33-.99.78-1.32A5.6 5.6 0 0 0 10 2.4z" fill="none" stroke="#1f8a4c" stroke-width="1.5"/>'
           '<path d="M8 17.2h4M8.7 18.7h2.6" stroke="#1f8a4c" stroke-width="1.4" stroke-linecap="round"/></svg>',
    "warning": '<svg class="adm-ico" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" aria-hidden="true">'
               '<path d="M10 3l7.3 12.6H2.7L10 3z" fill="none" stroke="#c2410c" stroke-width="1.6" stroke-linejoin="round"/>'
               '<rect x="9.1" y="8.2" width="1.8" height="4.4" rx="0.9" fill="#c2410c"/>'
               '<circle cx="10" cy="13.9" r="1.05" fill="#c2410c"/></svg>',
    "spring": '<svg class="adm-ico" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" aria-hidden="true">'
              '<path d="M4.5 15.5c0-5.5 4-9.5 11-9.5-.5 5.5-4.5 9.5-11 9.5z" fill="none" '
              'stroke="#43b02a" stroke-width="1.5" stroke-linejoin="round"/>'
              '<path d="M6 14.2c2.6-3.2 5.4-5.2 8.4-6.1" stroke="#43b02a" stroke-width="1.3" '
              'fill="none" stroke-linecap="round"/></svg>',
    "reactor": '<svg class="adm-ico" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" aria-hidden="true">'
               '<circle cx="10" cy="10" r="2.1" fill="#7c3aed"/>'
               '<ellipse cx="10" cy="10" rx="8" ry="3.4" fill="none" stroke="#7c3aed" stroke-width="1.4"/>'
               '<ellipse cx="10" cy="10" rx="8" ry="3.4" fill="none" stroke="#7c3aed" stroke-width="1.4" '
               'transform="rotate(60 10 10)"/>'
               '<ellipse cx="10" cy="10" rx="8" ry="3.4" fill="none" stroke="#7c3aed" stroke-width="1.4" '
               'transform="rotate(120 10 10)"/></svg>',
}

_FENCE_RE = re.compile(r"^```+([^\n`]*)$")
# leading "**Note**" / "**Spring parity**" possibly followed by "—" / "-" / ":"
_LEADER_RE = re.compile(r"^\*\*(?P<name>[A-Za-z][A-Za-z ]*?)\*\*\s*(?P<sep>[—–:-])?\s*(?P<rest>.*)$")
_INCLUDE_RE = re.compile(r"\{\{#(include|playground|rustdoc_include)[^}]*\}\}")


def _strip_fence_suffix(info: str) -> str:
    """'rust,ignore' / 'rust,no_run' -> 'rust'; keep only the first token."""
    return info.strip().split(",")[0].split()[0].lower() if info.strip() else ""


def _render_code(info: str, code: str) -> str:
    lang = _strip_fence_suffix(info)
    lexer_name, label = _LANG.get(lang, ("text", lang.upper() or "Text"))
    try:
        lexer = get_lexer_by_name(lexer_name)
    except ClassNotFound:
        try:
            lexer = guess_lexer(code)
        except ClassNotFound:
            lexer = get_lexer_by_name("text")
    body = highlight(code, lexer, _FMT)
    return (f'<div class="listing"><span class="filetab">{html.escape(label)}</span>'
            f'<pre class="code">{body}</pre></div>')


class _Blocks(Preprocessor):
    """Lift fenced code + blockquote callouts into stashed HTML BEFORE the core
    block parser runs, so they never get mangled by python-markdown."""

    def run(self, lines):
        out: list[str] = []
        i, n = 0, len(lines)
        while i < n:
            line = lines[i]
            if _INCLUDE_RE.search(line):
                i += 1
                continue
            m = _FENCE_RE.match(line)
            if m:
                info = m.group(1)
                body, i = [], i + 1
                while i < n and not _FENCE_RE.match(lines[i].rstrip()):
                    body.append(lines[i]); i += 1
                i += 1  # consume closing fence
                stash = self.md.htmlStash.store(_render_code(info, "\n".join(body)))
                out.extend(["", stash, ""])
                continue
            if line.startswith(">"):
                # gather the whole blockquote
                block, i = [], i
                while i < n and lines[i].startswith(">"):
                    block.append(lines[i][1:].lstrip(" "))
                    i += 1
                html_block = self._blockquote(block)
                out.extend(["", self.md.htmlStash.store(html_block), ""])
                continue
            out.append(line); i += 1
        return out

    def _blockquote(self, block: list[str]) -> str:
        # join, drop leading blanks
        while block and not block[0].strip():
            block.pop(0)
        text = "\n".join(block).strip()
        lead = _LEADER_RE.match(block[0].strip()) if block else None
        if lead:
            name = lead.group("name").strip().lower()
            css = _CALLOUTS.get(name)
            if css:
                # rebuild body: first line minus the leader, then the rest
                first_rest = lead.group("rest").strip()
                tail = "\n".join(block[1:]).strip()
                body_md = (first_rest + ("\n" + tail if tail else "")).strip()
                inner = _inline_md(body_md)
                label = _CALLOUT_LABEL[css]
                icon = _ADM_ICON.get(css, "")
                return (f'<div class="admonition {css}">'
                        f'<p class="admonition-title">{icon}{html.escape(label)}</p>'
                        f'{inner}</div>')
        # plain blockquote
        return f'<blockquote>{_inline_md(text)}</blockquote>'


class _FireflyExtension(Extension):
    def extendMarkdown(self, md):
        # priority above fenced_code/blockquote so we win
        md.preprocessors.register(_Blocks(md), "firefly_blocks", 30)


def _inline_md(text: str) -> str:
    """Render a small Markdown fragment to HTML body (used inside callouts and
    plain blockquotes). Fenced code blocks within the fragment are pulled out
    and highlighted with our own listing renderer (python-markdown's
    fenced_code is unreliable for fragments), and the prose between them is
    rendered with the standard extensions."""
    lines = text.split("\n")
    chunks: list[str] = []
    prose: list[str] = []
    i, n = 0, len(lines)

    def flush_prose():
        body = "\n".join(prose).strip()
        if body:
            sub = markdown.Markdown(extensions=["extra", "sane_lists"],
                                    output_format="xhtml")
            chunks.append(sub.convert(body))
        prose.clear()

    while i < n:
        m = _FENCE_RE.match(lines[i].rstrip())
        if m:
            flush_prose()
            info = m.group(1)
            body, i = [], i + 1
            while i < n and not _FENCE_RE.match(lines[i].rstrip()):
                body.append(lines[i]); i += 1
            i += 1  # consume closing fence
            chunks.append(_render_code(info, "\n".join(body)))
            continue
        prose.append(lines[i]); i += 1
    flush_prose()
    return "".join(chunks)


_XML_SAFE_ENTITIES = {"amp", "lt", "gt", "quot", "apos"}


def _to_xml_entities(s: str) -> str:
    """Convert HTML named entities (e.g. &nbsp;) to numeric refs so the output is
    well-formed XML for EPUB3 XHTML (XML predefines only amp/lt/gt/quot/apos)."""
    def repl(m: "re.Match[str]") -> str:
        name = m.group(1)
        if name in _XML_SAFE_ENTITIES:
            return m.group(0)
        cp = _htmlent.name2codepoint.get(name)
        return f"&#{cp};" if cp is not None else m.group(0)
    return re.sub(r"&([a-zA-Z][a-zA-Z0-9]*);", repl, s)


def render_markdown(text: str, base: Path) -> str:
    md = markdown.Markdown(
        extensions=["extra", "sane_lists", _FireflyExtension()],
        output_format="xhtml",
    )
    return _to_xml_entities(md.convert(text))
