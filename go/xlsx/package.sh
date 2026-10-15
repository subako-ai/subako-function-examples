#!/usr/bin/env bash
# Builds dist/xlsx.tar.gz, the bundle Subako takes: module.wasm and
# subako-package.json.
set -euo pipefail
cd "$(dirname "$0")"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
GOOS=wasip1 GOARCH=wasm go build -trimpath -o "$stage/module.wasm" .
cp subako-package.json "$stage/"

mkdir -p dist
# COPYFILE_DISABLE keeps macOS's tar from adding `._` entries Subako refuses.
COPYFILE_DISABLE=1 tar --format=ustar -czf dist/xlsx.tar.gz -C "$stage" subako-package.json module.wasm
echo "dist/xlsx.tar.gz"
