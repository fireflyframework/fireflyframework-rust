"""Generate the on-brand inline concept diagrams for *Firefly for Rust by Example*.

These are the in-text technical figures (NOT the chapter openers — see
``gen_openers.py`` for those). Each diagram is a self-contained ``<figure class="fig">``
holding one ``<svg>`` plus a ``<figcaption>``, matching the markup the CQRS,
event-sourcing and saga chapters already embed.

Design language (shared with the cover, the openers, and ``theme/tokens.css``):

  * Cards are rounded rects ``rx=9..10``, cream fill ``#fdf6ea`` (or ``#fffaf0``),
    stroke ``#e0cda8`` width ``1.5``. Accent/highlight cards use fill ``#fff6e6``
    stroke ``#e0b96a``.
  * Connectors are rust ``#d4793a`` ``stroke-width=3`` with arrowheads drawn as
    explicit ``<polygon>`` triangles (never ``<marker>``).
  * Titles ``#2a1d10`` / ``#3a2a1c`` in the sans stack; sub-labels ``#7a6450``
    smaller; code/type tokens in the mono stack. Gold accents ``#f6a821`` /
    ``#ffc24a`` used sparingly.

WeasyPrint SVG constraints (violating these breaks PDF rendering):

  * solid fills ONLY — no gradients, no ``<filter>``, no ``<marker>`` in shared
    ``<defs>``;
  * arrowheads are explicit polygons;
  * every ``viewBox`` is set and every SVG is self-contained (no external fonts).

The COMPUTED-LAYOUT primitives (``card``/``chip``/``arrow``/``lane``/``label``)
and the auto-positioning helpers (``flow_row``/``stack``/``grid``/``lanes``) keep
the whole set looking like one family: nothing overlaps because positions are
computed from a few constants, not hand-tuned per figure.

Run:  python build/gen_diagrams.py     (writes art/diagrams/*.svg, bare SVG)
The book EMBEDS the inline ``figure_*()`` strings; the bare files are for preview.
"""
from __future__ import annotations

import math
from pathlib import Path
from xml.sax.saxutils import escape as _xml_escape

ART = Path(__file__).resolve().parents[1] / "art" / "diagrams"

# --- palette (mirrors tokens.css + the existing inline figures) --------------
FIELD    = "#fdf6ea"   # cream card fill
FIELD2   = "#fffaf0"   # lighter cream card fill
ACCENT   = "#fff6e6"   # highlight/accent card fill
ACCENT_S = "#e0b96a"   # highlight/accent card stroke
CARD_S   = "#e0cda8"   # default card stroke
RUST     = "#d4793a"   # connectors, accent stroke
RUST_D   = "#b5531f"   # deeper rust (arrowheads/emphasis)
AMBER    = "#f6a821"
AMBER_B  = "#ffc24a"
GREEN    = "#1f8a4c"   # query / success accent (matches admin "green" badges)
GREEN_BG = "#ecf9f0"
BLUE     = "#2563c9"   # command / note accent (matches admin "blue" badges)
BLUE_BG  = "#eef4ff"
RED      = "#b03a2e"   # failure / compensation (matches existing saga figs)
RED_BG   = "#fdecea"
TITLE    = "#2a1d10"   # card titles
TITLE2   = "#3a2a1c"   # secondary titles
SUB      = "#7a6450"   # sub-labels
LANE_BG  = "#f7ecd8"   # swimlane background
LANE_S   = "#e6d4b0"   # swimlane stroke

FONT = "Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif"
MONO = "SF Mono,JetBrains Mono,Menlo,Consolas,monospace"


def esc(s: str) -> str:
    """XML-escape SVG text content (these SVGs are inlined into XHTML, so a raw
    ``&`` / ``<`` / ``>`` would make the chapter document malformed)."""
    return _xml_escape(str(s))


# ===========================================================================
# COMPUTED-LAYOUT PRIMITIVES
# ===========================================================================
def label(x, y, text, *, size=11.5, fill=SUB, weight="600", anchor="middle",
          mono=False, italic=False):
    """A free text label."""
    fam = MONO if mono else FONT
    style = ' font-style="italic"' if italic else ""
    return (f'<text x="{x:.1f}" y="{y:.1f}" text-anchor="{anchor}" '
            f'font-size="{size}" font-weight="{weight}" fill="{fill}" '
            f'font-family="{fam}"{style}>{esc(text)}</text>')


def card(x, y, w, h, title, *, sub=None, mono=False, accent=False,
         fill=None, stroke=None, tcol=None, ts=13.0, ss=10.0):
    """A rounded-rect card with a title and an optional sub-label.

    ``accent`` switches to the warm highlight fill/stroke. ``mono`` renders the
    title in the monospace stack (for code/type identifiers). Explicit
    ``fill``/``stroke``/``tcol`` override the defaults (used for command/query
    colour-coding)."""
    f = fill if fill else (ACCENT if accent else FIELD)
    s = stroke if stroke else (ACCENT_S if accent else CARD_S)
    tc = tcol if tcol else TITLE
    fam = MONO if mono else FONT
    ty = (y + h / 2 + 4.5) if sub is None else (y + h / 2 - 3)
    out = [
        # soft drop shadow plate, then the card
        f'<rect x="{x:.1f}" y="{y + 2.5:.1f}" width="{w:.1f}" height="{h:.1f}" '
        f'rx="9" fill="#d9c4a3" opacity="0.22"/>',
        f'<rect x="{x:.1f}" y="{y:.1f}" width="{w:.1f}" height="{h:.1f}" rx="9" '
        f'fill="{f}" stroke="{s}" stroke-width="1.5"/>',
        f'<text x="{x + w / 2:.1f}" y="{ty:.1f}" text-anchor="middle" '
        f'font-size="{ts}" font-weight="700" fill="{tc}" '
        f'font-family="{fam}">{esc(title)}</text>',
    ]
    if sub is not None:
        out.append(
            f'<text x="{x + w / 2:.1f}" y="{y + h / 2 + 11:.1f}" '
            f'text-anchor="middle" font-size="{ss}" fill="{SUB}" '
            f'font-family="{FONT}">{esc(sub)}</text>')
    return "".join(out)


