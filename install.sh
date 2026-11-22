#!/bin/sh
# Builds Lumaris and installs it for the current user (~/.local), or system wide with
# PREFIX=/usr/local (run as root). Does not change the default image viewer.
set -e
PREFIX="${PREFIX:-$HOME/.local}"
cd "$(dirname "$0")"

cargo build --release

install -Dm755 target/release/lumaris "$PREFIX/bin/lumaris"
install -Dm644 packaging/lumaris.desktop "$PREFIX/share/applications/lumaris.desktop"
# icon name lumaris-app (a new name makes running KDE sessions load the current picture)
rm -f "$PREFIX"/share/icons/hicolor/*/apps/lumaris.png "$PREFIX/share/icons/hicolor/scalable/apps/lumaris.svg"
install -Dm644 packaging/lumaris.svg "$PREFIX/share/icons/hicolor/scalable/apps/lumaris-app.svg"
# PNG sizes for panels and menus that do not use the SVG
if command -v rsvg-convert >/dev/null 2>&1; then
    for s in 16 22 24 32 48 64 128 256 512; do
        d="$PREFIX/share/icons/hicolor/${s}x${s}/apps"
        mkdir -p "$d"
        rsvg-convert -w "$s" -h "$s" packaging/lumaris.svg -o "$d/lumaris-app.png"
    done
fi

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$PREFIX/share/applications" || true
# refresh the icon index only where one exists (a stale index would hide the new icon)
if [ -f "$PREFIX/share/icons/hicolor/icon-theme.cache" ] && command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" 2>/dev/null || rm -f "$PREFIX/share/icons/hicolor/icon-theme.cache"
fi
command -v kbuildsycoca6 >/dev/null 2>&1 && kbuildsycoca6 >/dev/null 2>&1 || true

echo "Installed Lumaris to $PREFIX"
echo "Make it the default image viewer in System Settings > Default Applications, or:"
echo "  xdg-mime default lumaris.desktop image/png image/jpeg image/gif image/webp"
