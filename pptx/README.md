# pptx

A Subako function package that builds PowerPoint decks from JavaScript. The agent writes a script against [PptxGenJS](https://gitbrent.github.io/PptxGenJS/), which runs in [QuickJS](https://github.com/quickjs-ng/quickjs) compiled into the function, and the `.pptx` lands on the Volume. Subako slide masters, with the logo and the subako.ai site's colors and fonts, are ready to use.

## Function

### `create_pptx`

| Input | |
| --- | --- |
| `source` | `{"type": "code", "code": "..."}`, or `{"type": "file", "path": "/volume/data/deck.js"}` for a script on the Volume |
| `output` | Volume path for the deck, such as `/volume/tmp/deck.pptx`. Missing directories are created |

```json
{
  "source": { "type": "code", "code": "const s = pptx.addSlide({ masterName: 'SUBAKO_TITLE' });\ns.addText('Quarterly report', { placeholder: 'title' });\ns.addText('Q3 2026', { placeholder: 'subtitle' });" },
  "output": "/volume/tmp/q3.pptx"
}
```

returns

```json
{ "path": "/volume/tmp/q3.pptx", "slides": 1, "bytes": 147264 }
```

with `logs` when the script used `console`. A script that throws fails the call with the error, its line in the script, and the logs so far; nothing is written.

## Writing scripts

The script is the body of an async function, so it can `await`. It gets:

- `pptx`: a `PptxGenJS` instance at 16:9 (10 × 5.625 in). Add slides with `pptx.addSlide()` and fill them with [`addText`](https://gitbrent.github.io/PptxGenJS/docs/api-text/), [`addTable`](https://gitbrent.github.io/PptxGenJS/docs/api-tables/), [`addChart`](https://gitbrent.github.io/PptxGenJS/docs/api-charts/), [`addShape`](https://gitbrent.github.io/PptxGenJS/docs/api-shapes/) and [`addImage`](https://gitbrent.github.io/PptxGenJS/docs/api-images/). The deck is written when the script ends; calling `writeFile` would try to start a browser download. A script may instead `return` a `PptxGenJS` it made itself.
- `image(source)`: image data for `addImage({ data })` or a slide background, from a PNG, JPEG or GIF on the Volume or an https URL fetched through the Subako worker. SVG isn't supported: PptxGenJS turns SVG into PNG with a browser canvas, which isn't here.
- `readText(path)`: a text file on the Volume, such as CSV or JSON to chart.
- `console.log`, `warn` and `error`, which end up in `logs`.

Relative paths resolve from `/volume/tmp`, or from the script's directory for a script file.

## Subako masters

| Master | |
| --- | --- |
| `SUBAKO_TITLE` | Ink background, the logo in paper, placeholders `title` and `subtitle` |
| `SUBAKO_SECTION` | Canvas background, a large `title` over a rule, the slide number |
| `SUBAKO_CONTENT` | Paper background, the logo and a rule up top, placeholders `title` and `body`, `subako.ai` and the slide number in the footer |

```js
const slide = pptx.addSlide({ masterName: "SUBAKO_CONTENT" });
slide.addText("Calls by quarter", { placeholder: "title" });
slide.addChart(pptx.ChartType.bar, [{ name: "Calls", labels: ["Q1", "Q2"], values: [1204, 3918] }],
  { x: 0.5, y: 1.7, w: 9, h: 3.3, chartColors: [subako.colors.ink] });
```

`subako.colors` holds the site's palette as hex (`ink`, `strong`, `body`, `muted`, `paper`, `canvas`, `sunken`, `line`, `action`), and `subako.fonts` its typefaces: Playfair Display for headings, Inter for text, IBM Plex Mono for code. A deck names its fonts rather than embedding them, so they show as designed where they're installed and in Google Slides, which has all three; elsewhere PowerPoint substitutes its own. The logo is in `brand/`, rendered from the site header's SVG.

## How it works

- `src/main.rs` runs a fresh QuickJS (through [rquickjs](https://github.com/DelSkayn/rquickjs)) for each call: `src/prelude.js`, the PptxGenJS browser bundle, `src/setup.js`, then the script, after which it writes the deck and runs promise jobs and timers until nothing is left.
- `src/prelude.js` supplies what PptxGenJS needs that QuickJS lacks, which is only `setTimeout` (run in order, without delays), plus `console`, `image` and `readText`.
- `src/setup.js` creates `pptx` and defines the masters.

A script is stopped after 60 seconds, and the engine may use 1 GiB of memory. A call takes about 400 ms in wasmtime on an M-series Mac, and the bundle is about 0.6 MB.

## Build

QuickJS is C, so building for `wasm32-wasip1` needs the [WASI SDK](https://github.com/WebAssembly/wasi-sdk)'s clang. `fetch-deps.sh` downloads it (about 180 MB) into `wasi-sdk/`, and PptxGenJS into `vendor/`; `.cargo/config.toml` points the build at that clang.

```sh
./fetch-deps.sh                                        # once; package.sh also runs it
cargo test                                             # natively
cargo test --target wasm32-wasip1                      # in wasmtime
./package.sh                                           # dist/pptx-function.tar.gz
subako function create dist/pptx-function.tar.gz
```

Locally, `tests/subako_http_stub.wat` stands in for the worker's `subako_http` import and fails every request:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"create_pptx","arguments":{"source":{"type":"code","code":"pptx.addSlide().addText(\"Hi\", { x: 1, y: 1, w: 4, h: 1 });"},"output":"/volume/tmp/hi.pptx"}}}' |
    wasmtime run --preload subako_http=tests/subako_http_stub.wat --argv0 pptx \
    --dir /path/to/scratch::/volume/tmp target/wasm32-wasip1/release/pptx-function.wasm
```

## Licenses

PptxGenJS is under the MIT license, QuickJS and rquickjs under the MIT license. The Subako logo belongs to Subako.
