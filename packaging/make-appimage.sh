#!/usr/bin/env bash
# Build WorkDesk-Chrome-<version>-x86_64.AppImage (Electron shell + Rust backend).
#   ELECTRON_DIR  an unpacked Electron Linux x64 distribution (v41.x; the folder that contains the `electron` binary)
#   APPIMAGETOOL  path of appimagetool (downloaded if unset)
# Needs the release backend built first:  cargo build --release --manifest-path backend/Cargo.toml
set -euo pipefail
cd "$(dirname "$0")/.."
: "${ELECTRON_DIR:?set ELECTRON_DIR to an unpacked Electron 41.x linux-x64 folder}"
VERSION=$(node -p "require('./electron/package.json').version")
BIN=${BIN:-backend/target/release/workdesk-chrome}
[ -x "$BIN" ] || { echo "Build the backend first (see README)"; exit 1; }
APPDIR=packaging/WorkDesk-Chrome.AppDir
rm -rf "$APPDIR"; mkdir -p "$APPDIR/app" "$APPDIR/bin"
cp -a "$ELECTRON_DIR" "$APPDIR/electron"
install -m755 "$BIN" "$APPDIR/bin/workdesk-chrome"
cp electron/main.js electron/preload.js electron/package.json electron/icon.png "$APPDIR/app/"
cp packaging/workdesk-chrome.desktop packaging/workdesk-chrome.png "$APPDIR/"
cat > "$APPDIR/AppRun" <<'RUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
SB=""
if ! unshare -U true >/dev/null 2>&1; then SB="--no-sandbox"; fi
export WORKDESK_BACKEND="$HERE/bin/workdesk-chrome"
exec "$HERE/electron/electron" "$HERE/app" $SB "$@"
RUN
chmod +x "$APPDIR/AppRun"
TOOL=${APPIMAGETOOL:-packaging/appimagetool}
[ -x "$TOOL" ] || { curl -fsSL -o "$TOOL" https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage; chmod +x "$TOOL"; }
OUT="WorkDesk-Chrome-v${VERSION}-x86_64.AppImage"
ARCH=x86_64 "$TOOL" --appimage-extract-and-run "$APPDIR" "$OUT"
echo "Built $OUT"
