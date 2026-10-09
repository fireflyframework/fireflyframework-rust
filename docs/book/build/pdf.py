"""Render the assembled book HTML to a PDF via WeasyPrint.

WeasyPrint needs Homebrew's gobject/pango/cairo on the dynamic-loader path;
the build wrapper (build-book.sh) exports DYLD_FALLBACK_LIBRARY_PATH before
invoking Python, so the import here succeeds.
"""
from __future__ import annotations
from io import BytesIO
import base64
from pathlib import Path
import re
import cairosvg
from pypdf import PdfReader, PdfWriter
from weasyprint import HTML, CSS


def _brand_fidelity(html: str) -> str:
    """Use canonical PNGs for small lockups affected by WeasyPrint's SVG gradients."""
    stack, replacements = [], []
    for match in re.finditer(r'<(/?)svg\b[^>]*>', html):
        if not match.group(1):
            stack.append((match.start(), match.end()))
        else:
            start, body_start = stack.pop()
            body = html[body_start:match.start()]
            if not body.startswith('<title>rsfly - Rust framework by Firefly</title>'):
                continue
            theme = "dark" if '<g fill="#f3f1eb"' in body[:150] else "light"
            path = Path(__file__).resolve().parents[1] / "art" / f"rsfly-{theme}.png"
            payload = base64.b64encode(path.read_bytes()).decode("ascii")
            viewbox = re.search(r'viewBox="([^"]+)"', html[start:body_start])
            _, _, width, height = viewbox.group(1).split()
            replacement = ('<title>rsfly - Rust framework by Firefly</title>'
                           f'<image width="{width}" height="{height}" '
                           f'href="data:image/png;base64,{payload}"/>')
            replacements.append((body_start, match.start(), replacement))
    for start, end, replacement in reversed(replacements):
        html = html[:start] + replacement + html[end:]
    return html


def render_pdf(full_html: str, base_url: Path, css_paths: list[Path], out: Path,
               cover_pages: dict[str, Path] | None = None) -> Path:
    out = Path(out)
    out.parent.mkdir(parents=True, exist_ok=True)
    HTML(string=_brand_fidelity(full_html), base_url=str(base_url)).write_pdf(
        str(out), stylesheets=[CSS(filename=str(p)) for p in css_paths])
    if cover_pages:
        # Cairo preserves translucent SVG gradients that WeasyPrint flattens.
        # Replace the already reserved cover pages; body pagination stays intact.
        writer = PdfWriter(clone_from=str(out))
        for side, path in cover_pages.items():
            index = 0 if side == "cover" else len(writer.pages) - 1
            original = writer.pages[index]
            vector = cairosvg.svg2pdf(
                url=str(path), output_width=float(original.mediabox.width) * 4 / 3,
                output_height=float(original.mediabox.height) * 4 / 3)
            page = PdfReader(BytesIO(vector)).pages[0]
            writer.remove_page(index)
            writer.insert_page(page, index)
        buffer = BytesIO()
        writer.write(buffer)
        out.write_bytes(buffer.getvalue())
    return out
