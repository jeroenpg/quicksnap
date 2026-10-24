#!/usr/bin/env python3
"""Writes docs/benchmark-{light,dark}.svg from the measurements in the README. Run: python3 docs/make_chart.py"""
from pathlib import Path

# (label, seconds, is_quicksnap). Medians; see the README table for the raw runs.
PANELS = [
    ("4 monitors · 12720×3840 px (laptop at 200%)", [
        ("QuickSnap", 0.065, True),
        ("Spectacle · current screen", 1.7, False),
        ("Spectacle · whole desktop", 6.4, False),
        ("Flameshot 14 (Flatpak) · screen", 10.7, False),
        ("Flameshot 12.1 (Fedora) · whole desktop", 36.3, False),
    ]),
    ("1 monitor · 1920×1080 px", [
        ("QuickSnap", 0.069, True),
        ("Spectacle · current screen", 0.52, False),
        ("Flameshot 12.1 (Fedora) · whole desktop", 0.58, False),
    ]),
]

W, LABEL_W, VALUE_W = 760, 300, 70
BAR_H, GAP, PANEL_GAP, HEAD = 22, 10, 34, 26
PLOT_W = W - LABEL_W - VALUE_W


def fmt(s):
    return f"{s * 1000:.0f} ms" if s < 1 else f"{s:.1f} s"


rows, y = [], 8
for title, bars in PANELS:
    top = max(s for _, s, _ in bars)
    rows.append(f'<text x="0" y="{y + 16}" class="title">{title}</text>')
    y += HEAD
    for label, s, ours in bars:
        w = max(PLOT_W * s / top, 3)
        cls = "ours" if ours else "other"
        rows.append(
            f'<text x="{LABEL_W - 12}" y="{y + BAR_H / 2 + 5}" class="label{" strong" if ours else ""}" '
            f'text-anchor="end">{label}</text>'
            f'<rect x="{LABEL_W}" y="{y}" width="{w:.1f}" height="{BAR_H}" rx="4" class="{cls}"/>'
            f'<text x="{LABEL_W + w + 8:.1f}" y="{y + BAR_H / 2 + 5}" class="value{" strong" if ours else ""}">{fmt(s)}</text>'
        )
        y += BAR_H + GAP
    y += PANEL_GAP - GAP

H = y - PANEL_GAP + 34
THEMES = {
    "light": dict(text="#52514e", strong="#0b0b0b", ours="#2a78d6", other="#a8a7a0"),
    "dark": dict(text="#c3c2b7", strong="#f0efec", ours="#3987e5", other="#5f5e59"),
}
for name, c in THEMES.items():
    svg = f'''<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" role="img"
  aria-labelledby="t d">
<title id="t">Time until you can start selecting</title>
<desc id="d">QuickSnap shows its overlay in about 65 to 70 milliseconds. On four monitors Spectacle needs 1.7 to 6.4
seconds and Flameshot 10.7 to 36.3 seconds; on one monitor both need about half a second.</desc>
<style>
  text {{ font: 13px -apple-system, "Segoe UI", Helvetica, Arial, sans-serif; fill: {c["text"]}; }}
  .title, .strong {{ font-weight: 600; fill: {c["strong"]}; }}
  .ours {{ fill: {c["ours"]}; }}
  .other {{ fill: {c["other"]}; }}
</style>
{chr(10).join(rows)}
<text x="0" y="{H - 6}" class="label">Each panel has its own scale. Lower is better.</text>
</svg>
'''
    Path(__file__).with_name(f"benchmark-{name}.svg").write_text(svg)
print(f"wrote benchmark-light.svg and benchmark-dark.svg ({W}x{H})")
