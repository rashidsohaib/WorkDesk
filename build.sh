#!/usr/bin/env bash
# Build everything: frontend -> embedded into the Rust binary -> AppImage.
set -euo pipefail
cd "$(dirname "$0")"
# dist/ is the finished v2.10 frontend (see SOURCE-README.md); no frontend build step
cargo build --release --manifest-path backend/Cargo.toml
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-backend/target} ./packaging/make-appimage.sh
