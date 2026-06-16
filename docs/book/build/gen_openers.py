"""Generate on-brand chapter-opener SVGs for *Firefly for Rust by Example*.

Each opener is a 720x300 banner sharing one visual language with the cover:
a warm cream diagram field on the left, and a deep night-sky panel on the
right holding a glowing **firefly** emblem (the same bioluminescent motif as
the cover, not a literal gear). The emblem + palette are constant so the set
reads as a family; the left-hand glyph differs per chapter so each opener
previews that chapter's idea.

Run:  python build/gen_openers.py        (writes art/openers/*.svg)
"""
from __future__ import annotations
from pathlib import Path
from xml.sax.saxutils import escape as _xml_escape

ART = Path(__file__).resolve().parents[1] / "art" / "openers"


def esc(s: str) -> str:
    """XML-escape SVG text content (these SVGs are inlined into XHTML, so raw
    & / < / > would make the chapter document malformed)."""
    return _xml_escape(str(s))

# ---- palette ---------------------------------------------------------------
FIELD   = "#fdf6ea"   # warm cream banner field
FIELD2  = "#f7ecd8"   # slightly deeper cream
PANEL1  = "#0e1217"   # night-sky panel, cool top
PANEL2  = "#16100b"   # night-sky panel, warm bottom
AMBER   = "#f6a821"
AMBER_B = "#ffc24a"   # bright amber
AMBER_D = "#c97e10"
RUST    = "#d4793a"
RUST_D  = "#b5531f"
INK     = "#3a2a1c"   # dark text / dark fills on the cream field
MUTED   = "#9a8163"
GEAR    = "#e8923f"
NODE    = "#fffaf0"   # light card on the cream field
CREAM   = "#f3e7cf"   # light text on the dark panel
GREEN   = "#1f8a4c"
GREEN_B = "#bfe26a"   # bioluminescent green-gold accent (matches the cover)

W, H = 720, 300
# the night-sky panel on the right
PX, PY, PW, PH = 490, 14, 216, 272
FONT = "Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif"
MONO = "SF Mono,JetBrains Mono,Menlo,Consolas,monospace"


def header(label: str) -> str:
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" '
        f'xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 {W} {H}" '
        f'role="img" aria-label="{esc(label)}" font-family="{FONT}">'
    )


def defs() -> str:
    # NOTE: WeasyPrint's SVG engine drops gradients when a <marker>/<filter>
    # shares the <defs>, and resolves objectBoundingBox gradients unreliably.
    # So we use ONLY userSpaceOnUse gradients here, draw arrowheads as explicit
    # triangles, and fake glows with stacked translucent circles.
    return (
        '<defs>'
        f'<linearGradient id="fld" x1="0" y1="0" x2="{W}" y2="{H}" gradientUnits="userSpaceOnUse">'
        f'<stop offset="0" stop-color="{FIELD}"/>'
        f'<stop offset="1" stop-color="{FIELD2}"/></linearGradient>'
        # the night-sky panel gradient:
        f'<linearGradient id="pnl" x1="{PX}" y1="{PY}" x2="{PX+PW}" y2="{PY+PH}" gradientUnits="userSpaceOnUse">'
        f'<stop offset="0" stop-color="{PANEL1}"/>'
        f'<stop offset="1" stop-color="{PANEL2}"/></linearGradient>'
        # root-space horizontal gold gradient for cards, bars and chips:
        f'<linearGradient id="grh" x1="0" y1="0" x2="{W}" y2="0" gradientUnits="userSpaceOnUse">'
        f'<stop offset="0" stop-color="{AMBER}"/>'
        f'<stop offset="1" stop-color="{RUST}"/></linearGradient>'
        '</defs>'
    )


