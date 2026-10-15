# python

A Subako function package that runs Python: real CPython 3.15, compiled to WebAssembly and embedded in a Rust function. It runs code passed as a string or a script file on the Volume.

## Function

### `run_python`

| Input | |
| --- | --- |
| `source` | `{"type": "code", "code": "..."}` runs the string, like `python -c`. `{"type": "file", "path": "/volume/data/script.py"}` runs a script on the Volume, like `python script.py`, with its directory first on `sys.path` so it can import modules next to it |
| `args` | Optional. Command-line arguments, as `sys.argv[1:]` |

```json
{ "source": { "type": "code", "code": "import sys\nprint(sys.version)" } }
```

```json
{ "source": { "type": "file", "path": "/volume/data/report.py" }, "args": ["--month", "2026-09"] }
```

The result has what the script printed to `stdout` and `stderr`, and an `outcome`:

| `outcome` | |
| --- | --- |
| `{"type": "completed"}` | Ran to the end, or called `sys.exit()` with 0 |
| `{"type": "exited", "code": 3}` | Called `sys.exit()` with a nonzero code |
| `{"type": "raised", "exception": "ValueError: nope", "traceback": "..."}` | An exception escaped it; the traceback starts at the script's own frames |

An exception or a nonzero exit is still a successful call, so the agent keeps what the script printed before it failed. Each call is a fresh interpreter: nothing carries over between calls but files on the Volume.

## What's available

The standard library, except what needs native libraries the build leaves out or that WASI has no equivalent for:

- compression: `zlib`, `gzip`, `bz2`, `lzma` (`zipfile` imports, but only reads and writes uncompressed archives)
- `sqlite3`, `ssl`, `ctypes`
- `subprocess`, `threading`, sockets, and the `asyncio` event loop

Third-party packages have to be pure Python, since WASI can't load native extension modules; anything with compiled code (numpy, pandas, matplotlib) would have to be built into the interpreter itself. One is installed, for charts.

## Charts

[pygal](https://www.pygal.org) draws charts as SVG: bar, line, pie, XY, box, radar, treemap, gauge and more.

```python
import pygal

chart = pygal.Bar(title="Revenue by quarter")
chart.x_labels = ["Q1", "Q2", "Q3", "Q4"]
chart.add("2025", [3.1, 4.8, 4.2, 6.9])
chart.add("2026", [4.0, 5.5, 6.1, 8.3])
chart.render_to_file("/volume/data/revenue.svg")
chart.render_to_png("/volume/data/revenue.png")
```

pygal makes PNGs with CairoSVG, which needs native Cairo, so a small stand-in under that name draws them with [resvg](https://github.com/linebender/resvg) instead, built into the function. Only `cairosvg.svg2png` is there. The `subako` module has the same drawing for any SVG:

```python
import subako

width, height = subako.svg_to_png(svg_text, "/volume/data/chart.png", scale=2)
```

Text uses the fonts bundled in `assets/fonts`: Inter for sans-serif, serif and every family that isn't bundled (so pygal's default Consolas becomes IBM Plex Mono, as `monospace`), IBM Plex Mono for monospace, and Noto Sans JP for Japanese. Interactive features of pygal's SVG, such as tooltips, need its JavaScript from kozea.github.io when viewed in a browser.

## Network

WASI has no sockets, so the worker makes HTTP requests on the function's behalf, through the SDK's [`http`](https://docs.rs/subako-function-sdk/latest/subako_function_sdk/http/) import. Requests are HTTPS only, reach only the hosts the function's network setting in Subako allows, and carry any stored credentials for their own origin. Python gets at it two ways.

`urllib.request` works unchanged:

```python
import json, urllib.request

with urllib.request.urlopen("https://api.github.com/repos/python/cpython") as r:
    print(json.load(r)["stargazers_count"])
```

Every urllib response body passes through a temporary file in `/volume/tmp`, so binary and large bodies work. Error statuses raise `urllib.error.HTTPError` as usual; a request the worker refuses raises `urllib.error.URLError` whose reason names why, such as `host_not_allowed: ...`. `http.client` and anything else that opens sockets itself still fails.

The `subako` module makes the same requests directly:

```python
import subako

r = subako.request("https://api.example.com/items", method="POST", json={"name": "x"})
print(r.status, r.headers["content-type"], r.json())

# Bodies that aren't UTF-8 text, or are over 256 KiB, have to go to a file.
r = subako.request("https://example.com/report.pdf", save_to="/volume/data/report.pdf")
print(r.path, r.bytes)
```

`subako.request` takes `method`, `headers`, `body` (`str` or `bytes`), `json`, `save_to`, `follow_redirects` and `timeout` (seconds), and raises `subako.HTTPError`, with a `kind` such as `host_not_allowed`, `timeout` or `too_large`, when the worker can't complete a request.

## How it works

- `fetch-cpython.sh` downloads a pinned CPython build for `wasm32-wasip1` from [brettcannon/cpython-wasi-build](https://github.com/brettcannon/cpython-wasi-build): `libpython3.15.a` and the static libraries it links against go to `cpython/`, and the standard library goes to `assets/`, which Subako mounts read-only at `/package`. It also copies a few emulation libraries from the matching [WASI SDK](https://github.com/WebAssembly/wasi-sdk) sysroot, and precompiles the standard library's bytecode, which takes about 40% off each call's startup because Python can't cache bytecode on a read-only mount.
  It then installs the pure-Python packages it pins (pygal) into `assets/site-packages`, which is on `sys.path`, with the shims in `src/site-packages`, and downloads the fonts.
- `build.rs` links them into the wasm build.
- `src/main.rs` starts the interpreter through a handful of C API calls and runs `src/harness.py`, which captures `sys.stdout` and `sys.stderr`, runs the script, and hands the result back as JSON. It also gives Python two C functions: `_subako.http_request`, which passes a request to the SDK's `http::request` as JSON, and `_subako.svg_to_png`, which draws SVG with `src/svg.rs`. `src/subako.py` builds the `subako` module and the urllib handler on them. The handler is installed only when a script imports `urllib.request`, which costs about 35 ms.

Output written straight to file descriptors 1 and 2 (`os.write(1, ...)`) bypasses the capture and is lost.

A call takes about 160 ms on an M-series Mac in wasmtime, almost all of it starting the interpreter; one that draws a pygal chart as PNG takes about 260 ms.

## Build

Needs [wasmtime](https://wasmtime.dev), to precompile the standard library and to run the tests.

```sh
./fetch-cpython.sh                                     # once; package.sh also runs it
cargo test --target wasm32-wasip1                      # runs the tests in wasmtime
./package.sh                                           # dist/python-function.tar.gz
subako function create dist/python-function.tar.gz
```

The tests run in WebAssembly because only the wasm build embeds Python; the build for your machine just prints the package manifest. wasmtime has no `subako_http` import, so `tests/subako_http_stub.wat` stands in for it and fails every request; the urllib tests replace `_subako.http_request` with a fake worker written in Python.

To try the module directly:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"run_python","arguments":{"source":{"type":"code","code":"print(2 ** 100)"}}}}' |
    wasmtime run --preload subako_http=tests/subako_http_stub.wat --argv0 python \
    --dir assets::/package target/wasm32-wasip1/release/python-function.wasm
```

CPython is under the [PSF License](https://docs.python.org/3/license.html), which `fetch-cpython.sh` puts in the bundle as `assets/LICENSE-CPython`. pygal is under the LGPL 3.0 and its license is in its `dist-info`; the fonts are under the SIL Open Font License 1.1.
