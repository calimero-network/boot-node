#!/usr/bin/env bash
set -euo pipefail

# Build boot-node for Linux (x86_64-unknown-linux-musl)
#
# Requirements:
#   1. Install musl cross-compiler toolchain:
#      brew install filosottile/musl-cross/musl-cross
#
#   2. Add the Rust target:
#      rustup target add x86_64-unknown-linux-musl

TARGET="x86_64-unknown-linux-musl"
OUTPUT_DIR="target/linux"

if ! command -v x86_64-linux-musl-gcc &> /dev/null; then
    echo "Error: x86_64-linux-musl-gcc not found"
    echo "Install with: brew install filosottile/musl-cross/musl-cross"
    exit 1
fi

echo "Adding target $TARGET..."
rustup target add "$TARGET"

echo "Building for $TARGET..."
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc \
CC_x86_64_unknown_linux_musl=x86_64-linux-musl-gcc \
cargo build --release --target "$TARGET"

mkdir -p "$OUTPUT_DIR"
cp "target/$TARGET/release/boot-node" "$OUTPUT_DIR/"

echo ""
echo "Build complete: $OUTPUT_DIR/boot-node"
