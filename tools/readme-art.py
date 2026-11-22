#!/usr/bin/env python3
# Draws the README artwork (title card, buttons, section cards) as SVG and renders it to PNG
# in docs/assets/. Needs rsvg-convert and the Noto Sans font.  Run: tools/readme-art.py
import base64, os, subprocess, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "docs", "assets")
FONT = "Noto Sans"
INK = "#0b1d1a"       # panel
INK2 = "#123b33"      # panel, lighter
MINT = "#24b88a"
MINT_LIGHT = "#76e4be"
GOLD = "#ffd27a"
WHITE = "#f4fffb"


def render(name, svg, width):
    os.makedirs(OUT, exist_ok=True)
    with tempfile.NamedTemporaryFile("w", suffix=".svg", delete=False) as f:
        f.write(svg)
        path = f.name
    subprocess.run(["rsvg-convert", "-w", str(width), path, "-o", os.path.join(OUT, name)], check=True)
    os.unlink(path)


def icon_href():
    png = subprocess.run(["rsvg-convert", "-w", "512", os.path.join(ROOT, "packaging", "lumaris.svg")], check=True, capture_output=True).stdout
    return "data:image/png;base64," + base64.b64encode(png).decode()


def defs(w, h):
    return f'''<defs>
  <linearGradient id="panel" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{INK2}"/><stop offset="1" stop-color="{INK}"/></linearGradient>
  <linearGradient id="fade" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{MINT_LIGHT}" stop-opacity="0"/><stop offset="1" stop-color="{MINT_LIGHT}" stop-opacity="0.35"/></linearGradient>
  <pattern id="scan" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="1" fill="#ffffff" opacity="0.035"/></pattern>
  <filter id="glow" x="-20%" y="-40%" width="140%" height="180%"><feGaussianBlur stdDeviation="6" result="b"/><feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge></filter>
</defs>'''


def slant(x, y, w, h, s):
    """parallelogram leaning right by s pixels"""
    return f"M{x + s} {y} L{x + w} {y} L{x + w - s} {y + h} L{x} {y + h} Z"


def grid(x0, y0, x1, y1, horizon):
    """retro perspective grid between y0 (horizon) and y1"""
    lines = []
    cx = (x0 + x1) / 2
    for i in range(-14, 15):
        lines.append(f'<line x1="{cx + i * 18}" y1="{y0}" x2="{cx + i * 140}" y2="{y1}"/>')
    k = 1.0
    y = y0
    while y < y1:
        lines.append(f'<line x1="{x0}" y1="{y:.1f}" x2="{x1}" y2="{y:.1f}"/>')
        k *= 1.35
        y = y0 + 4 * k
    return f'<g stroke="{MINT_LIGHT}" stroke-width="1.2" opacity="0.28">' + "".join(lines) + "</g>"


def star(x, y, r, color, opacity=1.0):
    return (f'<path d="M{x} {y - r} Q{x + r * 0.18} {y - r * 0.18} {x + r} {y} Q{x + r * 0.18} {y + r * 0.18} {x} {y + r} '
            f'Q{x - r * 0.18} {y + r * 0.18} {x - r} {y} Q{x - r * 0.18} {y - r * 0.18} {x} {y - r} Z" fill="{color}" opacity="{opacity}"/>')


def title_card(icon):
    w, h = 1370, 560
    px, py, pw, ph, s = 250, 150, 1080, 330, 60
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">{defs(w, h)}
  <path d="{slant(px + 14, py + 14, pw, ph, s)}" fill="{MINT}"/>
  <clipPath id="c"><path d="{slant(px, py, pw, ph, s)}"/></clipPath>
  <g clip-path="url(#c)">
    <rect x="{px}" y="{py}" width="{pw}" height="{ph}" fill="url(#panel)"/>
    <rect x="{px}" y="{py + ph * 0.55}" width="{pw}" height="{ph * 0.45}" fill="url(#fade)"/>
    {grid(px, py + ph * 0.58, px + pw, py + ph, 0)}
    <rect x="{px}" y="{py}" width="{pw}" height="{ph}" fill="url(#scan)"/>
  </g>
  <path d="{slant(px, py, pw, ph, s)}" fill="none" stroke="{MINT_LIGHT}" stroke-width="3"/>
  <circle cx="230" cy="300" r="190" fill="{MINT}" opacity="0.18"/>
  <image href="{icon}" x="60" y="110" width="350" height="350"/>
  <text x="840" y="318" text-anchor="middle" font-family="{FONT}" font-weight="bold" font-style="italic" font-size="150"
        fill="{WHITE}" stroke="{INK}" stroke-width="6" paint-order="stroke" filter="url(#glow)">Lumaris</text>
  <g transform="translate(840 378)">
    <path d="{slant(-210, -26, 420, 50, 14)}" fill="{INK}" stroke="{MINT_LIGHT}" stroke-width="2.5"/>
    <text y="10" text-anchor="middle" font-family="{FONT}" font-weight="bold" font-style="italic" font-size="26" letter-spacing="6" fill="{MINT_LIGHT}">IMAGE VIEWER</text>
  </g>
  {star(470, 120, 22, GOLD)}{star(1250, 110, 16, MINT_LIGHT)}{star(1290, 520, 12, GOLD, 0.9)}{star(560, 520, 10, MINT_LIGHT, 0.8)}