def chip(x, y, text, *, fill=AMBER, tcol="#16110c", mono=False, h=26.0):
    """A small pill/chip; width auto-sizes to the text."""
    fam = MONO if mono else FONT
    cw = 18 + len(str(text)) * (7.2 if mono else 7.0)
    return (
        f'<g transform="translate({x:.1f},{y:.1f})">'
        f'<rect x="0" y="0" width="{cw:.1f}" height="{h:.1f}" rx="{h / 2:.1f}" '
        f'fill="{fill}" opacity="0.95"/>'
        f'<text x="{cw / 2:.1f}" y="{h / 2 + 4.2:.1f}" text-anchor="middle" '
        f'font-size="12" font-weight="700" fill="{tcol}" '
        f'font-family="{fam}">{esc(text)}</text></g>'), cw


def _arrowhead(x2, y2, ang, *, color, hw=4.5, length=8.0):
    bx = x2 - length * math.cos(ang)
    by = y2 - length * math.sin(ang)
    perp = ang + math.pi / 2
    p2x, p2y = bx + hw * math.cos(perp), by + hw * math.sin(perp)
    p3x, p3y = bx - hw * math.cos(perp), by - hw * math.sin(perp)
    return (bx, by,
            f'<polygon points="{x2:.1f},{y2:.1f} {p2x:.1f},{p2y:.1f} '
            f'{p3x:.1f},{p3y:.1f}" fill="{color}"/>')


def arrow(x1, y1, x2, y2, *, label=None, dashed=False, color=RUST,
          head=RUST_D, width=3.0, label_dy=-7, curve=None):
    """A connector with an explicit triangular arrowhead.

    ``curve`` (a perpendicular offset in px) bends the line into a quadratic
    arc — used for compensation/feedback edges. ``label`` is centred on the
    line. Never emits a ``<marker>`` (which breaks WeasyPrint)."""
    dash = ' stroke-dasharray="6 5"' if dashed else ""
    ang = math.atan2(y2 - y1, x2 - x1)
    bx, by, headsvg = _arrowhead(x2, y2, ang, color=head)
    if curve:
        mx, my = (x1 + x2) / 2, (y1 + y2) / 2
        cx, cy = mx - curve * math.sin(ang), my + curve * math.cos(ang)
        # recompute the arrowhead tangent from the control point
        ang2 = math.atan2(y2 - cy, x2 - cx)
        bx, by, headsvg = _arrowhead(x2, y2, ang2, color=head)
        line = (f'<path d="M{x1:.1f},{y1:.1f} Q{cx:.1f},{cy:.1f} {bx:.1f},{by:.1f}" '
                f'fill="none" stroke="{color}" stroke-width="{width}"'
                f'{dash} stroke-linecap="round"/>')
        lx, ly = cx, cy + label_dy
    else:
        line = (f'<line x1="{x1:.1f}" y1="{y1:.1f}" x2="{bx:.1f}" y2="{by:.1f}" '
                f'stroke="{color}" stroke-width="{width}"{dash} '
                f'stroke-linecap="round"/>')
        lx, ly = (x1 + x2) / 2, (y1 + y2) / 2 + label_dy
    out = line + headsvg
    if label:
        out += (f'<text x="{lx:.1f}" y="{ly:.1f}" text-anchor="middle" '
                f'font-size="10.5" font-weight="600" fill="{color}" '
                f'font-family="{FONT}">{esc(label)}</text>')
    return out


def lane(x, y, w, h, label_text, *, fill=LANE_BG, stroke=LANE_S, tcol=SUB):
    """A labelled swimlane (background band) for grouping cards."""
    return (
        f'<rect x="{x:.1f}" y="{y:.1f}" width="{w:.1f}" height="{h:.1f}" rx="11" '
        f'fill="{fill}" stroke="{stroke}" stroke-width="1.2"/>'
        f'<text x="{x + 14:.1f}" y="{y + 19:.1f}" font-size="11" '
        f'font-weight="700" fill="{tcol}" font-family="{FONT}" '
        f'letter-spacing="0.5">{esc(label_text)}</text>')


# --- auto-positioning helpers ----------------------------------------------
def flow_row(items, y, *, x0=24, w=150, h=46, gap=34, arrow_label=None,
             arrows=True, **card_kw):
    """Lay ``items`` out as a horizontal row of cards with arrows between them.

    Each item is ``(title, sub)`` or ``(title, sub, overrides_dict)``. Returns
    ``(svg, centres)`` where ``centres`` are the per-card centre points so the
    caller can attach further connectors. Cards never overlap: x is computed."""
    svg, centres, x = [], [], x0
    n = len(items)
    for i, it in enumerate(items):
        title, sub = it[0], it[1]
        over = it[2] if len(it) > 2 else {}
        kw = dict(card_kw); kw.update(over)
        svg.append(card(x, y, w, h, title, sub=sub, **kw))
        centres.append((x + w / 2, y + h / 2))
        if arrows and i < n - 1:
            svg.append(arrow(x + w, y + h / 2, x + w + gap, y + h / 2,
                             label=arrow_label))
        x += w + gap
    return "".join(svg), centres


def stack(items, x, *, y0=24, w=220, h=46, gap=30, arrows=True, arrow_labels=None,
          **card_kw):
    """Lay ``items`` out as a vertical stack of cards with down-arrows between.

    Items as in :func:`flow_row`. Returns ``(svg, centres)``."""
    svg, centres, y = [], [], y0
    n = len(items)
    for i, it in enumerate(items):
        title, sub = it[0], it[1]
        over = it[2] if len(it) > 2 else {}
        kw = dict(card_kw); kw.update(over)
        svg.append(card(x, y, w, h, title, sub=sub, **kw))
        centres.append((x + w / 2, y + h / 2))
        if arrows and i < n - 1:
            lab = arrow_labels[i] if arrow_labels and i < len(arrow_labels) else None
            svg.append(arrow(x + w / 2, y + h, x + w / 2, y + h + gap, label=lab))
        y += h + gap
    return "".join(svg), centres