def emblem() -> str:
    """The constant glowing-firefly emblem on the night-sky panel (right)."""
    cx, cy = 601, 150
    return (
        f'<g transform="translate({cx},{cy})">'
        # ambient bloom on the panel
        f'<circle r="66" fill="{AMBER}" opacity="0.07"/>'
        f'<circle r="40" fill="{AMBER_B}" opacity="0.10"/>'
        # light trail curving in from lower-left (two strokes = a taper)
        f'<path d="M-80,84 C-46,44 -24,18 -8,-2" fill="none" stroke="{AMBER}" '
        f'stroke-width="2.4" opacity="0.10" stroke-linecap="round"/>'
        f'<path d="M-80,84 C-46,44 -24,18 -8,-2" fill="none" stroke="#ffd980" '
        f'stroke-width="1" opacity="0.22" stroke-linecap="round"/>'
        # the firefly, gently tilted for life
        '<g transform="rotate(-16)">'
        f'<circle cx="0" cy="30" r="24" fill="{AMBER}" opacity="0.18"/>'
        f'<circle cx="0" cy="30" r="14" fill="{AMBER_B}" opacity="0.45"/>'
        # wings, swept back and translucent
        f'<path d="M-3,-6 C-40,-30 -52,-2 -16,8 Z" fill="#ffd980" opacity="0.22"/>'
        f'<path d="M3,-6 C40,-30 52,-2 16,8 Z" fill="#ffd980" opacity="0.22"/>'
        # glowing abdomen
        f'<ellipse cx="0" cy="26" rx="11" ry="16" fill="{AMBER}"/>'
        f'<ellipse cx="0" cy="28" rx="6" ry="10" fill="#fff2cf"/>'
        # dark thorax + head with an amber rim
        f'<ellipse cx="0" cy="2" rx="9" ry="13" fill="#1a130c" stroke="{AMBER_D}" stroke-width="1.6"/>'
        f'<ellipse cx="0" cy="-13" rx="5.5" ry="6.5" fill="#1a130c" stroke="{AMBER_D}" stroke-width="1.3"/>'
        # antennae
        f'<path d="M-3,-18 C-9,-28 -13,-30 -17,-33" fill="none" stroke="{AMBER_D}" stroke-width="1.5" stroke-linecap="round"/>'
        f'<path d="M3,-18 C9,-28 13,-30 17,-33" fill="none" stroke="{AMBER_D}" stroke-width="1.5" stroke-linecap="round"/>'
        '</g>'
        # satellite motes (one bioluminescent green, matching the cover)
        f'<circle cx="50" cy="-46" r="2" fill="#ffd980" opacity="0.7"/>'
        f'<circle cx="62" cy="28" r="1.6" fill="{GREEN_B}" opacity="0.75"/>'
        f'<circle cx="-54" cy="-40" r="1.6" fill="#ffd980" opacity="0.6"/>'
        '</g>'
    )


def frame(num: str, kicker: str) -> str:
    """Cream field, gold accent bar, night-sky panel + motes, chapter kicker."""
    return (
        f'<rect width="{W}" height="{H}" fill="url(#fld)"/>'
        f'<rect x="0" y="0" width="8" height="{H}" fill="url(#grh)"/>'
        # night-sky panel on the right
        f'<rect x="{PX}" y="{PY}" width="{PW}" height="{PH}" rx="16" fill="url(#pnl)"/>'
        f'<rect x="{PX}" y="{PY}" width="{PW}" height="{PH}" rx="16" fill="none" '
        f'stroke="{AMBER}" stroke-width="1" opacity="0.22"/>'
        # a few distant motes inside the panel
        f'<g fill="#ffd980"><circle cx="528" cy="56" r="1.4" opacity="0.6"/>'
        f'<circle cx="678" cy="236" r="1.4" opacity="0.55"/>'
        f'<circle cx="644" cy="66" r="1.1" opacity="0.5"/>'
        f'<circle cx="512" cy="210" r="1.1" opacity="0.45"/></g>'
        f'<circle cx="552" cy="246" r="1.3" fill="{GREEN_B}" opacity="0.6"/>'
        # chapter number + kicker — a TOP-left header row, clear of the scene
        # (scenes draw from y>=56 down, so the header never overlaps them).
        f'<text x="40" y="40" fill="{RUST_D}" font-size="15" font-weight="800" '
        f'letter-spacing="2.5">{esc(num)}</text>'
        f'<text x="186" y="40" fill="{MUTED}" font-size="12.5" font-weight="600" '
        f'letter-spacing="1.5" font-family="{MONO}">{esc(kicker)}</text>'
        f'<rect x="40" y="49" width="46" height="4" rx="2" fill="{RUST}"/>'
    )


