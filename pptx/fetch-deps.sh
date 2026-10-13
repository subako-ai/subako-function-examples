#!/usr/bin/env bash
# Downloads what building the package needs but the repository doesn't keep:
# PptxGenJS, which the module embeds, into vendor/, and the WASI SDK, whose
# clang compiles QuickJS's C sources for wasm32-wasip1, into wasi-sdk/.
set -euo pipefail
cd "$(dirname "$0")"

pptxgenjs=4.0.1
wasi_sdk=33

if [ ! -f vendor/pptxgen.bundle.js ]; then
    mkdir -p vendor
    work=$(mktemp -d)
    curl -fsSL "https://registry.npmjs.org/pptxgenjs/-/pptxgenjs-$pptxgenjs.tgz" | tar -xz -C "$work"
    cp "$work/package/dist/pptxgen.bundle.js" "$work/package/LICENSE" vendor/
    rm -rf "$work"
    echo "fetched PptxGenJS $pptxgenjs"
fi

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