def grid(items, *, x0=24, y0=24, w=150, h=46, cols=3, gx=24, gy=24, **card_kw):
    """Lay ``items`` out on a grid. Returns ``(svg, centres)``."""
    svg, centres = [], []
    for idx, it in enumerate(items):
        r, c = divmod(idx, cols)
        x = x0 + c * (w + gx)
        y = y0 + r * (h + gy)
        title, sub = it[0], it[1]
        over = it[2] if len(it) > 2 else {}
        kw = dict(card_kw); kw.update(over)
        svg.append(card(x, y, w, h, title, sub=sub, **kw))
        centres.append((x + w / 2, y + h / 2))
    return "".join(svg), centres


def lanes(named, *, x0=24, y0=44, lane_w=560, row_h=58, gap=14, label_w=0,
          card_w=150, card_h=44, card_gap=22, **card_kw):
    """Draw one swimlane per ``named`` entry (an ordered dict ``{name: [items]}``).

    Cards in each lane flow left-to-right. Returns ``(svg, lane_centres)`` where
    ``lane_centres[name]`` is the list of card centres in that lane."""
    svg, centres = [], {}
    y = y0
    for name, items in named.items():
        svg.append(lane(x0, y, lane_w, row_h, name))
        cx = x0 + 16 + label_w
        row = []
        for it in items:
            title, sub = it[0], it[1]
            over = it[2] if len(it) > 2 else {}
            kw = dict(card_kw); kw.update(over)
            cy = y + (row_h - card_h) / 2 + 8
            svg.append(card(cx, cy, card_w, card_h, title, sub=sub, **kw))
            row.append((cx + card_w / 2, cy + card_h / 2))
            cx += card_w + card_gap
        centres[name] = row
        y += row_h + gap
    return "".join(svg), centres


# ===========================================================================
# FIGURE ASSEMBLER
# ===========================================================================
def figure(vw, vh, body, caption, aria):
    """Wrap an SVG body in the standard ``<figure class="fig">`` markup the book
    embeds. ``caption`` may contain inline HTML (e.g. ``<code>`` spans)."""
    return (
        '<figure class="fig">\n'
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {vw} {vh}" role="img"\n'
        f'     aria-label="{esc(aria)}"\n'
        f'     font-family="{FONT}">\n'
        f'{body}\n'
        '</svg>\n'
        f'<figcaption>{caption}</figcaption>\n'
        '</figure>\n')


def _bare(fig_html: str) -> str:
    """Strip the ``<figure>``/``<figcaption>`` wrapper, leaving the bare ``<svg>``
    for the preview files written to art/diagrams/."""
    start = fig_html.index("<svg")
    end = fig_html.index("</svg>") + len("</svg>")
    return fig_html[start:end] + "\n"


# ===========================================================================
# THE DIAGRAMS
# ===========================================================================
def fig_dual_port():
    """1. Dual-port topology: public :8080 vs management :8081."""
    vw, vh = 560, 312
    b = []
    # header chips for the two ports
    b.append(card(24, 16, 248, 40, "Public API  :8080", sub="client-facing",
                  accent=True, ts=14))
    b.append(card(288, 16, 248, 40, "Management  :8081", sub="operator-facing",
                  accent=True, ts=14))
    # public column
    pub = [("#[rest_controller]", "your routes"),
           ("Security", "JWT · roles · sessions"),
           ("RFC 9457 404", "problem+json fallback")]
    sp, _ = stack(pub, 24, y0=78, w=248, h=52, gap=18, arrows=False, mono=False)
    b.append(sp)
    # management column
    mgmt = [("/actuator/*", "health · info · metrics"),
            ("/admin", "self-hosted dashboard"),
            ("/swagger-ui · /redoc", "/v3/api-docs")]
    sm, _ = stack(mgmt, 288, y0=78, w=248, h=52, gap=18, arrows=False)
    b.append(sm)
    # bind-address note
    b.append(label(280, 300,
                   "FIREFLY_SERVER_ADDR  ·  FIREFLY_MANAGEMENT_ADDR  override the binds",
                   size=10.5, mono=True, fill=SUB))
    body = "\n".join(b)
    cap = ("Two listeners, one process. The <strong>public API</strong> "
           "(<code>:8080</code>) serves your controllers, security and the "
           "RFC&nbsp;9457 <code>404</code> fallback; the "
           "<strong>management surface</strong> (<code>:8081</code>) serves the "
           "actuator, the self-hosted <code>/admin</code> dashboard and the "
           "OpenAPI docs — so operational endpoints never leak onto the public "
           "network.")
    aria = ("Dual-port topology: the public API on port 8080 serves controllers, "
            "security and the RFC 9457 404 fallback; the management surface on "
            "port 8081 serves the actuator, the admin dashboard and the OpenAPI docs")
    return figure(vw, vh, body, cap, aria)