def card(x, y, w, label, fill=NODE, stroke=RUST, tcol=INK, fs=14):
    return (
        f'<g transform="translate({x},{y})">'
        f'<rect x="0" y="2.5" width="{w}" height="40" rx="9" fill="#d9c4a3" opacity="0.30"/>'
        f'<rect x="0" y="0" width="{w}" height="40" rx="9" fill="{fill}" '
        f'stroke="{stroke}" stroke-width="1.8"/>'
        f'<text x="{w/2}" y="25" text-anchor="middle" fill="{tcol}" '
        f'font-size="{fs}" font-weight="700">{esc(label)}</text></g>'
    )


def chip(x, y, label, fill=AMBER):
    w = 16 + len(label) * 8.2
    return (
        f'<g transform="translate({x},{y})">'
        f'<rect x="0" y="0" width="{w:.0f}" height="30" rx="15" fill="{fill}" opacity="0.92"/>'
        f'<text x="{w/2:.0f}" y="20" text-anchor="middle" fill="#16110c" '
        f'font-size="13" font-weight="700">{esc(label)}</text></g>'
    )


def arrow(x1, y1, x2, y2):
    """A line with an explicit triangular arrowhead (no <marker>, which would
    break gradient rendering in WeasyPrint)."""
    import math
    ang = math.atan2(y2 - y1, x2 - x1)
    bx = x2 - 8 * math.cos(ang)
    by = y2 - 8 * math.sin(ang)
    p1x, p1y = x2, y2
    perp = ang + math.pi / 2
    hw = 4.5
    p2x = bx + hw * math.cos(perp)
    p2y = by + hw * math.sin(perp)
    p3x = bx - hw * math.cos(perp)
    p3y = by - hw * math.sin(perp)
    return (f'<line x1="{x1}" y1="{y1}" x2="{bx:.1f}" y2="{by:.1f}" stroke="{RUST_D}" '
            f'stroke-width="3.5"/>'
            f'<polygon points="{p1x:.1f},{p1y:.1f} {p2x:.1f},{p2y:.1f} {p3x:.1f},{p3y:.1f}" '
            f'fill="{RUST_D}"/>')


def spark(x, y, r=6, fill=AMBER, op=1.0):
    s = (f'M0,-{r} L{r*0.22:.1f},-{r*0.22:.1f} L{r},0 L{r*0.22:.1f},{r*0.22:.1f} '
         f'L0,{r} L-{r*0.22:.1f},{r*0.22:.1f} L-{r},0 L-{r*0.22:.1f},-{r*0.22:.1f} Z')
    return f'<g transform="translate({x},{y})" opacity="{op}"><path d="{s}" fill="{fill}"/></g>'


# ---------------------------------------------------------------------------
# Per-chapter left-side scenes. Each returns an SVG fragment drawn over the
# common frame; the constant emblem is appended by the assembler. Scenes draw
# within x < 480 so they never collide with the night-sky panel.
# ---------------------------------------------------------------------------
def s_choice():  # why firefly — chaos -> cohesion
    cards = (card(60, 56, 96, "axum?", NODE, MUTED, MUTED, 13)
             + card(120, 110, 96, "sqlx?", NODE, MUTED, MUTED, 13)
             + card(56, 164, 96, "DI?", NODE, MUTED, MUTED, 13))
    return (cards + arrow(232, 130, 296, 130)
            + card(308, 108, 150, "Firefly Core", "url(#grh)", RUST_D, "#16110c", 16)
            + spark(250, 72) + spark(280, 178, 5, AMBER, 0.8))


