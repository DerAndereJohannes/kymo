"""Writes Kymo.icon's light-mode layers from kymo.svg, the icon for every platform: each of its
top-level groups becomes one layer on Icon Composer's 1024 grid. The dark-mode lid and stylus
(lid.svg, stylus.svg) are macOS's alone and edited in Kymo.icon itself. Used by icons.sh:
`python3 scripts/icon-layers.py kymo.svg Kymo.icon/Assets`."""

import sys
import xml.etree.ElementTree as ET
from pathlib import Path

SVG = "http://www.w3.org/2000/svg"
# Each group's layer file, as icon.json names them.
LAYERS = {
    "body": "body.svg",
    "lid": "lid-light.svg",
    "trace": "trace.svg",
    "stylus": "stylus-light.svg",
}

source, assets = sys.argv[1], Path(sys.argv[2])
ET.register_namespace("", SVG)
groups = {g.get("id"): g for g in ET.parse(source).getroot().findall(f"{{{SVG}}}g")}
if set(groups) != set(LAYERS):
    sys.exit(f"{source}: expected the groups {sorted(LAYERS)}, found {sorted(groups)}")
for name, file in LAYERS.items():
    layer = ET.Element(f"{{{SVG}}}svg", viewBox="0 0 1024 1024")
    layer.extend(groups[name])
    (assets / file).write_text(ET.tostring(layer, encoding="unicode") + "\n")
