# typst

A Subako function package that typesets [Typst](https://typst.app) markup or Markdown into a PDF on the Volume, with Typst 0.15 compiled into the function. By default the PDF uses a Subako template: the Subako logo, the subako.ai site's typefaces and its ink-on-paper colors.

## Function

### `render_pdf`

| Input | |
| --- | --- |
| `source` | `{"type": "markdown", "text": "..."}`, `{"type": "typst", "text": "..."}`, or `{"type": "file", "path": "/data/report.md"}` for a `.md` or `.typ` file on the Volume |
| `output` | Volume path for the PDF, such as `/tmp/report.pdf`. Missing directories are created |
| `template` | Optional. `"subako"` (the default) or `"plain"` for Typst's defaults |
| `title` | Optional. The document title: large at the top and in the header of the Subako template, and the PDF's title |
| `preamble` | Optional. Typst code run after the template and before the source, such as `#set page(paper: "us-letter")` |

```json
{
  "source": { "type": "markdown", "text": "# Q3\n\nCalls grew **225%**.\n\n| Quarter | Calls |\n| --- | ---: |\n| Q2 | 3,918 |" },
  "output": "/tmp/q3.pdf",
  "title": "Quarterly report"
}
```

returns

```json
{ "path": "/tmp/q3.pdf", "pages": 1, "bytes": 19492 }
```

with `warnings` when Typst has any. A source that doesn't compile fails the call with Typst's errors as `file:line:column: error: message` (`<input>` for source passed as text), and nothing is written.

## The Subako template

`assets/templates/subako/template.typ`, after the subako.ai site:

- the Subako logo (the site header's lockup, in `logo.svg`) and the document title in the header; `subako.ai` and page numbers in the footer
- Playfair Display headings over Inter text, IBM Plex Mono code, and Noto Sans JP for Japanese and anything else the Latin faces lack
- the site's paper (`#FFFFF6`) page color, ink (`#16221F`) headings, and its sunken and line colors for code, tables and quotes

Typst code can use it directly as well:

```typst
#import "/package/templates/subako/template.typ": subako
#show: subako.with(title: "Quarterly report")
```

## Markdown

Markdown is typeset with [cmarker](https://typst.app/universe/package/cmarker), which the package bundles: CommonMark with GitHub tables, strikethrough and task lists. Images in it resolve from the Markdown file's directory, or from `/tmp` for text. Math is left as text.

## Files, fonts and packages

- **Paths:** the project root is the Volume's filesystem, so `/data/chart.png` means that file; relative paths resolve from the source file's directory, or from `/tmp` for text.
- **Fonts:** the template's, plus Typst's own (Libertinus Serif, New Computer Modern, DejaVu Sans Mono).
- **Packages:** `@preview` packages are downloaded from packages.typst.org through the Subako worker, when the function's network setting allows that host, and kept in `/tmp/.typst-packages` for later calls.

## How it works

- `src/main.rs` builds a main Typst file in memory that applies the template and then includes the source, so errors in the source keep their own line numbers, and writes the PDF with `typst-pdf`.
- `src/world.rs` is the `World` Typst compiles against: the filesystem, files held in memory, fonts, and the date.
- `src/packages.rs` finds packages in the assets or the cache, or downloads and unpacks them. It unpacks with its own loop, because `tar`'s checks need `fs::canonicalize`, which WASI lacks.
- `fetch-assets.sh` downloads what `assets/` ships but the repository doesn't keep: cmarker and the template's fonts.

A call takes about 450 ms in wasmtime on an M-series Mac. The bundle is about 23 MB: the module is 35 MB uncompressed, most of it Typst's own data, and the two Noto Sans JP weights are 9 MB.

## Build

```sh
./fetch-assets.sh                                      # once; package.sh also runs it
cargo test                                             # natively
cargo test --target wasm32-wasip1                      # in wasmtime
./package.sh                                           # dist/typst-function.tar.gz
subako function create dist/typst-function.tar.gz
```

Locally, `tests/subako_http_stub.wat` stands in for the worker's `subako_http` import and fails every request:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"render_pdf","arguments":{"source":{"type":"markdown","text":"# Hello"},"output":"/tmp/hello.pdf"}}}' |
    wasmtime run --preload subako_http=tests/subako_http_stub.wat --argv0 typst \
    --dir assets::/package --dir /path/to/scratch::/tmp \
    target/wasm32-wasip1/release/typst-function.wasm
```

## Licenses

Typst is under the Apache License 2.0 and cmarker under the MIT license. Playfair Display, Inter, IBM Plex Mono and Noto Sans JP are under the SIL Open Font License 1.1. The Subako logo belongs to Subako.