def s_quickstart():
    steps = ["cargo new", "Core::new", "cargo run"]
    out = []
    x = 52
    for i, st in enumerate(steps):
        out.append(card(x, 124, 116, st, NODE, RUST, INK, 13))
        if i < 2:
            out.append(arrow(x + 116, 144, x + 132, 144))
        x += 148
    return "".join(out) + spark(150, 84) + chip(56, 196, "8081 ↑", AMBER)


def s_config():
    layers = [("default", 56), ("profile: dev", 92), ("env / secrets", 128)]
    out = []
    for lbl, y in layers:
        out.append(card(56, y, 200, lbl, NODE, RUST, INK, 13))
    out.append(arrow(150, 174, 150, 206))
    out.append(card(96, 208, 120, "Settings", "url(#grh)", RUST_D, "#16110c", 14))
    return "".join(out)


def s_di():
    hub = card(120, 120, 120, "Context", "url(#grh)", RUST_D, "#16110c", 14)
    beans = (chip(40, 58, "#[component]") + chip(40, 188, "Arc<dyn Port>")
             + chip(300, 60, "@autowired") + chip(300, 188, "lifecycle"))
    spokes = (f'<g stroke="{RUST}" stroke-width="2" opacity="0.5">'
              f'<line x1="120" y1="88" x2="180" y2="120"/>'
              f'<line x1="120" y1="196" x2="180" y2="160"/>'
              f'<line x1="300" y1="76" x2="240" y2="126"/>'
              f'<line x1="300" y1="196" x2="240" y2="154"/></g>')
    return spokes + beans + hub


def s_wiring():
    return (card(52, 70, 104, "Core", "url(#grh)", RUST_D, "#16110c", 14)
            + card(52, 130, 104, "cache", NODE, RUST, INK, 13)
            + card(52, 190, 104, "broker", NODE, RUST, INK, 13)
            + arrow(160, 90, 220, 130) + arrow(160, 150, 220, 150) + arrow(160, 210, 220, 170)
            + card(230, 128, 110, "compose()", NODE, RUST_D, RUST_D, 13))


def s_reactive():
    # Mono (one) and Flux (stream of pulses)
    mono = (f'<text x="56" y="92" fill="{RUST_D}" font-size="15" font-weight="800" '
            f'font-family="{MONO}">Mono&lt;T&gt;</text>'
            f'<circle cx="70" cy="116" r="11" fill="{AMBER}"/>'
            f'<line x1="92" y1="116" x2="300" y2="116" stroke="{MUTED}" stroke-width="2"/>')
    flux = (f'<text x="56" y="170" fill="{RUST_D}" font-size="15" font-weight="800" '
            f'font-family="{MONO}">Flux&lt;T&gt;</text>')
    dots = "".join(
        f'<circle cx="{72 + i*46}" cy="196" r="{9 - i}" fill="{AMBER if i%2==0 else RUST}"/>'
        for i in range(5))
    line = f'<line x1="56" y1="196" x2="320" y2="196" stroke="{MUTED}" stroke-width="2"/>'
    return mono + flux + line + dots + '<text x="300" y="200" fill="%s" font-size="22">▶</text>' % RUST_D


def s_http():
    return (chip(48, 70, "GET /wallets") + chip(48, 134, "POST /wallets")
            + arrow(196, 130, 256, 130)
            + card(266, 108, 120, "Router", "url(#grh)", RUST_D, "#16110c", 14)
            + chip(300, 196, "200 OK", AMBER))


