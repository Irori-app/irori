#!/bin/sh
# Install Irori for the current user: binaries, desktop entry, icon, URL scheme.
set -eu
if [ "$(uname -s)" != "Linux" ]; then
    echo "Use packaging/package.py for macOS and Windows; see docs/PLATFORMS.md." >&2
    exit 1
fi
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
cargo build --release --manifest-path "$ROOT/Cargo.toml" -p irori
install -Dm755 "$ROOT/target/release/irori" "$PREFIX/bin/irori"
# The sign-in window needs WebKitGTK; without it, store logins use the browser.
if cargo build --release --manifest-path "$ROOT/Cargo.toml" -p irori-sign-in; then
    install -Dm755 "$ROOT/target/release/irori-sign-in" "$PREFIX/bin/irori-sign-in"
else
    echo "Skipped the sign-in window (WebKitGTK 4.1 development files missing); logins will use the browser."
fi
install -Dm644 "$ROOT/packaging/irori.desktop" "$PREFIX/share/applications/irori.desktop"
install -Dm644 "$ROOT/packaging/irori.svg" "$PREFIX/share/icons/hicolor/scalable/apps/irori.svg"
command -v update-desktop-database >/dev/null && update-desktop-database "$PREFIX/share/applications" || true
command -v xdg-mime >/dev/null && xdg-mime default irori.desktop x-scheme-handler/irori || true
echo "Installed Irori to $PREFIX/bin/irori"
