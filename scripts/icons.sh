#!/bin/sh
# Renders Kymo's icon (crates/kymo/assets/Kymo.icon, an Icon Composer document) to the PNG
# sizes in crates/kymo/assets/png, as macOS draws it in light mode: the window's icon and
# Windows' and Linux's, and kymo-macos-256.png, a bare binary's Dock icon. Needs macOS with
# Icon Composer (its ictool). Run it after changing the icon and commit the PNGs; Kymo.app
# gets the icon itself, compiled by bundle.sh, which adapts to light, dark, tinted and clear
# mode.
set -eu
ictool="/Applications/Icon Composer.app/Contents/Executables/ictool"
cd "$(dirname "$0")/../crates/kymo/assets"
mkdir -p png
for size in 16 24 32 48 64 128 256 512 1024; do
    "$ictool" Kymo.icon --export-image --output-file "png/kymo-$size.png" --platform macOS \
        --rendition Default --width "$size" --height "$size" --scale 1 > /dev/null
done
# ictool writes 16 bits per channel; 8 are plenty and a fraction of the size.
swift ../../../scripts/png8.swift png/kymo-*.png
# For a bare binary's Dock icon on macOS (an app bundle has its own): on macOS's icon grid,
# the tile 824 of 1024, centered.
"$ictool" Kymo.icon --export-image --output-file png/kymo-macos-256.png --platform macOS \
    --rendition Default --width 206 --height 206 --scale 1 > /dev/null
swift ../../../scripts/png8.swift --canvas 256 png/kymo-macos-256.png
