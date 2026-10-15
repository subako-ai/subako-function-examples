# quickjs

A Subako function package that runs JavaScript in [QuickJS](https://github.com/quickjs-ng/quickjs) (QuickJS-ng, ES2023), compiled into the function through [rquickjs](https://github.com/DelSkayn/rquickjs). It runs code passed as a string or a script file on the Volume, with `fetch` through the Subako worker and the Volume's files.

## Function

### `run_javascript`

| Input | |
| --- | --- |
| `source` | `{"type": "code", "code": "..."}`, or `{"type": "file", "path": "/volume/data/report.js"}` for a script on the Volume |
| `args` | Optional. Strings for the script, as the global `args` |

```json
{ "source": { "type": "code", "code": "const r = await fetch('https://api.github.com/repos/quickjs-ng/quickjs');\nconst repo = await r.json();\nconsole.log(repo.full_name);\nreturn repo.stargazers_count;" } }
```

The result has what the script wrote to `stdout` (`console.log`, `info`, `debug`) and `stderr` (`console.warn`, `error`), its return value as JSON in `result`, and an `outcome`:

| `outcome` | |
| --- | --- |
| `{"type": "completed"}` | It ran to the end and every promise it awaited settled |
| `{"type": "raised", "exception": "TypeError: ...", "stack": "at f (<script>:3:5)"}` | It threw, or a promise it awaited rejected |
| `{"type": "stopped", "seconds": 60}` | It ran past the time limit |

An exception is still a successful call, so the agent keeps what the script printed first. Each call is a fresh engine: nothing carries over between calls but files on the Volume.

## What scripts get

The script is the body of an async function, so it can `await` and `return`. Beyond the language itself:

- `console`, `setTimeout` (run in order once promises settle; delays are ignored), `queueMicrotask`, and UTF-8 `TextEncoder` and `TextDecoder`.
- `fetch(url, { method, headers, body, redirect })`, returning a `Response` with `status`, `ok`, `headers`, `text()`, `json()`, `arrayBuffer()` and `bytes()`. Requests are HTTPS through the Subako worker, to hosts the function's network setting allows, with any stored credentials for their origin; a refused request rejects with `TypeError: fetch failed: ...`. Bodies may be strings or binary.
- `fs`: `readText`, `readBytes` (a `Uint8Array`), `writeText`, `writeBytes`, `appendText`, `list`, `exists` and `remove` for files on the Volume. Writing creates missing directories.
- `await import("./lib.js")` for other script files, which may themselves `import` by relative path. There are no packages, no `require`, and no static `import` in the script itself, since it is a function body.

Relative paths, in `fs` and in `import()`, resolve from `/volume/tmp`, or from the script's directory for a script file.

## Limits

A script is stopped after 60 seconds, and the engine may use 1 GiB of memory.

## How it works

`src/main.rs` creates a QuickJS runtime for each call, runs `src/prelude.js`, which builds `console`, `fetch`, `fs` and the rest on a few functions written in Rust, then runs the script and keeps running promise jobs and timers until nothing is left. A call takes about 45 ms in wasmtime on an M-series Mac, and the bundle is about 0.4 MB.

## Build

QuickJS is C, so building for `wasm32-wasip1` needs the [WASI SDK](https://github.com/WebAssembly/wasi-sdk)'s clang. `fetch-deps.sh` downloads it (about 180 MB) into `wasi-sdk/`, and `.cargo/config.toml` points the build at it.

```sh
./fetch-deps.sh                                        # once; package.sh also runs it
cargo test                                             # natively
cargo test --target wasm32-wasip1                      # in wasmtime
./package.sh                                           # dist/quickjs-function.tar.gz
subako function create dist/quickjs-function.tar.gz
```

Locally, `tests/subako_http_stub.wat` stands in for the worker's `subako_http` import and fails every request:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"run_javascript","arguments":{"source":{"type":"code","code":"return [1, 2, 3].map((n) => n * 2);"}}}}' |
    wasmtime run --preload subako_http=tests/subako_http_stub.wat --argv0 javascript \
    --dir /path/to/scratch::/volume/tmp target/wasm32-wasip1/release/quickjs-function.wasm
```

QuickJS-ng and rquickjs are under the MIT license.