</svg>'''


def button(label, shadow):
    w, h, s = 325, 106, 22
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">{defs(w, h)}
  <path d="{slant(10, 12, 305, 86, s)}" fill="{shadow}"/>
  <path d="{slant(2, 4, 305, 86, s)}" fill="url(#panel)" stroke="{MINT_LIGHT}" stroke-width="2.5"/>
  <clipPath id="c"><path d="{slant(2, 4, 305, 86, s)}"/></clipPath>
  <rect x="2" y="4" width="305" height="86" fill="url(#scan)" clip-path="url(#c)"/>
  <text x="154" y="61" text-anchor="middle" font-family="{FONT}" font-weight="bold" font-style="italic" font-size="34"
        fill="{WHITE}" stroke="{INK}" stroke-width="5" paint-order="stroke">{label}</text>
</svg>'''


def section(title, subtitle, accent):
    w, h = 1360, 300
    px, py, pw, ph, s = 20, 50, 1300, 200, 50
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">{defs(w, h)}
  <path d="{slant(px + 12, py + 12, pw, ph, s)}" fill="{accent}"/>
  <clipPath id="c"><path d="{slant(px, py, pw, ph, s)}"/></clipPath>
  <g clip-path="url(#c)">
    <rect x="{px}" y="{py}" width="{pw}" height="{ph}" fill="url(#panel)"/>
    <rect x="{px}" y="{py + ph * 0.5}" width="{pw}" height="{ph * 0.5}" fill="url(#fade)"/>
    {grid(px, py + ph * 0.55, px + pw, py + ph, 0)}
    <rect x="{px}" y="{py}" width="{pw}" height="{ph}" fill="url(#scan)"/>
  </g>
  <path d="{slant(px, py, pw, ph, s)}" fill="none" stroke="{MINT_LIGHT}" stroke-width="3"/>
  <text x="120" y="150" font-family="{FONT}" font-weight="bold" font-style="italic" font-size="82"
        fill="{MINT_LIGHT}" stroke="{INK}" stroke-width="6" paint-order="stroke" filter="url(#glow)">{title}</text>
  <text x="124" y="204" font-family="{FONT}" font-weight="bold" font-style="italic" font-size="32" fill="{GOLD}">{subtitle}</text>
  {star(1230, 60, 18, GOLD)}{star(1275, 230, 11, MINT_LIGHT, 0.85)}
</svg>'''


if __name__ == "__main__":
    render("title-card.png", title_card(icon_href()), 1370)
    shadows = [MINT, GOLD]
    for i, (name, label) in enumerate([("install", "INSTALL"), ("features", "FEATURES"), ("keys", "KEYS"), ("settings", "SETTINGS"),
                                       ("desktops", "DESKTOPS"), ("build", "BUILD"), ("news", "WHAT'S NEW")]):
        render(f"button-{name}.png", button(label, shadows[i % 2]), 325)
    for name, title, sub, accent in [
        ("install", "Install", "Up and running in a minute.", MINT),
        ("features", "Features", "Everything a viewer needs, nothing it doesn't.", GOLD),
        ("keys", "Keys", "Press F1 in Lumaris for all of them.", MINT),
        ("settings", "Settings", "Right-click, Settings. Changes apply at once.", GOLD),
        ("desktops", "Desktops", "Where everything works, and where some of it doesn't.", MINT),
        ("build", "Build", "For those who like to compile things.", GOLD),
    ]:
        render(f"card-{name}.png", section(title, sub, accent), 1360)
