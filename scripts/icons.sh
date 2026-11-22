#!/bin/sh
# Renders Kymo's icon (crates/kymo/assets/Kymo.icon, an Icon Composer document) to the PNG
# sizes in crates/kymo/assets/png, as macOS draws it in light mode but without macOS's tile,
# just the drum (scripts/icon-bare.swift): the window's icon and Windows' and Linux's, and
# kymo.ico for Windows; and kymo-macos-256.png, a bare binary's Dock icon, with the tile.
# Needs macOS with Icon Composer (its ictool). Run it after changing the icon and commit what
# it writes; Kymo.app gets the icon itself, compiled by bundle.sh, which adapts to light,
# dark, tinted and clear mode.
set -eu
ictool="/Applications/Icon Composer.app/Contents/Executables/ictool"
cd "$(dirname "$0")/../crates/kymo/assets"
mkdir -p png
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# The same icon over a solid black and a solid white tile, from which icon-bare.swift cuts it
# out and writes each size.
for fill in black:0 white:1; do
    name=${fill%%:*}
    value=${fill#*:}
    cp -R Kymo.icon "$tmp/$name.icon"
    python3 -c 'import json, sys
path, value = sys.argv[1], sys.argv[2]
icon = json.load(open(path))
icon["fill"] = {"solid": f"srgb:{value},{value},{value},1"}
json.dump(icon, open(path, "w"))' "$tmp/$name.icon/icon.json" "$value"
    "$ictool" "$tmp/$name.icon" --export-image --output-file "$tmp/$name.png" \
        --platform macOS --rendition Default --width 1024 --height 1024 --scale 1 > /dev/null
done
swift ../../../scripts/icon-bare.swift "$tmp/black.png" "$tmp/white.png" png \
    16 24 32 48 64 128 256 512 1024
# Windows' icon, for the executable (a resource, see crates/kymo/build.rs) and the installer.
python3 ../../../scripts/ico.py kymo.ico png/kymo-16.png png/kymo-24.png png/kymo-32.png \
    png/kymo-48.png png/kymo-64.png png/kymo-256.png
# For a bare binary's Dock icon on macOS (an app bundle has its own): on macOS's icon grid,
# the tile 824 of 1024, centered.
"$ictool" Kymo.icon --export-image --output-file png/kymo-macos-256.png --platform macOS \
    --rendition Default --width 206 --height 206 --scale 1 > /dev/null
swift ../../../scripts/png8.swift --canvas 256 png/kymo-macos-256.png
