#!/usr/bin/env bash
# Builds dist/<name>.tar.gz, the bundle Subako takes: module.wasm, the
# subako-package.json the program prints, and assets/, which holds the
# fonts pages are drawn with.
set -euo pipefail
cd "$(dirname "$0")"

# First: Cargo.toml points blitz-dom at what it fetches.
./fetch-deps.sh

name=$(cargo metadata --no-deps --format-version 1 | grep -o '"name":"[^"]*"' | head -1 | cut -d'"' -f4)

cargo build --release --target wasm32-wasip1
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp "target/wasm32-wasip1/release/$name.wasm" "$stage/module.wasm"
# The same crate built for this machine prints the manifest.
cargo run --quiet -- --subako-manifest >"$stage/subako-package.json"

entries=(subako-package.json module.wasm)
if [ -d assets ]; then
    cp -R assets "$stage/assets"
    entries+=(assets)
fi
mkdir -p dist
# COPYFILE_DISABLE keeps macOS's tar from adding `._` entries Subako refuses.
COPYFILE_DISABLE=1 tar --format=ustar -czf "dist/$name.tar.gz" -C "$stage" "${entries[@]}"
echo "dist/$name.tar.gz"