def s_persist():
    db = (f'<g transform="translate(300,90)"><ellipse cx="0" cy="0" rx="46" ry="14" fill="url(#grh)"/>'
          f'<path d="M-46,0 V72 A46,14 0 0 0 46,72 V0" fill="{GEAR}" opacity="0.85"/>'
          f'<ellipse cx="0" cy="36" rx="46" ry="14" fill="{RUST}" opacity="0.5"/>'
          f'<ellipse cx="0" cy="72" rx="46" ry="14" fill="{RUST}" opacity="0.5"/></g>')
    return (card(48, 116, 132, "Repository", NODE, RUST, INK, 14)
            + arrow(182, 136, 244, 136) + db
            + chip(60, 188, "find · save · delete"))


def s_ddd():
    agg = card(170, 116, 150, "Wallet aggregate", "url(#grh)", RUST_D, "#16110c", 13)
    vos = (chip(40, 70, "Money") + chip(40, 196, "WalletId")
           + chip(330, 70, "invariant") + chip(330, 196, "domain event"))
    return vos + agg


def s_cqrs():
    return (card(48, 64, 150, "DepositCommand", NODE, RUST, INK, 13)
            + card(48, 172, 150, "BalanceQuery", NODE, RUST, INK, 13)
            + arrow(202, 84, 262, 126) + arrow(202, 192, 262, 150)
            + card(272, 108, 110, "Bus", "url(#grh)", RUST_D, "#16110c", 15)
            + arrow(384, 130, 410, 130))


def s_eda():
    hub = card(160, 120, 120, "EventBus", "url(#grh)", RUST_D, "#16110c", 13)
    nodes = (chip(40, 64, "Kafka") + chip(40, 200, "RabbitMQ")
             + chip(320, 66, "listener") + chip(316, 196, "projection"))
    spokes = (f'<g stroke="{RUST}" stroke-width="2" opacity="0.5">'
              f'<line x1="120" y1="80" x2="200" y2="120"/>'
              f'<line x1="100" y1="208" x2="200" y2="160"/>'
              f'<line x1="320" y1="80" x2="280" y2="128"/>'
              f'<line x1="320" y1="204" x2="280" y2="158"/></g>')
    return spokes + nodes + hub


def s_es():
    dots = "".join(card(40 + i*70, 116, 56, e, NODE, RUST, INK, 12)
                   for i, e in enumerate(["+10", "-3", "+5"]))
    return (dots + arrow(220, 136, 248, 136)
            + card(258, 110, 120, "= balance 12", "url(#grh)", RUST_D, "#16110c", 13)
            + chip(40, 188, "replay the stream"))


def s_clients():
    return (card(48, 116, 120, "Lumen", "url(#grh)", RUST_D, "#16110c", 14)
            + arrow(172, 136, 250, 136)
            + card(260, 116, 130, "Payments API", NODE, RUST, INK, 13)
            + chip(60, 190, "WebClient · retry · breaker"))


def s_bff():
    return (chip(40, 130, "mobile") + arrow(120, 144, 170, 144)
            + card(180, 120, 110, "BFF", "url(#grh)", RUST_D, "#16110c", 15)
            + arrow(292, 122, 330, 96) + arrow(292, 144, 340, 144) + arrow(292, 166, 330, 192)
            + chip(338, 80, "wallets") + chip(348, 130, "ledger") + chip(338, 180, "fx"))


def s_saga():
    steps = ["debit", "credit", "notify"]
    out, x = [], 44
    for i, st in enumerate(steps):
        out.append(card(x, 96, 92, st, NODE, RUST, INK, 13))
        if i < 2:
            out.append(arrow(x + 92, 116, x + 110, 116))
        x += 110
    out.append(f'<path d="M370,140 C300,210 130,210 64,152" fill="none" '
               f'stroke="{RUST_D}" stroke-width="2.4" stroke-dasharray="6 5"/>')
    out.append(f'<polygon points="64,152 78,150 74,162" fill="{RUST_D}"/>')
    out.append(f'<text x="210" y="212" text-anchor="middle" fill="{RUST_D}" '
               f'font-size="13" font-weight="700">compensate</text>')
    return "".join(out)


