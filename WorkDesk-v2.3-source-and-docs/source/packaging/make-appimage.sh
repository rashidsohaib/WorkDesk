#!/usr/bin/env bash
# Build WorkDesk-Chrome-<version>-x86_64.AppImage from the release binary.
# Needs: appimagetool (set APPIMAGETOOL=/path/to/appimagetool, or it is downloaded), a release build of the backend.
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=$(node -p "require('./package.json').version")
BIN=${BIN:-${CARGO_TARGET_DIR:-backend/target}/release/workdesk-chrome}
[ -x "$BIN" ] || { echo "Build the backend first: cargo build --release --manifest-path backend/Cargo.toml"; exit 1; }
APPDIR=packaging/WorkDesk-Chrome.AppDir
rm -rf "$APPDIR"; mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib"
install -m755 "$BIN" "$APPDIR/usr/bin/workdesk-chrome"
cp packaging/workdesk-chrome.desktop "$APPDIR/"
cp packaging/workdesk-chrome.png "$APPDIR/workdesk-chrome.png"
# OpenSSL is the only non-system library the binary needs; ship it so the AppImage runs on any recent distro.
for lib in libssl.so.3 libcrypto.so.3; do
  p=$(ldd "$BIN" | awk -v l="$lib" '$1==l {print $3}')
  [ -n "$p" ] && cp -L "$p" "$APPDIR/usr/lib/"
done
cat > "$APPDIR/AppRun" <<'RUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
export LD_LIBRARY_PATH="$HERE/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$HERE/usr/bin/workdesk-chrome" "$@"
RUN
chmod +x "$APPDIR/AppRun"
TOOL=${APPIMAGETOOL:-}
if [ -z "$TOOL" ]; then
  TOOL=packaging/appimagetool
  [ -x "$TOOL" ] || { curl -fsSL -o "$TOOL" https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage; chmod +x "$TOOL"; }
fi
OUT="WorkDesk-Chrome-v${VERSION}-x86_64.AppImage"
ARCH=x86_64 "$TOOL" --appimage-extract-and-run "$APPDIR" "$OUT"
echo "Built $OUT"
