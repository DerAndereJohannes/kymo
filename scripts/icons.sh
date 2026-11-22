#!/bin/sh
# Builds Kymo's icons from crates/kymo/assets/kymo.svg, the icon for every platform (written
# by scripts/icon-svg.py, which holds the design):
#
# - the PNG sizes in crates/kymo/assets/png (scripts/icon-png.swift), for the window and for
#   Windows and Linux, and kymo.ico from them for Windows;
# - then macOS's: the layers of Kymo.icon, an Icon Composer document in which macOS draws the
#   tile itself, white in light mode and black in dark mode, with dark-mode colors for the
#   channels (scripts/icon-layers.py); and from it kymo-macos-256.png, a bare binary's Dock
#   icon. Kymo.app gets Kymo.icon itself, compiled by bundle.sh, which adapts to light, dark,
#   tinted and clear mode.
#
# Needs macOS, and Icon Composer (its ictool) for the Dock icon. Run it after changing the
# icon and commit what it writes.
set -eu
cd "$(dirname "$0")/../crates/kymo/assets"
mkdir -p png
swift ../../../scripts/icon-png.swift kymo.svg png 16 24 32 48 64 128 256 512 1024
# Windows' icon, for the executable (a resource, see crates/kymo/build.rs) and the installer.
python3 ../../../scripts/ico.py kymo.ico png/kymo-16.png png/kymo-24.png png/kymo-32.png \
    png/kymo-48.png png/kymo-64.png png/kymo-256.png

# macOS.
python3 ../../../scripts/icon-layers.py kymo.svg Kymo.icon/Assets
# For a bare binary's Dock icon (an app bundle has its own): on macOS's icon grid, the tile
# 824 of 1024, centered.
ictool="/Applications/Icon Composer.app/Contents/Executables/ictool"
"$ictool" Kymo.icon --export-image --output-file png/kymo-macos-256.png --platform macOS \
    --rendition Default --width 206 --height 206 --scale 1 > /dev/null
# ictool writes 16 bits per channel; 8 are plenty and a fraction of the size.
swift ../../../scripts/png8.swift --canvas 256 png/kymo-macos-256.png