def fig_request_lifecycle():
    """2. Request lifecycle / middleware chain on the public port."""
    vw, vh = 560, 380
    b = []
    b.append(label(280, 24, "inbound HTTP request", size=12.5, weight="700",
                   fill=TITLE2))
    b.append(arrow(280, 30, 280, 52))
    # outermost-to-innermost layer chain (down the page)
    chain = [
        ("ProblemLayer", "errors → problem+json"),
        ("TraceContextLayer", "W3C traceparent in / out"),
        ("CorrelationLayer", "ensure-or-generate id"),
        ("ContentNegotiationLayer", "Accept → JSON / XML"),
    ]
    h, gap, y0 = 48, 16, 56
    sc, centres = stack(chain, 150, y0=y0, w=260, h=h, gap=gap,
                        arrow_labels=["", "", ""])
    b.append(sc)
    # the controller at the bottom
    stack_bottom = y0 + 4 * h + 3 * gap
    b.append(arrow(280, stack_bottom, 280, stack_bottom + 22))
    cy = stack_bottom + 22
    b.append(card(180, cy, 200, 46, "#[rest_controller]", sub="your handler runs",
                  accent=True, mono=True, ts=13))
    # side note: outermost wraps the first layer, innermost the last
    b.append(label(540, y0 + h / 2 + 4, "outermost", size=10, anchor="end",
                   fill=SUB, italic=True))
    b.append(label(540, y0 + 3 * (h + gap) + h / 2 + 4, "innermost", size=10,
                   anchor="end", fill=SUB, italic=True))
    body = "\n".join(b)
    cap = ("The default layer stack, outermost first (some optional layers — "
           "CORS, security headers, metrics — are elided). "
           "<code>ProblemLayer</code> wraps everything so any error "
           "unwinds to an RFC&nbsp;9457 <code>application/problem+json</code> "
           "response; trace-context and correlation open before your handler "
           "runs; content negotiation sits closest to the routes.")
    aria = ("Request lifecycle: an inbound HTTP request passes the Problem, "
            "TraceContext, Correlation and ContentNegotiation layers, outermost "
            "first, before reaching the rest_controller handler, and errors unwind "
            "to an RFC 9457 problem+json response")
    return figure(vw, vh, body, cap, aria)


def fig_di_graph():
    """3. DI bean graph: container + stereotype beans + autowired edges."""
    vw, vh = 560, 300
    b = []
    # the container plate behind everything
    b.append(f'<rect x="16" y="14" width="528" height="272" rx="14" '
             f'fill="#fbf3e3" stroke="{LANE_S}" stroke-width="1.3"/>')
    b.append(label(36, 36, "Container  ·  scan() wires beans in dependency order",
                   size=12, weight="700", fill=TITLE2, anchor="start"))
    # controller at top, two collaborators below, two ports at the bottom
    b.append(card(196, 54, 168, 48, "WalletApi", sub="#[derive(Controller)]",
                  accent=True, ts=14))
    led = card(70, 150, 168, 48, "Ledger", sub="#[derive(Service)]")
    rm = card(322, 150, 168, 48, "ReadModel", sub="#[derive(Component)]")
    b.append(led); b.append(rm)
    store = card(70, 234, 168, 44, "EventStore", sub="#[derive(Repository)]")
    broker = card(322, 234, 168, 44, "Broker", sub="port — #[autowired]")
    b.append(store); b.append(broker)
    # autowired edges (controller -> collaborators -> ports)
    b.append(arrow(248, 102, 170, 150, label="autowired", label_dy=-2))
    b.append(arrow(312, 102, 390, 150, label="autowired", label_dy=-2))
    b.append(arrow(154, 198, 154, 234))
    b.append(arrow(406, 198, 406, 234))
    body = "\n".join(b)
    cap = ("The container scans the stereotype beans and wires them in "
           "dependency order. <code>WalletApi</code> autowires the "
           "<code>Ledger</code> and <code>ReadModel</code>; the "
           "<code>Ledger</code> autowires the <code>EventStore</code> and "
           "<code>Broker</code> ports — no composition root by hand.")
    aria = ("Dependency-injection bean graph: a Container scans stereotype beans "
            "and autowires WalletApi to the Ledger and ReadModel, which in turn "
            "autowire the EventStore and Broker ports, in dependency order")
    return figure(vw, vh, body, cap, aria)


def fig_cqrs_dispatch():
    """4. CQRS dispatch (redesign of the 09-cqrs middleware figure)."""
    vw, vh = 560, 300
    b = []
    b.append(label(280, 24, "send / query a message", size=12.5, weight="700",
                   fill=TITLE2))
    b.append(arrow(280, 30, 280, 54))
    b.append(card(180, 56, 200, 46, "msg ↦ TypeId", sub="matched to a handler",
                  mono=True, ts=13))
    b.append(label(280, 120, "middleware chain", size=11.5, weight="700", fill=SUB))
    b.append(arrow(280, 102, 280, 130))
    # three middleware boxes in a row, validation outermost
    mids = [("V", "Validation"), ("C", "Correlation"), ("Q", "QueryCache")]
    x = 96
    cxs = []
    for i, (g, _name) in enumerate(mids):
        b.append(card(x, 140, 60, 52, g, mono=True, ts=18))
        cxs.append(x + 30)
        if i < 2:
            b.append(arrow(x + 60, 166, x + 60 + 28, 166))
        x += 88
    b.append(label(280, 212,
                   "V = Validation   ·   C = Correlation   ·   Q = QueryCache",
                   size=10.5, fill=SUB))
    b.append(arrow(280, 222, 280, 248))
    b.append(card(190, 250, 180, 44, "your handler", sub="Command or Query",
                  accent=True, ts=13))
    body = "\n".join(b)
    cap = ("A message is matched to its handler by <code>TypeId</code>, then runs "
           "the registered middleware chain — <code>Validation</code> "
           "outermost, then <code>Correlation</code>, then "
           "<code>QueryCache</code> — before the handler executes. The "
           "correlation scope opens before the cache layer, so everything it "
           "logs carries the id.")
    aria = ("CQRS dispatch: a message is matched to a handler by TypeId, passes "
            "the Validation, Correlation and QueryCache middleware chain with "
            "validation outermost, then reaches your command or query handler")
    return figure(vw, vh, body, cap, aria)


