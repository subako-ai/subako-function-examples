#!/usr/bin/env bash
# Downloads the CPython build this package embeds: libpython and the static
# libraries it links against into cpython/, and the standard library into
# assets/, which Subako mounts at /package. Then the third-party packages and
# fonts the package bundles, into assets/ too.
set -euo pipefail
cd "$(dirname "$0")"

if ! command -v wasmtime >/dev/null; then
    echo "error: wasmtime is needed to precompile the standard library: https://wasmtime.dev" >&2
    exit 1
fi

# CPython for wasm32-wasip1, built by one of its core developers with the WASI
# SDK release named here. The sysroot comes from that same release, for the
# libraries libpython links against: its stub libdl and the emulation of
# signals, getpid and process clocks.
python_version=3.15.0
wasi_sdk=33
python_release="https://github.com/brettcannon/cpython-wasi-build/releases/download/v$python_version"
sysroot="https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-$wasi_sdk/wasi-sysroot-$wasi_sdk.0%2Bm.tar.gz"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The Volume holding assets/ is read-only, so Python can never cache the
# bytecode it compiles; shipping it cuts each call's startup by about 40%.
# The release's own python.wasm compiles it, since the bytecode format is
# specific to 3.15. `unchecked-hash` skips comparing against the source.
compile() {
    wasmtime run --dir assets::/package --env PYTHONHOME=/package cpython/python.wasm \
        -m compileall -q --invalidation-mode unchecked-hash "$@"
}

if [ ! -f cpython/libpython${python_version%.*}.a ] || [ ! -f cpython/python.wasm ] ||
    [ ! -d assets/lib/python${python_version%.*}/__pycache__ ]; then
    curl -fsSL -o "$work/build.zip" "$python_release/_build-python-$python_version-wasi_sdk-$wasi_sdk.zip"
    curl -fsSL -o "$work/python.zip" "$python_release/python-$python_version-wasi_sdk-$wasi_sdk.zip"
    curl -fsSL -o "$work/sysroot.tar.gz" "$sysroot"

    rm -rf cpython assets/lib
    mkdir -p cpython assets
    unzip -q "$work/build.zip" -d "$work/build"
    find "$work/build" -name '*.a' -exec cp {} cpython/ \;
    tar -xzf "$work/sysroot.tar.gz" -C "$work"
    for lib in dl wasi-emulated-signal wasi-emulated-getpid wasi-emulated-process-clocks; do
        cp "$work"/wasi-sysroot-*/lib/wasm32-wasip1/lib$lib.a cpython/
    done
    unzip -q "$work/python.zip" 'lib/*' python.wasm LICENSE -d "$work/python"
    # Kept to precompile bytecode; build.rs links only the .a files.
    cp "$work/python/python.wasm" cpython/
    cp -R "$work/python/lib" assets/lib
    cp "$work/python/LICENSE" assets/LICENSE-CPython
    compile /package/lib
    echo "CPython $python_version in cpython/ and assets/"
fi

# Pure-Python packages from PyPI, pinned by hash, as `name version sha256`.
# pygal draws charts as SVG; its dependency on the importlib_metadata backport
# is met by a shim in src/site-packages, as is its PNG output, by a CairoSVG
# shim over subako.svg_to_png.
wheels=(
    "pygal 3.1.3 c0b9bc2d31df4094c9f65b0969b62571a47b28197aced081b1a9433c3a760f32"
)
mkdir -p assets/site-packages
for wheel in "${wheels[@]}"; do
    read -r name version sha256 <<<"$wheel"
    [ -d "assets/site-packages/$name-$version.dist-info" ] && continue
    rm -rf "assets/site-packages/$name" "assets/site-packages/$name"-*.dist-info
    url=$(curl -fsSL "https://pypi.org/pypi/$name/$version/json" |
        grep -o '"url": *"[^"]*-py3-none-any\.whl"' | head -1 | cut -d'"' -f4)
    curl -fsSL -o "$work/$name.whl" "$url"
    echo "$sha256  $work/$name.whl" | shasum -a 256 -c --quiet
    unzip -q "$work/$name.whl" -d assets/site-packages
    # Its own tests aren't needed at run time.
    rm -rf "assets/site-packages/$name/test"
    echo "fetched $name $version"
done
cp src/site-packages/*.py assets/site-packages/
compile /package/site-packages

# Fonts for subako.svg_to_png: Inter and IBM Plex Mono as static Latin
# instances from Fontsource, and Noto Sans JP for Japanese. All under the SIL
# Open Font License 1.1.
fontsource="https://cdn.jsdelivr.net/fontsource/fonts"
fonts=(
    "$fontsource/inter@5.3.0/latin-400-normal.ttf Inter-Regular.ttf"
    "$fontsource/inter@5.3.0/latin-700-normal.ttf Inter-Bold.ttf"
    "$fontsource/ibm-plex-mono@5.3.0/latin-400-normal.ttf IBMPlexMono-Regular.ttf"
    "$fontsource/ibm-plex-mono@5.3.0/latin-700-normal.ttf IBMPlexMono-Bold.ttf"
    "https://github.com/notofonts/noto-cjk/raw/Sans2.004/Sans/SubsetOTF/JP/NotoSansJP-Regular.otf NotoSansJP-Regular.otf"
)
mkdir -p assets/fonts
for font in "${fonts[@]}"; do
    read -r url file <<<"$font"
    [ -f "assets/fonts/$file" ] && continue
    curl -fsSL -o "assets/fonts/$file.partial" "$url"
    mv "assets/fonts/$file.partial" "assets/fonts/$file"
    echo "fetched $file"
done
