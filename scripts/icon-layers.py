"""Writes Kymo.icon's layers from kymo.svg, the icon for every platform: each of its top-level
groups but the tile becomes one layer on Icon Composer's 1024 grid, where macOS draws the tile
itself (light gray in light mode, the system's dark gray in dark mode, as icon.json says).
Dark mode is macOS's alone: each layer also gets a dark variant with the colors below. Used by icons.sh:
`python3 scripts/icon-layers.py kymo.svg Kymo.icon/Assets`."""

import copy
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

SVG = "http://www.w3.org/2000/svg"
# Each group's layer, as icon.json names it, and its colors in dark mode.
LAYERS = {
    "dim": {"#B3AAFF": "#5B50C8"},
    "bright": {"#4636D6": "#5CF2C6"},
}
# Windows' and Linux's tile, which macOS replaces with its own.
TILE = "tile"


def recolor(element, colors):
    for e in element.iter():
        for attribute in ("fill", "stroke", "stop-color"):
            value = e.get(attribute)
            if value is not None and value.upper() in colors:
                e.set(attribute, colors[value.upper()])
    return element


def write(path, group):
    layer = ET.Element(f"{{{SVG}}}svg", viewBox="0 0 1024 1024")
    layer.append(group)
    path.write_text(ET.tostring(layer, encoding="unicode") + "\n")


source, assets = sys.argv[1], Path(sys.argv[2])
ET.register_namespace("", SVG)
groups = {g.get("id"): g for g in ET.parse(source).getroot().findall(f"{{{SVG}}}g")}
if set(groups) != {TILE, *LAYERS}:
    sys.exit(f"{source}: expected the groups {sorted([TILE, *LAYERS])}, found {sorted(groups)}")
assets.mkdir(exist_ok=True)
for name, dark in LAYERS.items():
    write(assets / f"{name}.svg", groups[name])
    colors = {light.upper(): value for light, value in dark.items()}
    write(assets / f"{name}-dark.svg", recolor(copy.deepcopy(groups[name]), colors))