def fig_reactive():
    """5. Reactive Mono (0..1) vs Flux (0..N) streams."""
    vw, vh = 560, 250
    b = []
    # Mono lane
    b.append(label(36, 44, "Mono<T>", size=15, weight="800", fill=RUST_D,
                   anchor="start", mono=True))
    b.append(label(36, 62, "0 or 1 item, then complete", size=11,
                   anchor="start", fill=SUB))
    b.append(f'<line x1="150" y1="58" x2="500" y2="58" stroke="{SUB}" '
             f'stroke-width="2"/>')
    b.append(f'<polygon points="510,58 500,53 500,63" fill="{SUB}"/>')
    b.append(f'<circle cx="250" cy="58" r="13" fill="{AMBER}" '
             f'stroke="{RUST}" stroke-width="1.5"/>')
    b.append(f'<line x1="430" y1="50" x2="430" y2="66" stroke="{GREEN}" '
             f'stroke-width="3"/>')  # completion bar
    b.append(label(250, 86, "just(v)", size=10, mono=True, fill=SUB))
    b.append(label(430, 86, "complete", size=10, fill=GREEN))
    # divider
    b.append(f'<line x1="24" y1="120" x2="536" y2="120" stroke="{CARD_S}" '
             f'stroke-width="1" stroke-dasharray="4 4"/>')
    # Flux lane
    b.append(label(36, 162, "Flux<T>", size=15, weight="800", fill=RUST_D,
                   anchor="start", mono=True))
    b.append(label(36, 180, "0..N items, then complete", size=11,
                   anchor="start", fill=SUB))
    b.append(f'<line x1="150" y1="176" x2="500" y2="176" stroke="{SUB}" '
             f'stroke-width="2"/>')
    b.append(f'<polygon points="510,176 500,171 500,181" fill="{SUB}"/>')
    for i in range(5):
        cx = 200 + i * 56
        b.append(f'<circle cx="{cx}" cy="176" r="{11 - i}" '
                 f'fill="{AMBER if i % 2 == 0 else RUST}" '
                 f'stroke="{RUST_D}" stroke-width="1.2"/>')
    b.append(f'<line x1="492" y1="168" x2="492" y2="184" stroke="{GREEN}" '
             f'stroke-width="3"/>')
    b.append(label(228, 206, "map · filter · flat_map", size=10, mono=True,
                   fill=SUB))
    body = "\n".join(b)
    cap = ("The two reactive return types. A <code>Mono&lt;T&gt;</code> emits at "
           "most one item (<code>Ok(Some)</code>), or none "
           "(<code>Ok(None)</code>), then completes; a "
           "<code>Flux&lt;T&gt;</code> emits a stream of zero-or-more items. "
           "Both short-circuit on a terminal <code>Err(FireflyError)</code>.")
    aria = ("Reactive streams: a Mono of T emits at most one item then completes; "
            "a Flux of T emits zero or more items then completes; both can "
            "short-circuit on a terminal error")
    return figure(vw, vh, body, cap, aria)


def fig_event_sourcing():
    """6. Event sourcing: command -> append events -> state; replay to rebuild."""
    vw, vh = 560, 322
    b = []
    # left: the write path (command -> raise -> append)
    b.append(label(150, 24, "write path", size=11.5, weight="700", fill=SUB))
    write = [
        ("Command", "Deposit { amount }"),
        ("raise(event)", "→ uncommitted []"),
        ("append(events)", "optimistic concurrency"),
    ]
    sw, _ = stack(write, 50, y0=36, w=200, h=52, gap=22, mono=False)
    b.append(sw)
    # the durable event stream (a row of small event cards)
    b.append(label(420, 24, "event stream (append-only)", size=11.5,
                   weight="700", fill=SUB))
    evs = ["+100", "+50", "−30"]
    for i, e in enumerate(evs):
        y = 44 + i * 70
        b.append(card(330, y, 180, 50, e, sub=["WalletOpened", "MoneyDeposited",
                      "MoneyWithdrawn"][i], mono=True, ts=14,
                      fill=ACCENT, stroke=ACCENT_S))
        if i < 2:
            b.append(arrow(420, y + 50, 420, y + 70))
    # append edge from write column to the stream
    b.append(arrow(250, 198, 330, 110, label="append", label_dy=-4))
    # replay/fold back into state
    b.append(arrow(330, 244, 250, 286, label="fold / replay", label_dy=14,
                   color=GREEN, head=GREEN))
    b.append(card(50, 264, 200, 46, "current state", sub="balance = 120",
                  accent=True, ts=14))
    body = "\n".join(b)
    cap = ("Three moves. A command <code>raise</code>s an event onto the "
           "aggregate; <code>EventStore::append</code> persists the uncommitted "
           "events under optimistic concurrency; a later load "
           "<code>fold</code>s the whole append-only stream back into the "
           "current state — the events are the source of truth, the state "
           "is derived.")
    aria = ("Event sourcing: a command raises an event onto the aggregate, "
            "EventStore append persists the events to an append-only stream under "
            "optimistic concurrency, and a later load folds the stream back into "
            "the current state")
    return figure(vw, vh, body, cap, aria)


def fig_saga_compensation():
    """7. Saga forward steps + reverse-order compensation."""
    vw, vh = 560, 220
    b = []
    b.append(label(280, 24, "forward: dependency-ordered steps", size=12,
                   weight="700", fill=TITLE2))
    steps = [("debit", "withdraw(amount)"), ("credit", "deposit(amount)"),
             ("notify", "publish event")]
    sf, centres = flow_row(steps, 48, x0=40, w=150, h=52, gap=34)
    b.append(sf)
    # "fails" marker over the credit step
    b.append(label(centres[1][0], 44, "may fail", size=10.5, weight="700",
                   fill=RED))
    # compensation edge: drop from credit, run left well under the row, rise
    # into debit — routed so it never crosses the cards.
    cx0, cx1 = centres[0][0], centres[1][0]
    row_bottom = 48 + 52
    y_lo = 150
    comp = (f'<path d="M{cx1:.1f},{row_bottom} V{y_lo} H{cx0:.1f} V{row_bottom + 8}" '
            f'fill="none" stroke="{RED}" stroke-width="2.6" '
            f'stroke-dasharray="6 5" stroke-linecap="round"/>')
    b.append(comp)
    # arrowhead pointing up into the debit card
    b.append(f'<polygon points="{cx0:.1f},{row_bottom} {cx0 - 4.5:.1f},'
             f'{row_bottom + 9:.1f} {cx0 + 4.5:.1f},{row_bottom + 9:.1f}" '
             f'fill="{RED}"/>')
    b.append(label((cx0 + cx1) / 2, y_lo - 7,
                   "compensate — reverse order", size=11, weight="700",
                   fill=RED))
    b.append(label(280, 200,
                   "a compensation is a forward undo, not a database rollback",
                   size=11, fill=SUB))
    body = "\n".join(b)
    cap = ("A saga runs its steps in dependency order. If a step fails, the "
           "engine runs the already-completed steps' compensations in "
           "<strong>reverse order</strong> — here a failed "
           "<code>credit</code> refunds the <code>debit</code>. A compensation "
           "is a forward action that undoes, not a database rollback.")
    aria = ("Saga with compensation: forward steps debit, credit and notify run "
            "in dependency order; if credit fails, the engine runs the debit's "
            "compensation in reverse order to refund")
    return figure(vw, vh, body, cap, aria)


