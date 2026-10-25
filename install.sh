#!/bin/sh
# Build and install into ~/.local/bin (override with PREFIX=/usr/local).
#   ./install.sh               both binaries
#   ./install.sh --cli-only    only `nowplay` (waybar module + remote); needs no GTK
#   ./install.sh --layer-shell popup anchored under your bar on Wayland
set -eu
PREFIX="${PREFIX:-$HOME/.local}"
cd "$(dirname "$0")"

case "${1:-}" in
  --cli-only)
    cargo build --release --no-default-features --bin nowplay
    BINS="nowplay" ;;
  --layer-shell)
    cargo build --release --features layer-shell
    BINS="nowplay nowplay-popup" ;;
  "")
    cargo build --release
    BINS="nowplay nowplay-popup" ;;
  *) echo "unknown option: $1" >&2; exit 2 ;;
esac

mkdir -p "$PREFIX/bin"
for b in $BINS; do
  install -m 755 "target/release/$b" "$PREFIX/bin/$b"
  echo "installed $PREFIX/bin/$b"
done
case ":$PATH:" in *":$PREFIX/bin:"*) ;; *) echo "note: $PREFIX/bin is not in PATH (waybar needs it there)";; esac
