#!/bin/bash
# Lays out DSViewR under PREFIX as the .deb installs it (the cargo-deb assets
# in crates/dsview-gui/Cargo.toml), for the AppImage: the release binaries of
# `cargo build --release -p dsview-gui -p dsview-cli`, the data directory
# share/dsview (assets/ without __pycache__), the desktop entry and the icon.
#
# usage: tools/package/stage.sh PREFIX
set -euo pipefail
PREFIX="$1"
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
mkdir -p "$PREFIX/bin" "$PREFIX/share/dsview" "$PREFIX/share/applications" \
    "$PREFIX/share/icons/hicolor/scalable/apps"
install -m 755 "$REPO/target/release/dsview" "$PREFIX/bin/DSViewR"
install -m 755 "$REPO/target/release/dsview-cli" "$PREFIX/bin/dsviewr-cli"
rsync -a --exclude '__pycache__' \
    "$REPO/assets/res" "$REPO/assets/demo" "$REPO/assets/lang" "$REPO/assets/decoders" \
    "$REPO/assets/ug25.pdf" "$REPO/assets/ug31.pdf" "$REPO/assets/DreamSourceLab.rules" \
    "$PREFIX/share/dsview/"
install -m 644 "$REPO/assets/icons/logo.svg" "$PREFIX/share/dsview/logo.svg"
# the licence of the CJK font built into the binary
install -D -m 644 "$REPO/assets/fonts/OFL.txt" "$PREFIX/share/dsview/fonts/OFL.txt"
install -m 644 "$REPO/assets/dsviewr.desktop" "$PREFIX/share/applications/dsviewr.desktop"
install -m 644 "$REPO/assets/icons/logo.svg" "$PREFIX/share/icons/hicolor/scalable/apps/dsviewr.svg"
