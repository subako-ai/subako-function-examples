# html

A Subako function package that draws HTML pages to PNG, so an agent can look at the HTML it wrote: check the layout at a desktop width and a phone's, and catch what sticks out or doesn't load. It uses [Blitz](https://github.com/DioxusLabs/blitz), an HTML and CSS engine built on Stylo, Firefox's CSS engine, drawing on the CPU. No browser is involved.

## Function

### `render_html`

| Input | |
| --- | --- |
| `source` | `{"type": "file", "path": "/data/site/index.html"}` for an HTML file on the Volume, whose relative URLs resolve from its directory, or `{"type": "html", "html": "..."}`, whose relative URLs resolve from `/tmp` |
| `output` | Volume path for the PNG, such as `/tmp/page.png`. Missing directories are created |
| `width` | Optional. The viewport's width in CSS pixels; 1280 by default, 390 for a phone |
| `height` | Optional. The viewport's height, for `vh` units and `full_page: false`; 800 by default |
| `scale` | Optional. Device pixels per CSS pixel, 0.1 to 4; 2 draws a sharper image twice the size |
| `full_page` | Optional. `true` (the default) captures the whole page, `false` only the viewport |
| `color_scheme` | Optional. The `prefers-color-scheme` the page sees: `"light"` (the default) or `"dark"` |

```json
{
  "source": { "type": "file", "path": "/data/report/index.html" },
  "output": "/data/report/phone.png",
  "width": 390
}
```

returns

```json
{
  "output": "/data/report/phone.png",
  "image": { "width": 390, "height": 3242 },
  "page": { "width": 449, "height": 3242 },
  "overflowing": ["section.panel.full (right edge at 449px)", "section.panel.full (right edge at 449px)"],
  "not_loaded": [],
  "notes": []
}
```

That page was an agent's report whose grid column grew to fit a table with `white-space: nowrap` on a phone. Chrome draws it the same way.

The result:

- `page` is the page's full size in CSS pixels. A width over the viewport's means the page scrolls sideways.
- `overflowing` names the outermost elements whose right edge is past the viewport. Elements inside a box that scrolls or clips (`overflow-x: auto` or `hidden`) aren't counted.
- `not_loaded` lists the stylesheets, images and fonts the page asked for but didn't get, with the reason.
- `notes` says what else to know: that `<script>` elements didn't run, or that a very long page was cut. Images stop at 16,384 CSS pixels high and at 40 million pixels.

## What it draws

Most of modern CSS layout and painting works: flexbox, grid, media queries, CSS variables, `calc()` and `clamp()`, borders and border radius, shadows, gradients, transforms, `@font-face` web fonts, and SVG, PNG, JPEG, GIF and WebP images, inline `<svg>` included.

It runs no JavaScript, so content that scripts would create or change is missing. Blitz is in beta, and some CSS still comes out differently from a browser. Two known cases:

- `margin` on inline elements, such as links in a row
- borders on table cells with `border-collapse: collapse`

Without a `font-family` of its own a page gets these bundled fonts:

- **Inter** for `sans-serif` and `system-ui`
- **Source Serif 4** for `serif`, and for families the page names but that aren't bundled, as a browser's default font would be
- **IBM Plex Mono** for `monospace`
- **Noto Sans JP** for Japanese

There are no system fonts in WebAssembly, so `Arial` or `Helvetica` resolves the same way.

Resources load:

- from the Volume
- from `data:` URLs
- from `https:` URLs through the Subako worker, when the function's network setting allows the host

`http:` URLs are refused.

A full-page 1280-pixel render of a two-screen page takes about 0.25 s once the module is compiled (on an M-series Mac in wasmtime).

## How it works

`src/main.rs` parses the page with Blitz into a document, then resolves its styles and layout. Between passes it loads the resources the page asked for, through `src/net.rs`. It then paints the document with `anyrender_vello_cpu` into an RGBA buffer and encodes it as PNG. It walks the laid-out tree to find what sticks out.

`src/fonts.rs` loads the fonts in `assets/fonts` into both of Blitz's text engines:

- Parley, for the page's text
- usvg, for text in SVG images

SVG text needs one change to Blitz. `blitz-dom` builds its SVG font list from the system fonts, which on WASI is an empty list. That empty list was the cause of SVG charts losing all their text. `patches/blitz-dom-svg-fonts.patch` adds `blitz_dom::util::set_svg_font_db`, and `fetch-deps.sh` downloads `blitz-dom` from crates.io, checks its hash, and applies the patch in `vendor/`. `Cargo.toml` points at that copy through `[patch.crates-io]`.

## Build

```sh
./fetch-deps.sh                                # once; package.sh also runs it
cargo test                                     # the tests, on this machine
cargo test --target wasm32-wasip1              # the same tests in wasmtime
./package.sh                                   # dist/html-function.tar.gz
subako function create dist/html-function.tar.gz
```

`fetch-deps.sh` has to run before any `cargo` command, because `Cargo.toml` refers to the patched crate in `vendor/`. wasmtime has no `subako_http` import, so `tests/subako_http_stub.wat` stands in for it. The tests pass a fake worker to `render` instead.

To try the module directly:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"render_html","arguments":{"source":{"type":"html","html":"<h1 style=\"color:teal\">Hello</h1>"},"output":"/tmp/hello.png","width":400,"height":200}}}' |
    wasmtime run --preload subako_http=tests/subako_http_stub.wat --argv0 html \
    --dir assets::/package --dir target::/tmp target/wasm32-wasip1/release/html-function.wasm
```

The fonts are under the SIL Open Font License 1.1. Blitz is under the MIT or Apache 2.0 license.
