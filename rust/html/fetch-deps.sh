#!/usr/bin/env bash
# Downloads what the build and the bundle need but the repository doesn't
# keep: Blitz's DOM crate with a small patch, into vendor/, and the fonts
# pages are drawn with, into assets/fonts, which Subako mounts at
# /package/fonts.
set -euo pipefail
cd "$(dirname "$0")"

# blitz-dom draws text in SVG with the system fonts, and WASI has none, so
# the patch lets the function hand it the bundled ones. Cargo.toml points
# blitz-dom at this copy.
blitz_dom=0.3.0-beta.3
blitz_dom_sha256=53e58a506e20e90b69ab8b03f933c125237361c31b0dae0e7e149c5bcbf9b28a
if [ ! -f vendor/blitz-dom/.patched ] || [ "$(cat vendor/blitz-dom/.patched)" != "$blitz_dom" ]; then
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    curl -fsSL -o "$work/blitz-dom.crate" "https://static.crates.io/crates/blitz-dom/blitz-dom-$blitz_dom.crate"
    echo "$blitz_dom_sha256  $work/blitz-dom.crate" | shasum -a 256 -c --quiet
    tar -xzf "$work/blitz-dom.crate" -C "$work"
    patch -s -d "$work/blitz-dom-$blitz_dom" -p1 <patches/blitz-dom-svg-fonts.patch
    rm -rf vendor/blitz-dom
    mkdir -p vendor
    mv "$work/blitz-dom-$blitz_dom" vendor/blitz-dom
    echo "$blitz_dom" >vendor/blitz-dom/.patched
    echo "fetched blitz-dom $blitz_dom and patched it"
fi

# Static instances from Fontsource (Latin) and Noto Sans JP for Japanese, all
# under the SIL Open Font License 1.1. CSS generic families map to them:
# sans-serif and system-ui to Inter, serif to Source Serif 4, monospace to IBM
# Plex Mono; characters a font lacks come from any font that has them.
fontsource="https://cdn.jsdelivr.net/fontsource/fonts"
noto="https://github.com/notofonts/noto-cjk/raw/Sans2.004/Sans/SubsetOTF/JP"
fonts=(
    "$fontsource/inter@5.3.0/latin-400-normal.ttf Inter-Regular.ttf"
    "$fontsource/inter@5.3.0/latin-400-italic.ttf Inter-Italic.ttf"
    "$fontsource/inter@5.3.0/latin-700-normal.ttf Inter-Bold.ttf"
    "$fontsource/inter@5.3.0/latin-700-italic.ttf Inter-BoldItalic.ttf"
    "$fontsource/source-serif-4@5.3.0/latin-400-normal.ttf SourceSerif4-Regular.ttf"
    "$fontsource/source-serif-4@5.3.0/latin-400-italic.ttf SourceSerif4-Italic.ttf"
    "$fontsource/source-serif-4@5.3.0/latin-700-normal.ttf SourceSerif4-Bold.ttf"
    "$fontsource/ibm-plex-mono@5.3.0/latin-400-normal.ttf IBMPlexMono-Regular.ttf"
    "$fontsource/ibm-plex-mono@5.3.0/latin-700-normal.ttf IBMPlexMono-Bold.ttf"
    "$noto/NotoSansJP-Regular.otf NotoSansJP-Regular.otf"
    "$noto/NotoSansJP-Bold.otf NotoSansJP-Bold.otf"
)
mkdir -p assets/fonts
for font in "${fonts[@]}"; do
    read -r url file <<<"$font"
    [ -f "assets/fonts/$file" ] && continue
    curl -fsSL -o "assets/fonts/$file.partial" "$url"
    mv "assets/fonts/$file.partial" "assets/fonts/$file"
    echo "fetched $file"
done
