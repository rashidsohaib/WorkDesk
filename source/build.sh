#!/usr/bin/env bash
# Build everything: frontend -> embedded into the Rust binary -> AppImage.
set -euo pipefail
cd "$(dirname "$0")"
npm install
npm run check:js
npm run build
cargo build --release --manifest-path backend/Cargo.toml
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-backend/target} ./packaging/make-appimage.sh