def fig_workflow_dag():
    """7c. Workflow DAG: two parallel checks both feed an approve gate."""
    vw, vh = 560, 220
    b = []
    b.append(label(170, 26, "parallel layer", size=11, weight="700", fill=SUB))
    # two independent checks in one layer
    b.append(card(40, 40, 188, 52, "balance-check", sub="funds_ok: bool"))
    b.append(card(40, 128, 188, 52, "limit-check", sub="within_limit: bool"))
    # approve gate (accent) depending on both
    b.append(card(360, 84, 188, 52, "approve", sub="depends_on both",
                  accent=True, ts=14))
    # curved edges from each check into the gate
    b.append(arrow(228, 66, 360, 102, curve=22))
    b.append(arrow(228, 154, 360, 118, curve=-22))
    body = "\n".join(b)
    cap = ("A workflow is a DAG of steps. <code>balance-check</code> and "
           "<code>limit-check</code> have no dependency on each other, so they "
           "run in the same parallel layer; <code>approve</code> waits for both "
           "and consumes their verdicts.")
    aria = ("Workflow DAG: balance-check and limit-check run in parallel in one "
            "layer and both feed the approve gate, which depends on both")
    return figure(vw, vh, body, cap, aria)


def fig_tcc():
    """7b. TCC try / confirm / cancel across participants."""
    vw, vh = 616, 250
    b = []
    cols = [("Try", "reserve", RUST_D), ("Confirm", "on all-tried", GREEN),
            ("Cancel", "on a try failure", RED)]
    cw = 158
    cx = [176, 356, 536]   # column centres, leaving a left margin for row labels
    for (name, sub, col), x in zip(cols, cx):
        b.append(label(x, 28, name, size=14, weight="800", fill=col))
        b.append(label(x, 44, sub, size=10, fill=col))
    rows = [("source", [("withdraw (hold)", RUST, FIELD), ("(none — held)", GREEN, GREEN_BG),
                        ("deposit (release)", RED, RED_BG)]),
            ("dest", [("verify exists", RUST, FIELD), ("deposit (capture)", GREEN, GREEN_BG),
                      ("(none — nothing held)", RED, RED_BG)])]
    for ri, (rname, cells) in enumerate(rows):
        ry = 60 + ri * 74
        b.append(label(20, ry + 28, rname, size=11.5, weight="700", fill="#8a6d3b",
                       anchor="start"))
        for ci, (txt, stroke, fill) in enumerate(cells):
            x = cx[ci] - cw / 2
            b.append(card(x, ry, cw, 46, txt, fill=fill, stroke=stroke,
                          tcol=stroke, ts=11))
    # flow notes under the grid
    b.append(arrow(252, 216, 268, 216, color=GREEN, head=GREEN, width=2.5))
    b.append(label(348, 212, "all tried → confirm", size=10.5, fill=GREEN))
    b.append(label(430, 236, "any try fails → cancel tried in reverse",
                   size=10.5, fill=RED))
    body = "\n".join(b)
    cap = ("Try / Confirm / Cancel. Every participant's <strong>Try</strong> "
           "reserves; once all have tried, <strong>Confirm</strong> captures; if "
           "any Try fails, the engine <strong>Cancels</strong> the already-tried "
           "participants in reverse order. The source holds funds on Try and "
           "releases them on Cancel; the destination captures on Confirm.")
    aria = ("TCC phases for two participants source and dest: a Try column "
            "reserves, a Confirm column captures on success, and a Cancel column "
            "releases on a try failure")
    return figure(vw, vh, body, cap, aria)


def fig_layered_crates():
    """8. Layered crate stack: interfaces <- models <- core <- web, sdk <- interfaces."""
    vw, vh = 560, 320
    b = []
    crates = [
        ("-interfaces", "DTOs · the public contract", "(pure data)"),
        ("-models", "@Entity · @Repository · @Bean", "models"),
        ("-core", "@Service · @Mapper · @Component", "core"),
        ("-web", "@RestController · the binary", "web"),
    ]
    # stack the four inward-depending crates; arrows point INWARD (up the page)
    y0, h, gap, cw = 30, 50, 26, 260
    cleft = 140
    for i, (name, sub, _) in enumerate(crates):
        y = y0 + i * (h + gap)
        accent = (i == 0)  # interfaces is the contract everyone rests on
        b.append(card(cleft, y, cw, h, name, sub=sub, mono=False, accent=accent,
                      ts=14))
        if i < len(crates) - 1:
            # dependency arrow from the lower crate UP to the one it depends on
            yy = y0 + (i + 1) * (h + gap)
            b.append(arrow(cleft + cw / 2, yy, cleft + cw / 2, y + h))
            b.append(label(cleft + cw / 2 + 64, (yy + y + h) / 2 + 4,
                           "depends on", size=9.5, fill=SUB, anchor="start"))
    # sdk on the side, depending only on interfaces
    b.append(card(444, y0 + 2 * (h + gap), 112, 50, "-sdk", sub="typed client",
                  ts=14))
    # dashed edge from sdk up to the interfaces contract
    sdk_cx = 500
    b.append(arrow(sdk_cx, y0 + 2 * (h + gap), cleft + cw, y0 + h / 2,
                   curve=-44, dashed=True, label=None))
    b.append(label(sdk_cx, y0 + 2 * (h + gap) + 70, "→ -interfaces", size=9.5,
                   mono=True, fill=SUB))
    body = "\n".join(b)
    cap = ("Five separately-compiled crates. Dependencies run strictly "
           "<strong>inward</strong>: <code>-web</code> knows "
           "<code>-core</code>, which knows <code>-models</code>, which knows "
           "<code>-interfaces</code> — and the contract crate knows nobody. "
           "<code>-sdk</code> depends only on <code>-interfaces</code>, so a "
           "caller links the DTOs without the persistence or web code.")
    aria = ("Layered crate stack: interfaces, models, core and web crates with "
            "dependencies pointing strictly inward toward the interfaces contract, "
            "and an sdk crate that depends only on interfaces")
    return figure(vw, vh, body, cap, aria)


