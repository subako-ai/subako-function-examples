# Subako function examples

Example [Subako](https://subako.ai) function packages, grouped by the language they are written in. Each example is a standalone project that builds the bundle Subako takes: `module.wasm`, `subako-package.json` and any `assets/`.

## Rust

Written with [subako-function-sdk](https://crates.io/crates/subako-function-sdk), each crate starting from [subako-function-template-rust](https://github.com/subako-ai/subako-function-template-rust).

| Example | What it does |
| --- | --- |
| [anydoc](rust/anydoc/) | Converts office documents, EPUB, CSV and text-based PDFs on the Volume to Markdown with [anydoc](https://github.com/firecrawl/anydoc) |
| [python](rust/python/) | Runs Python code or scripts with CPython 3.15 compiled to WebAssembly, with HTTPS through `urllib.request` and charts with pygal (SVG or PNG) |
| [bashkit](rust/bashkit/) | Runs bash scripts with [Bashkit](https://github.com/everruns/bashkit): pipes, about 145 built-in commands, the Volume's real files, and a limited `curl` |
| [typst](rust/typst/) | Typesets Typst or Markdown into PDF with [Typst](https://typst.app), in a Subako-branded template by default |
| [quickjs](rust/quickjs/) | Runs JavaScript in [QuickJS](https://github.com/quickjs-ng/quickjs), with `fetch` through the worker and the Volume's files |
| [pptx](rust/pptx/) | Builds PowerPoint decks from JavaScript with [PptxGenJS](https://gitbrent.github.io/PptxGenJS/) in QuickJS, with Subako slide masters |
| [html](rust/html/) | Draws HTML pages to PNG with [Blitz](https://github.com/DioxusLabs/blitz), so agents can check the layout of HTML they write, and names elements that stick out past the viewport |
| [github](rust/github/) | Reads GitHub for questions about a repository: any read-only REST API path, files and directories, and whole repositories downloaded to the Volume, with an optional token from a Subako vault |

### Build a Rust example

```sh
cd rust/anydoc
cargo test
./package.sh                                   # dist/<name>.tar.gz
subako function create dist/<name>.tar.gz
```

`package.sh` needs the `wasm32-wasip1` target, which each example's `rust-toolchain.toml` installs through rustup.

A module is an MCP server over stdio, as [subako-function-sdk](https://crates.io/crates/subako-function-sdk#the-protocol) describes. To try one under [wasmtime](https://wasmtime.dev), pipe it a `tools/call` request on one line, as each example's README shows; it answers with a JSON-RPC response on stdout.

## Go

Written with [subako-function-sdk-go](https://github.com/subako-ai/subako-function-sdk-go) and the [official MCP Go SDK](https://github.com/modelcontextprotocol/go-sdk), each module starting from [subako-function-template-go](https://github.com/subako-ai/subako-function-template-go).

| Example | What it does |
| --- | --- |
| [xlsx](go/xlsx/) | Writes and reads Excel workbooks on the Volume with [excelize](https://github.com/qax-os/excelize): formulas, a frozen header row, column widths and number formats, and cells read back with formulas calculated |

### Build a Go example

```sh
cd go/xlsx
go test ./...
./package.sh                                   # dist/<name>.tar.gz
subako function create dist/<name>.tar.gz
```

Go examples need Go 1.25 or later.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
