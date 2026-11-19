#!/bin/sh
# Builds Kymo.app around a kymo binary: Contents/MacOS/kymo, Contents/Resources/Kymo.icns
# (from the icon's PNGs in crates/kymo/assets/png) and Contents/Info.plist, which names the
# recordings Kymo opens for Finder's Open With.
#
#   packaging/macos/bundle.sh BINARY VERSION OUTDIR    # writes OUTDIR/Kymo.app
#
# VERSION is the workspace's (0.1.0, or 0.1.0-rc.1, of which the bundle keeps 0.1.0).
# Signing and notarizing are separate (see .github/workflows/binaries.yml).
set -eu
binary=$1
version=$2
outdir=$3
root=$(cd "$(dirname "$0")/../.." && pwd)
png="$root/crates/kymo/assets/png"
# Bundle versions are up to three integers.
short=${version%%-*}

app="$outdir/Kymo.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/kymo"

# The icon, at each size and its double for Retina screens.
iconset=$(mktemp -d)/Kymo.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    cp "$png/kymo-$size.png" "$iconset/icon_${size}x${size}.png"
    cp "$png/kymo-$((size * 2)).png" "$iconset/icon_${size}x${size}@2x.png"
done
iconutil --convert icns --output "$app/Contents/Resources/Kymo.icns" "$iconset"
rm -rf "$(dirname "$iconset")"

# Recordings by extension, each set by the file Kymo is asked to open (any of a set works).
types=""
for entry in \
    "EDF Recording:edf" \
    "BDF Recording:bdf" \
    "BrainVision Recording:vhdr" \
    "BrainVision Recording Format Recording:bvrh" \
    "EEGLAB Recording:set" \
    "FIFF Recording:fif" \
    "GDF Recording:gdf" \
    "XDF Recording:xdf"; do
    name=${entry%%:*}
    ext=${entry#*:}
    types="$types
        <dict>
            <key>CFBundleTypeName</key>
            <string>$name</string>
            <key>CFBundleTypeRole</key>
            <string>Viewer</string>
            <key>LSHandlerRank</key>
            <string>Alternate</string>
            <key>CFBundleTypeExtensions</key>
            <array>
                <string>$ext</string>
            </array>
        </dict>"
done

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Kymo</string>
    <key>CFBundleDisplayName</key>
    <string>Kymo</string>
    <key>CFBundleIdentifier</key>
    <string>io.github.cbrnr.kymo</string>
    <key>CFBundleExecutable</key>
    <string>kymo</string>
    <key>CFBundleIconFile</key>
    <string>Kymo</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleShortVersionString</key>
    <string>$short</string>
    <key>CFBundleVersion</key>
    <string>$short</string>
    <key>NSHumanReadableCopyright</key>
    <string>© Kymo developers</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.medical</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>CFBundleDocumentTypes</key>
    <array>$types
    </array>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist" > /dev/null
echo "$app"