def s_security():
    shield = (f'<g transform="translate(150,150)"><path d="M0,-66 L58,-44 '
              f'C58,18 30,54 0,68 C-30,54 -58,18 -58,-44 Z" fill="url(#grh)"/>'
              f'<path d="M-18,2 L-4,18 L24,-18" fill="none" stroke="{FIELD}" '
              f'stroke-width="6" stroke-linecap="round" stroke-linejoin="round"/></g>')
    return shield + chip(280, 96, "JWT") + chip(280, 150, "#[secure]") + chip(280, 204, "roles")


def s_observe():
    bars = "".join(
        f'<rect x="{52 + i*30}" y="{200 - h}" width="18" height="{h}" rx="3" '
        f'fill="{AMBER if i%2==0 else RUST}"/>'
        for i, h in enumerate([40, 70, 55, 95, 72, 110]))
    return (bars + f'<line x1="44" y1="200" x2="240" y2="200" stroke="{MUTED}" stroke-width="2"/>'
            + chip(270, 96, "/health") + chip(270, 150, "/metrics") + chip(270, 204, "traces"))


def s_cache():
    return (chip(48, 80, "request") + arrow(132, 94, 180, 94)
            + card(190, 72, 110, "Cache", "url(#grh)", RUST_D, "#16110c", 14)
            + card(190, 150, 110, "Resilience", NODE, RUST, INK, 13)
            + chip(316, 80, "cache hit") + chip(316, 162, "retry · breaker"))


def s_sched():
    clock = (f'<g transform="translate(140,150)"><circle r="58" fill="url(#grh)"/>'
             f'<circle r="46" fill="{FIELD}"/>'
             f'<line x1="0" y1="0" x2="0" y2="-32" stroke="{RUST_D}" stroke-width="4" stroke-linecap="round"/>'
             f'<line x1="0" y1="0" x2="26" y2="10" stroke="{RUST_D}" stroke-width="4" stroke-linecap="round"/>'
             f'<circle r="4" fill="{RUST_D}"/></g>')
    return clock + chip(250, 96, "@scheduled") + chip(250, 150, "notify") + chip(250, 204, "webhook")


def s_macros():
    return (f'<text x="48" y="150" fill="{RUST_D}" font-size="40" font-weight="800" '
            f'font-family="{MONO}">#[..]</text>'
            + chip(210, 90, "#[handler]") + chip(210, 142, "#[route]")
            + chip(210, 194, "#[saga_step]")
            + arrow(190, 150, 206, 150))


def s_testing():
    return (card(48, 116, 130, "StepVerifier", NODE, RUST, INK, 13)
            + arrow(182, 136, 244, 136)
            + card(254, 116, 120, "Testcontainers", "url(#grh)", RUST_D, "#16110c", 12)
            + chip(60, 70, "all green", AMBER) + chip(60, 190, "real infra"))


def s_cli():
    term = (f'<g transform="translate(48,80)"><rect x="0" y="0" width="280" height="140" rx="10" '
            f'fill="#16110c"/><circle cx="18" cy="18" r="5" fill="{RUST}"/>'
            f'<circle cx="36" cy="18" r="5" fill="{AMBER}"/><circle cx="54" cy="18" r="5" fill="{GREEN}"/>'
            f'<text x="18" y="62" fill="{AMBER}" font-size="16" font-family="{MONO}">$ firefly new</text>'
            f'<text x="18" y="92" fill="{NODE}" font-size="16" font-family="{MONO}">$ firefly run</text>'
            f'<text x="18" y="122" fill="{NODE}" font-size="16" font-family="{MONO}">$ firefly migrate ▮</text></g>')
    return term


def s_prod():
    return (card(48, 116, 110, "build", NODE, RUST, INK, 13)
            + arrow(160, 136, 184, 136)
            + card(192, 116, 110, "image", NODE, RUST, INK, 13)
            + arrow(304, 136, 328, 136)
            + card(336, 116, 60, "ship", "url(#grh)", RUST_D, "#16110c", 13)
            + chip(60, 70, "12-factor") + chip(180, 196, "k8s · health · graceful"))


