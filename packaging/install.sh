#!/bin/sh
# Install Ember for the current user: binaries, desktop entry, icon, URL scheme.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
cargo build --release --manifest-path "$ROOT/Cargo.toml" -p ember
install -Dm755 "$ROOT/target/release/ember" "$PREFIX/bin/ember"
# The sign-in window needs WebKitGTK; without it, store logins use the browser.
if cargo build --release --manifest-path "$ROOT/Cargo.toml" -p ember-sign-in; then
    install -Dm755 "$ROOT/target/release/ember-sign-in" "$PREFIX/bin/ember-sign-in"
else
    echo "Skipped the sign-in window (WebKitGTK 4.1 development files missing); logins will use the browser."
fi
install -Dm644 "$ROOT/packaging/ember.desktop" "$PREFIX/share/applications/ember.desktop"
install -Dm644 "$ROOT/packaging/ember.svg" "$PREFIX/share/icons/hicolor/scalable/apps/ember.svg"
command -v update-desktop-database >/dev/null && update-desktop-database "$PREFIX/share/applications" || true
command -v xdg-mime >/dev/null && xdg-mime default ember.desktop x-scheme-handler/ember || true
echo "Installed Ember to $PREFIX/bin/ember"