def fig_four_tier():
    """9. Four-tier architecture (replaces the 01 ASCII tier diagram)."""
    vw, vh = 560, 360
    b = []
    # the front door across the top
    b.append(card(120, 16, 320, 46, "firefly + firefly-macros",
                  sub="one dependency · use firefly::prelude::*;",
                  accent=True, ts=14))
    # four tiers as columns
    tiers = [
        ("Tier 1", "Foundational", ["kernel", "reactive", "web", "config",
                                    "container", "i18n"], RUST),
        ("Tier 2", "Platform", ["cqrs", "eda", "event-sourcing",
                                "orchestration", "cache", "security"], AMBER_B),
        ("Tier 3", "Adapters", ["data-sqlx", "data-mongodb", "eda-kafka",
                                "cache-redis", "idp-*", "notif-*"], RUST),
        ("Tier 4", "Starters", ["starter-core", "starter-web",
                               "starter-domain", "starter-data", "admin",
                               "cli"], AMBER_B),
    ]
    col_w, gx, x0 = 124, 12, 24
    for i, (tnum, tname, items, accent_col) in enumerate(tiers):
        x = x0 + i * (col_w + gx)
        b.append(f'<rect x="{x}" y="82" width="{col_w}" height="206" rx="11" '
                 f'fill="{LANE_BG}" stroke="{LANE_S}" stroke-width="1.2"/>')
        b.append(f'<rect x="{x}" y="82" width="{col_w}" height="34" rx="11" '
                 f'fill="{accent_col}" opacity="0.30"/>')
        b.append(label(x + col_w / 2, 100, tnum, size=10.5, weight="800",
                       fill=RUST_D))
        b.append(label(x + col_w / 2, 132, tname, size=12, weight="700",
                       fill=TITLE2))
        for j, it in enumerate(items):
            b.append(label(x + col_w / 2, 152 + j * 21, it, size=10.5, mono=True,
                           fill=SUB))
        # arrow from the front door down into each tier
        b.append(arrow(x + col_w / 2, 62, x + col_w / 2, 80, width=2.5))
        # left-to-right dependency arrows between tiers
        if i < 3:
            b.append(arrow(x + col_w, 200, x + col_w + gx, 200, width=2.5,
                          color=RUST, head=RUST_D))
    # the reactive base across the bottom
    b.append(card(80, 304, 400, 44, "firefly-reactive",
                  sub="the Mono / Flux core every tier rests on (tokio · axum)",
                  ts=14))
    body = "\n".join(b)
    cap = ("The four tiers. A service depends only on the <code>firefly</code> "
           "facade (the front door). The tiers build left to right — "
           "<strong>Foundational</strong> vocabulary, <strong>Platform</strong> "
           "engines that define ports, <strong>Adapters</strong> that implement "
           "them, <strong>Starters</strong> that compose and ship — each "
           "depending only on the tiers to its left, all resting on the "
           "<code>firefly-reactive</code> core.")
    aria = ("Four-tier architecture: the firefly facade is the front door; below "
            "it Foundational, Platform, Adapters and Starters tiers build left to "
            "right, each depending on the tiers to its left, all resting on the "
            "firefly-reactive Mono/Flux core")
    return figure(vw, vh, body, cap, aria)


def fig_openapi():
    """10. OpenAPI generation: controllers + schemas -> spec -> Swagger/ReDoc."""
    vw, vh = 600, 250
    b = []
    # sources on the left
    b.append(card(24, 40, 200, 50, "#[rest_controller]", sub="routes + status codes",
                  mono=True, ts=13))
    b.append(card(24, 150, 200, 50, "#[derive(Schema)]", sub="DTO component schemas",
                  mono=True, ts=13))
    # the spec in the middle
    b.append(arrow(224, 65, 318, 110, label=None))
    b.append(arrow(224, 175, 318, 130, label=None))
    # spec card with little "lines"
    b.append(f'<rect x="320" y="80" width="120" height="92" rx="9" '
             f'fill="#d9c4a3" opacity="0.22"/>')
    b.append(f'<rect x="320" y="78" width="120" height="92" rx="9" '
             f'fill="{ACCENT}" stroke="{ACCENT_S}" stroke-width="1.5"/>')
    b.append(f'<rect x="338" y="94" width="84" height="9" rx="4.5" fill="{RUST}"/>')
    for r in range(4):
        b.append(f'<rect x="338" y="{114 + r * 13}" width="{84 - r * 12}" '
                 f'height="6" rx="3" fill="{SUB}" opacity="0.6"/>')
    b.append(label(380, 190, "openapi.json", size=11, weight="700", fill=RUST_D,
                   mono=True))
    b.append(label(380, 206, "(/v3/api-docs)", size=10, fill=SUB, mono=True))
    # outputs on the right
    b.append(arrow(440, 110, 484, 92))
    b.append(arrow(440, 140, 484, 158))
    o1, _ = chip(486, 80, "Swagger UI")
    o2, _ = chip(486, 146, "ReDoc")
    b.append(o1); b.append(o2)
    body = "\n".join(b)
    cap = ("No codegen step, no annotation framework. At boot "
           "<code>FireflyApplication</code> harvests the routing attributes and "
           "every <code>#[derive(Schema)]</code> type into one OpenAPI&nbsp;3.1 "
           "document (served at <code>/v3/api-docs</code> on the management "
           "port) and points Swagger&nbsp;UI and ReDoc at it.")
    aria = ("OpenAPI generation: rest_controller routes and derive Schema DTOs are "
            "harvested into one openapi.json spec served at /v3/api-docs, which "
            "Swagger UI and ReDoc render")
    return figure(vw, vh, body, cap, aria)