def s_appa():
    return (card(40, 96, 130, "Spring Boot", NODE, MUTED, MUTED, 13)
            + arrow(174, 116, 234, 116)
            + card(244, 96, 130, "Firefly", "url(#grh)", RUST_D, "#16110c", 14)
            + chip(60, 178, "@Component → #[component]")
            + chip(60, 214, "@Transactional → #[transactional]"))


def s_appb():
    rows = "".join(card(40, 70 + i*52, 360, c, NODE, RUST, INK, 13)
                   for i, c in enumerate(["firefly-core", "firefly-eda", "firefly-cqrs"]))
    return rows


def s_bootstrap():
    # FireflyApplication::new("lumen").run() ignites the whole stack: one call
    # on the left fans out into the services the framework discovers & wires.
    call = (card(48, 132, 150, "new().run()", "url(#grh)", RUST_D, "#16110c", 14)
            + spark(60, 90, 5, AMBER, 0.85) + spark(178, 102, 4, AMBER_B, 0.8))
    fans = (arrow(202, 142, 270, 78) + arrow(202, 150, 274, 150) + arrow(202, 158, 270, 222))
    pieces = (chip(280, 64, "web + CQRS") + chip(284, 136, "scan beans")
              + chip(280, 208, "admin + docs"))
    return call + fans + pieces


def s_openapi():
    # routes + DTOs are harvested into one generated spec, served as Swagger/ReDoc.
    sources = (chip(40, 70, "#[rest_controller]") + chip(40, 196, "#[derive(Schema)]"))
    funnel = (arrow(196, 92, 256, 132) + arrow(196, 210, 256, 156))
    doc = (f'<g transform="translate(266,104)">'
           f'<rect x="0" y="3" width="92" height="92" rx="9" fill="#d9c4a3" opacity="0.30"/>'
           f'<rect x="0" y="0" width="92" height="92" rx="9" fill="{NODE}" '
           f'stroke="{RUST}" stroke-width="1.8"/>'
           # a little "spec" sheet: title bar + lines
           f'<rect x="14" y="16" width="64" height="8" rx="4" fill="url(#grh)"/>'
           + "".join(f'<rect x="14" y="{34 + r*13}" width="{64 - r*8}" height="5" rx="2.5" '
                     f'fill="{MUTED}" opacity="0.7"/>' for r in range(4))
           + f'</g>')
    out = (f'<text x="312" y="222" text-anchor="middle" fill="{RUST_D}" '
           f'font-size="12.5" font-weight="700">openapi.json</text>')
    return sources + funnel + doc + out + chip(290, 70, "Swagger · ReDoc", AMBER)


def s_layered():
    # five separately-compiled crates stacked like a multi-module project.
    crates = ["…-interfaces", "…-models", "…-core", "…-web", "…-sdk"]
    rows = []
    for i, c in enumerate(crates):
        y = 60 + i * 38
        fill = "url(#grh)" if i == 4 else NODE
        stroke = RUST_D if i == 4 else RUST
        tcol = "#16110c" if i == 4 else INK
        # slight left inset per layer to read as a stack
        rows.append(card(44 + i * 6, y, 300 - i * 12, c, fill, stroke, tcol, 13))
    return "".join(rows) + spark(420, 96, 5, AMBER, 0.8) + spark(404, 196, 4, AMBER_B, 0.7)


