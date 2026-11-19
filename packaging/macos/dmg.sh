#!/bin/sh
# Puts Kymo.app (signed and notarized by then) and a link to /Applications into a
# compressed disk image, to drag the app onto.
#
#   packaging/macos/dmg.sh APP DMG    # APP is …/Kymo.app; writes DMG
set -eu
app=$1
dmg=$2
stage=$(mktemp -d)
# ditto keeps the app's signature and the stapled ticket as they are.
ditto "$app" "$stage/Kymo.app"
ln -s /Applications "$stage/Applications"
rm -f "$dmg"
hdiutil create -volname Kymo -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$dmg" > /dev/null
rm -rf "$stage"
echo "$dmg"
