#!/bin/sh
# Installs Kymo from this archive for the current user, without root: the binary into
# ~/.local/bin, and its desktop entry, icons and file types into ~/.local/share, so it
# starts from the desktop's launcher and file managers offer it under Open With for
# recordings. `./install.sh --uninstall` removes them again (settings and the cache in
# ~/.local/share/kymo and ~/.cache/kymo stay).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
bin="${XDG_BIN_HOME:-$HOME/.local/bin}"
data="${XDG_DATA_HOME:-$HOME/.local/share}"

files="applications/kymo.desktop mime/packages/kymo.xml icons/hicolor/scalable/apps/kymo.svg"
for size in 16 24 32 48 64 128 256 512; do
    files="$files icons/hicolor/${size}x${size}/apps/kymo.png"
done

refresh() {
    # Each is optional: without one, the desktop picks the change up later.
    if command -v update-mime-database > /dev/null; then update-mime-database "$data/mime"; fi
    if command -v update-desktop-database > /dev/null; then
        update-desktop-database "$data/applications"
    fi
    if command -v gtk-update-icon-cache > /dev/null; then
        gtk-update-icon-cache --quiet --ignore-theme-index "$data/icons/hicolor" || true
    fi
}

if [ "${1:-}" = --uninstall ]; then
    rm -f "$bin/kymo"
    for file in $files; do rm -f "$data/$file"; done
    refresh
    echo "Kymo removed"
    exit 0
fi

mkdir -p "$bin"
install -m 755 "$here/kymo" "$bin/kymo"
for file in $files; do
    mkdir -p "$(dirname "$data/$file")"
    install -m 644 "$here/share/$file" "$data/$file"
done
# The desktop's own PATH may lack ~/.local/bin: the entry names the binary in full.
sed "s|^Exec=kymo |Exec=$bin/kymo |" "$here/share/applications/kymo.desktop" \
    > "$data/applications/kymo.desktop"
refresh
echo "Kymo installed to $bin/kymo"
case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "Add $bin to PATH to start it with \"kymo\" in a terminal." ;;
esac