SCENES = {
    "ch01": ("Why Firefly — infinite choice becomes cohesion", "WHY FIREFLY", s_choice),
    "ch02": ("Quickstart — cargo new to a running service", "QUICKSTART", s_quickstart),
    "ch03": ("Configuration — layered defaults, profiles, secrets", "CONFIGURATION", s_config),
    "ch04": ("Dependency Injection — the application context", "DEPENDENCY INJECTION", s_di),
    "ch05": ("Dependency Wiring — composing the core", "WIRING", s_wiring),
    "ch04b": ("Application Bootstrap — one call ignites the stack", "BOOTSTRAP", s_bootstrap),
    "ch06": ("The Reactive Model — Mono and Flux", "MONO & FLUX", s_reactive),
    "ch07": ("Your first HTTP API — routes and handlers", "HTTP API", s_http),
    "ch06a": ("OpenAPI — routes and DTOs become a served spec", "OPENAPI", s_openapi),
    "ch08": ("Persistence — reactive repositories", "PERSISTENCE", s_persist),
    "ch09": ("Domain-Driven Design — aggregates and value objects", "DOMAIN MODEL", s_ddd),
    "ch10": ("CQRS — commands and queries on a bus", "CQRS", s_cqrs),
    "ch11": ("Event-Driven Architecture and messaging", "EVENTS & MESSAGING", s_eda),
    "ch12": ("Event Sourcing — replaying the ledger", "EVENT SOURCING", s_es),
    "ch13": ("HTTP clients — calling other services", "HTTP CLIENTS", s_clients),
    "ch14": ("The experience tier — composing a BFF", "EXPERIENCE TIER", s_bff),
    "ch15": ("Sagas, workflows and TCC — and compensation", "SAGAS", s_saga),
    "ch22b": ("Layered microservices — separately-compiled crates", "LAYERED SERVICES", s_layered),
    "ch16": ("Security, sessions and identity", "SECURITY", s_security),
    "ch17": ("Observability and health", "OBSERVABILITY", s_observe),
    "ch18": ("Caching and resilience", "CACHING", s_cache),
    "ch19": ("Scheduling, notifications and webhooks", "SCHEDULING", s_sched),
    "ch20": ("Declarative services with macros", "MACROS", s_macros),
    "ch21": ("Testing Firefly applications", "TESTING", s_testing),
    "ch22": ("The firefly CLI", "THE CLI", s_cli),
    "ch23": ("Extending Firefly and going to production", "PRODUCTION", s_prod),
    "appa": ("Spring Boot to Firefly cheat-sheet", "APPENDIX A", s_appa),
    "appb": ("Crate and module index", "MODULE INDEX", s_appb),
}

# kicker numbers shown top-left, keyed by opener id. The order below is the
# book's reading order and the display number must match book.yaml's `num:`.
# Inserting 04b-bootstrap, 06a-openapi and 22-layered renumbers everything that
# follows, so this is an EXPLICIT ordered list rather than a range() formula.
_CH_ORDER = [
    "ch01", "ch02", "ch03", "ch04", "ch05", "ch04b", "ch06",   # Part I  (1..7)
    "ch07", "ch06a", "ch08", "ch09", "ch10",                    # Part II (8..12)
    "ch11", "ch12",                                             # Part III (13..14)
    "ch13", "ch14", "ch15", "ch22b",                            # Part IV (15..18)
    "ch16", "ch17", "ch18", "ch19", "ch20", "ch21", "ch22", "ch23",  # Part V (19..26)
]
NUMS = {oid: f"CHAPTER {i}" for i, oid in enumerate(_CH_ORDER, start=1)}
NUMS["appa"] = "APPENDIX A"
# book.yaml maps the module-index appendix (opener id appb) to "Appendix A".
NUMS["appb"] = "APPENDIX A"


def build_one(oid: str) -> str:
    label, kicker, scene = SCENES[oid]
    return (header(label) + defs() + frame(NUMS[oid], kicker)
            + scene() + emblem() + "</svg>\n")


def main() -> None:
    ART.mkdir(parents=True, exist_ok=True)
    for oid in SCENES:
        (ART / f"{oid}.svg").write_text(build_one(oid), encoding="utf-8")
    print(f"wrote {len(SCENES)} chapter openers to {ART}")


if __name__ == "__main__":
    main()
