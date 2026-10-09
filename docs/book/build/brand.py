"""Embed canonical rsfly artwork without redrawing its outlined wordmark."""
from pathlib import Path
import re

ART = Path(__file__).resolve().parents[1] / "art"


def lockup(x, y, width, height, *, theme="light", key="brand"):
    svg = (ART / f"rsfly-{theme}.svg").read_text(encoding="utf-8")
    for identifier in re.findall(r'\bid="([^"]+)"', svg):
        svg = svg.replace(f'id="{identifier}"', f'id="{key}-{identifier}"')
        svg = svg.replace(f'url(#{identifier})', f'url(#{key}-{identifier})')
        svg = svg.replace(f'href="#{identifier}"', f'href="#{key}-{identifier}"')
    svg = re.sub(r'\s(?:width|height)="[^"]+"', "", svg, count=2)
    return svg.replace("<svg ", f'<svg x="{x}" y="{y}" width="{width}" height="{height}" ', 1)


def diagram_footer(svg, *, key="diagram"):
    """Reserve a footer outside the technical drawing's existing coordinates."""
    match = re.search(r'viewBox="0 0 ([\d.]+) ([\d.]+)"', svg)
    if not match:
        raise ValueError("Diagram needs a zero-origin viewBox")
    width, height = map(float, match.groups())
    svg = svg[:match.start()] + f'viewBox="0 0 {width:g} {height + 60:g}"' + svg[match.end():]
    footer = lockup(width - 80, height + 4, 72, 52, key=key)
    return svg.rsplit("</svg>", 1)[0] + footer + "</svg>"
