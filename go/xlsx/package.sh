#!/usr/bin/env bash
# Builds dist/<name>.tar.gz, the bundle Subako takes: module.wasm,
# subako-package.json, and assets/ when it exists.
set -euo pipefail
cd "$(dirname "$0")"

name=$(grep -o '"name": *"[^"]*"' subako-package.json | head -1 | cut -d'"' -f4)

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
GOOS=wasip1 GOARCH=wasm go build -trimpath -o "$stage/module.wasm" .
cp subako-package.json "$stage/"

entries=(subako-package.json module.wasm)
if [ -d assets ]; then
    cp -R assets "$stage/assets"
    entries+=(assets)
fi
mkdir -p dist
# COPYFILE_DISABLE keeps macOS's tar from adding `._` entries Subako refuses.
COPYFILE_DISABLE=1 tar --format=ustar -czf "dist/$name.tar.gz" -C "$stage" "${entries[@]}"
echo "dist/$name.tar.gz"
