#!/bin/sh
# Renders Kymo's icon to the PNG sizes in crates/kymo/assets/png, from kymo-small.svg up to
# 32 px (thicker, smoother traces, no hub) and from kymo.svg above. Needs resvg
# (`cargo install resvg`). Run it after changing either SVG and commit the PNGs.
set -eu
cd "$(dirname "$0")/../crates/kymo/assets"
mkdir -p png
for size in 16 24 32 48 64 128 256 512 1024; do
    if [ "$size" -le 32 ]; then svg=kymo-small.svg; else svg=kymo.svg; fi
    resvg --width "$size" --height "$size" "$svg" "png/kymo-$size.png"
done
