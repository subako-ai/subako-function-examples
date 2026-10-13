#!/usr/bin/env bash
# Downloads the WASI SDK into wasi-sdk/: its clang compiles QuickJS's C
# sources for wasm32-wasip1. The repository doesn't keep it.
set -euo pipefail
cd "$(dirname "$0")"

wasi_sdk=33

if [ ! -x wasi-sdk/bin/clang ]; then
    case "$(uname -s)-$(uname -m)" in
        Darwin-arm64) platform=arm64-macos ;;
        Darwin-x86_64) platform=x86_64-macos ;;
        Linux-aarch64 | Linux-arm64) platform=arm64-linux ;;
        Linux-x86_64) platform=x86_64-linux ;;
        *) echo "error: no WASI SDK build for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
    esac
    mkdir -p wasi-sdk
    curl -fsSL "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-$wasi_sdk/wasi-sdk-$wasi_sdk.0-$platform.tar.gz" |
        tar -xz -C wasi-sdk --strip-components 1
    echo "fetched WASI SDK $wasi_sdk ($platform)"
fi
