#!/usr/bin/env bash
# Builds GitBro in release mode and installs it for the current user:
# binary, icons and launcher entry. Safe to re-run for updates, also while GitBro is running.
#
# Override the targets for a dry run, e.g.:
#   BIN_DIR=/tmp/x/bin DATA_DIR=/tmp/x/share ./install.sh
set -euo pipefail

cd "$(dirname "$0")"

BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
DATA_DIR="${DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}}"

if command -v cargo >/dev/null 2>&1; then
  cargo build --release
elif command -v mise >/dev/null 2>&1; then
  mise exec -- cargo build --release
else
  echo "cargo wurde nicht gefunden (weder direkt noch über mise)." >&2
  exit 1
fi

# Install next to the target and rename: replacing a binary that is currently running
# fails with "Text file busy" when overwritten in place, but a rename is fine.
install -Dm755 target/release/gitbro "$BIN_DIR/gitbro.new"
mv -f "$BIN_DIR/gitbro.new" "$BIN_DIR/gitbro"

install -Dm644 assets/gitbro.svg "$DATA_DIR/icons/hicolor/scalable/apps/gitbro.svg"
for size in 16 24 32 48 64 128 256 512; do
  install -Dm644 "assets/gitbro-$size.png" "$DATA_DIR/icons/hicolor/${size}x${size}/apps/gitbro.png"
done

# Launchers don't always search PATH, so the installed entry points at the binary directly.
mkdir -p "$DATA_DIR/applications"
sed "s|^Exec=.*|Exec=$BIN_DIR/gitbro|" assets/gitbro.desktop > "$DATA_DIR/applications/gitbro.desktop"

# Refresh caches when the tools exist; a missing tool is not an error.
gtk-update-icon-cache -f -t "$DATA_DIR/icons/hicolor" >/dev/null 2>&1 || true
update-desktop-database "$DATA_DIR/applications" >/dev/null 2>&1 || true

echo "GitBro installiert:"
echo "  Programm  $BIN_DIR/gitbro"
echo "  Starter   $DATA_DIR/applications/gitbro.desktop"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "Hinweis: $BIN_DIR liegt nicht im PATH." ;;
esac