def fig_config_precedence():
    """11. Configuration precedence: defaults -> YAML -> profile -> env -> CLI."""
    vw, vh = 560, 250
    b = []
    layers = [
        ("defaults", "StaticSource", "beats nothing"),
        ("base YAML", "application.yaml", ""),
        ("profile YAML", "application-prod.yaml", ""),
        ("environment", "FIREFLY_*", ""),
        ("CLI flags", "FlagSource", "beats everything"),
    ]
    n = len(layers)
    cw, gap, x0 = 92, 16, 24
    for i, (name, src, note) in enumerate(layers):
        x = x0 + i * (cw + gap)
        accent = (i == n - 1)
        b.append(card(x, 70, cw, 56, name, sub=src, accent=accent, ts=12.5,
                      ss=9))
        if i < n - 1:
            b.append(arrow(x + cw, 98, x + cw + gap, 98, width=2.6))
    b.append(label(280, 40, "merged left → right  ·  last write wins",
                   size=13, weight="800", fill=TITLE2))
    b.append(label(70, 160, "beats nothing", size=10.5, fill=SUB, italic=True))
    b.append(label(490, 160, "beats everything", size=10.5, fill=RUST_D,
                   weight="700"))
    b.append(label(280, 200,
                   "an env override beats a YAML file; a CLI flag beats both",
                   size=11, fill=SUB))
    body = "\n".join(b)
    cap = ("<code>Layered::new(...)</code> merges its sources left to right and "
           "the <strong>last write wins</strong>. Defaults sit earliest and beat "
           "nothing; a base YAML beats defaults; a profile overlay beats the "
           "base; environment beats YAML files; and a CLI flag beats everything "
           "— one artifact, deployable everywhere.")
    aria = ("Configuration precedence: defaults, base YAML, profile YAML, "
            "environment and CLI flags are merged left to right with the last "
            "write winning, so a CLI flag beats environment, which beats YAML, "
            "which beats defaults")
    return figure(vw, vh, body, cap, aria)


def fig_macros():
    """12. Declarative macro -> generated code mapping."""
    vw, vh = 560, 300
    b = []
    b.append(label(150, 24, "you write", size=11.5, weight="700", fill=SUB))
    b.append(label(430, 24, "the macro generates", size=11.5, weight="700",
                   fill=SUB))
    rows = [
        ("#[derive(Command)]", "the Message impl  (kind, validate, cache_ttl)"),
        ("#[derive(Schema)]", "an OpenAPI schema  (appears in /v3/api-docs)"),
        ("#[derive(DomainEvent)]", "EVENT_TYPE + to_domain_event"),
        ("#[rest_controller]", "a Controller bean + WalletApi::routes(state)"),
        ("#[derive(Service)]", "a scanned bean with #[autowired] fields"),
    ]
    y0, h, gap = 40, 40, 8
    for i, (macro, gen) in enumerate(rows):
        y = y0 + i * (h + gap)
        b.append(card(24, y, 240, h, macro, mono=True, ts=12.5, accent=True))
        b.append(arrow(264, y + h / 2, 304, y + h / 2, width=2.4))
        b.append(card(304, y, 232, h, gen, ts=11, ss=9))
    body = "\n".join(b)
    cap = ("Declarative, compile-time. Each attribute or derive expands to the "
           "wiring you would otherwise hand-write: a <code>Message</code> impl, "
           "an OpenAPI schema, an event discriminator, a controller "
           "<code>routes()</code> builder, or a scanned bean with autowired "
           "fields — generated by <code>firefly-macros</code>, not by a "
           "codegen step you run.")
    aria = ("Declarative macros mapped to generated code: derive Command emits a "
            "Message impl, derive Schema emits an OpenAPI schema, derive "
            "DomainEvent emits EVENT_TYPE and to_domain_event, rest_controller "
            "emits a controller bean and routes builder, derive Service emits a "
            "scanned bean with autowired fields")
    return figure(vw, vh, body, cap, aria)


# ===========================================================================
DIAGRAMS = {
    "dual-port": fig_dual_port,
    "request-lifecycle": fig_request_lifecycle,
    "di-graph": fig_di_graph,
    "cqrs-dispatch": fig_cqrs_dispatch,
    "reactive": fig_reactive,
    "event-sourcing": fig_event_sourcing,
    "saga-compensation": fig_saga_compensation,
    "workflow-dag": fig_workflow_dag,
    "tcc": fig_tcc,
    "layered-crates": fig_layered_crates,
    "four-tier": fig_four_tier,
    "openapi": fig_openapi,
    "config-precedence": fig_config_precedence,
    "macros": fig_macros,
}


def main() -> None:
    ART.mkdir(parents=True, exist_ok=True)
    for name, fn in DIAGRAMS.items():
        (ART / f"{name}.svg").write_text(_bare(fn()), encoding="utf-8")
    print(f"wrote {len(DIAGRAMS)} diagrams to {ART}")


if __name__ == "__main__":
    main()
