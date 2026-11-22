"""Writes crates/kymo/assets/kymo.svg, Kymo's icon for every platform, from the design below:
three channels on a white rounded square, each drawn as Kymo draws a zoomed-out trace, one
vertical line per column from the minimum to the maximum of the samples in it. The middle
channel is the bright one, with a burst. Run scripts/icons.sh afterwards.
`python3 scripts/icon-svg.py`."""

import math
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "crates/kymo/assets/kymo.svg"

# The tile is Icon Composer's 1024 grid; the view box adds a margin around it.
TILE_RADIUS = 230
MARGIN = 48
# The channels: left and right ends, columns, the lines' width.
X0, X1, COLUMNS, WIDTH = 168, 856, 40, 13
SAMPLES = 8000
DIM = "#B3AAFF"
BRIGHT = "#4636D6"
# Each channel: baseline, amplitude, the rhythm's cycles across the icon, its phase, and a
# burst (center and width as fractions of the width, gain) or None.
CHANNELS = {
    "dim": [(316, 30, 2.2, 0.1, None), (708, 28, 2.7, 0.7, None)],
    "bright": [(512, 32, 1.8, 0.3, (0.5, 0.1, 2.2))],
}


def signal(t, amp, freq, phase, burst):
    """A slow rhythm with fast activity on top, which gives each column its height; a burst
    (a spindle) is fast activity growing and fading."""
    a = amp
    if burst:
        center, width, gain = burst
        a *= 1 + gain * math.exp(-(((t - center) / width) ** 2))
    return (
        amp * math.sin(2 * math.pi * (freq * t + phase))
        + 0.3 * amp * math.sin(2 * math.pi * (0.5 * freq * t + 2 * phase))
        + 0.75 * a * math.sin(2 * math.pi * (17.3 * freq * t + 3 * phase))
        + 0.35 * a * math.sin(2 * math.pi * (41.1 * freq * t + phase))
    )


def channel(y0, amp, freq, phase, burst, width):
    """One rounded rectangle per column, from its minimum to its maximum."""
    values = [signal(i / SAMPLES, amp, freq, phase, burst) for i in range(SAMPLES + 1)]
    per = SAMPLES / COLUMNS
    step = (X1 - X0) / (COLUMNS - 1)
    rects = []
    for c in range(COLUMNS):
        column = values[int(c * per) : int((c + 1) * per) + 1]
        low, high = y0 + min(column), y0 + max(column)
        x = X0 + c * step
        rects.append(
            f'<rect x="{x - width / 2:.1f}" y="{low - width / 2:.1f}" width="{width:g}" '
            f'height="{high - low + width:.1f}" rx="{width / 2:g}"/>'
        )
    return "\n".join(rects)


side = 1024 + 2 * MARGIN
parts = [
    f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{-MARGIN} {-MARGIN} {side} {side}">',
    "<!-- Kymo's icon, written by scripts/icon-svg.py. Each top-level group is a layer on Icon",
    "     Composer's 1024 grid, back to front; scripts/icons.sh writes the channels into",
    "     Kymo.icon, where macOS draws the tile itself, white or black. -->",
    '<g id="tile">',
    '<defs><linearGradient id="tile-fill" x1="0" y1="0" x2="0" y2="1">'
    '<stop offset="0" stop-color="#FFFFFF"/><stop offset="1" stop-color="#F0EFF6"/>'
    "</linearGradient></defs>",
    f'<rect x="0" y="0" width="1024" height="1024" rx="{TILE_RADIUS}" fill="url(#tile-fill)" '
    'stroke="#000000" stroke-opacity="0.08" stroke-width="6"/>',
    "</g>",
]
for name, color in [("dim", DIM), ("bright", BRIGHT)]:
    width = WIDTH * (1.15 if name == "bright" else 1)
    parts.append(f'<g id="{name}" fill="{color}">')
    parts += [channel(*spec, width) for spec in CHANNELS[name]]
    parts.append("</g>")
parts.append("</svg>")
OUT.write_text("\n".join(parts) + "\n")
