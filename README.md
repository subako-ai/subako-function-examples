# Subako function examples

Example [Subako](https://subako.ai) function packages, each one a standalone crate built from [subako-function-template-rust](https://github.com/subako-ai/subako-function-template-rust).

| Example | What it does |
| --- | --- |
| [anydoc](anydoc/) | Converts office documents, EPUB, CSV and text-based PDFs on the Volume to Markdown with [anydoc](https://github.com/firecrawl/anydoc) |
| [python](python/) | Runs Python code or scripts with CPython 3.15 compiled to WebAssembly, with HTTPS through `urllib.request` and charts with pygal (SVG or PNG) |
| [bashkit](bashkit/) | Runs bash scripts with [Bashkit](https://github.com/everruns/bashkit): pipes, about 145 built-in commands, the Volume's real files, and a limited `curl` |
| [typst](typst/) | Typesets Typst or Markdown into PDF with [Typst](https://typst.app), in a Subako-branded template by default |
| [quickjs](quickjs/) | Runs JavaScript in [QuickJS](https://github.com/quickjs-ng/quickjs), with `fetch` through the worker and the Volume's files |
| [pptx](pptx/) | Builds PowerPoint decks from JavaScript with [PptxGenJS](https://gitbrent.github.io/PptxGenJS/) in QuickJS, with Subako slide masters |
| [html](html/) | Draws HTML pages to PNG with [Blitz](https://github.com/DioxusLabs/blitz), so agents can check the layout of HTML they write, and names elements that stick out past the viewport |
| [github](github/) | Reads GitHub for questions about a repository: any read-only REST API path, files and directories, and whole repositories downloaded to the Volume, with an optional token from a Subako vault |

## Build an example

```sh
cd anydoc
cargo test
./package.sh                                   # dist/<name>.tar.gz
subako function create dist/<name>.tar.gz
```

`package.sh` needs the `wasm32-wasip1` target, which each example's `rust-toolchain.toml` installs through rustup.

A module is an MCP server over stdio, as [subako-function-sdk](https://crates.io/crates/subako-function-sdk) describes. To try one under [wasmtime](https://wasmtime.dev), pipe it a `tools/call` request on one line, as each example's README shows; it answers with a JSON-RPC response on stdout.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
