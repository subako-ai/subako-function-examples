#!/usr/bin/env bash
# Downloads what the package ships in assets/ but the repository doesn't keep,
# and Subako mounts at /package: the Typst packages it bundles, and the fonts
# of the Subako template.
set -euo pipefail
cd "$(dirname "$0")"

# cmarker typesets Markdown. Other @preview packages are downloaded at run
# time, when the function's network setting allows packages.typst.org.
packages=("preview/cmarker/0.1.10")
for package in "${packages[@]}"; do
    dir="assets/packages/$package"
    [ -f "$dir/typst.toml" ] && continue
    IFS=/ read -r namespace name version <<<"$package"
    mkdir -p "$dir"
    curl -fsSL "https://packages.typst.org/$namespace/$name-$version.tar.gz" | tar -xz -C "$dir"
    echo "fetched @$namespace/$name:$version"
done

# The subako.ai site's typefaces, as static Latin instances from Fontsource,
# and Noto Sans JP for Japanese. All under the SIL Open Font License 1.1.
fontsource="https://cdn.jsdelivr.net/fontsource/fonts"
fonts=(
    "$fontsource/playfair-display@5.3.0/latin-400-normal.ttf PlayfairDisplay-Regular.ttf"
    "$fontsource/playfair-display@5.3.0/latin-400-italic.ttf PlayfairDisplay-Italic.ttf"
    "$fontsource/inter@5.3.0/latin-400-normal.ttf Inter-Regular.ttf"
    "$fontsource/inter@5.3.0/latin-400-italic.ttf Inter-Italic.ttf"
    "$fontsource/inter@5.3.0/latin-700-normal.ttf Inter-Bold.ttf"
    "$fontsource/inter@5.3.0/latin-700-italic.ttf Inter-BoldItalic.ttf"
    "$fontsource/ibm-plex-mono@5.3.0/latin-400-normal.ttf IBMPlexMono-Regular.ttf"
    "$fontsource/ibm-plex-mono@5.3.0/latin-700-normal.ttf IBMPlexMono-Bold.ttf"
    "https://github.com/notofonts/noto-cjk/raw/Sans2.004/Sans/SubsetOTF/JP/NotoSansJP-Regular.otf NotoSansJP-Regular.otf"
    "https://github.com/notofonts/noto-cjk/raw/Sans2.004/Sans/SubsetOTF/JP/NotoSansJP-Bold.otf NotoSansJP-Bold.otf"
)
mkdir -p assets/fonts
for font in "${fonts[@]}"; do
    read -r url file <<<"$font"
    [ -f "assets/fonts/$file" ] && continue
    curl -fsSL -o "assets/fonts/$file.partial" "$url"
    mv "assets/fonts/$file.partial" "assets/fonts/$file"
    echo "fetched $file"
done
