# bashkit

A Subako function package that runs bash scripts with [Bashkit](https://github.com/everruns/bashkit), a bash interpreter written in Rust with its commands built in. Scripts get pipes, redirects and about 145 commands, work on the Volume's real files, and reach the network through a limited `curl`.

## Function

### `run_bash`

```json
{ "script": "tail -n +2 /data/stock.csv | cut -d, -f1 | sort | uniq -c > counts.txt && cat counts.txt" }
```

returns

```json
{ "exit_code": 0, "stdout": "      1 apple\n      1 pear\n", "stderr": "" }
```

A nonzero exit code is still a successful call. `stdout_truncated` and `stderr_truncated` appear when output passed 1 MiB and was cut off.

## What scripts get

- **Bash:** pipes, redirects, variables, functions, arrays, loops, globs, command substitution and here-docs, from Bashkit's interpreter.
- **Commands:** `grep`, `sed`, `awk`, `jq`, `yq`, `find`, `sort`, `uniq`, `cut`, `tr`, `wc`, `head`, `tail`, `diff`, `tar`, `gzip`, `zip`, `base64`, `sha256sum`, `bc` and the rest of Bashkit's built-in commands. They are reimplementations, so their behavior can differ from GNU's at the edges. There are no other programs to run.
- **Files:** `/tmp` and `/data` are the Volume's real directories, and scripts start in `/tmp`. Anything written elsewhere lives in memory and is gone after the call, as are variables and functions.
- **Network:** `curl`, below. Bashkit's own `wget` and `http`, which need sockets, report "command not found", as do its interactive commands (`less`, `watch`, `clear`, `history`, `fc`).

## curl

WASI has no sockets, so `curl` here is this package's own builtin. It sends each request through the Subako worker with the SDK's [`http`](https://docs.rs/subako-function-sdk/latest/subako_function_sdk/http/) module. Requests are HTTPS only, reach only hosts the function's network setting allows, and carry any stored credentials for their own origin.

It takes one URL and these options:

| Option | |
| --- | --- |
| `-X`, `--request` | Method: GET, HEAD, POST, PUT, PATCH, DELETE or OPTIONS |
| `-H`, `--header` | A request header |
| `-d`, `--data`, `--data-binary` | A body, sent as POST unless `-X` says otherwise; repeated values join with `&`; `@file` reads a file |
| `--data-raw` | A body that never reads `@file` |
| `--json` | A JSON body, with JSON `Content-Type` and `Accept` |
| `-o`, `--output` | Write the body to a file |
| `-i`, `--include` / `-I`, `--head` | Print the status line and headers, before the body / instead of it |
| `-L`, `--location` | Follow redirects (off by default, as in curl) |
| `-f`, `--fail` | Exit 22 on a status of 400 or more |
| `-s`, `--silent` / `-S`, `--show-error` | Hide error messages / show them anyway |
| `-w`, `--write-out` | `%{http_code}`, `%{response_code}`, `%{size_download}`, `%{url_effective}` |
| `-m`, `--max-time` | Seconds to allow the request |
| `-A`, `--user-agent` | The `User-Agent` header |

Any other option fails with exit code 2 and the list above. Failures use curl's exit codes: 7 when the worker refuses or can't reach the host (including hosts the network setting doesn't allow), 28 for a timeout, 47 for too many redirects, 22 for `-f`.

Bodies pass through a temporary file in `/tmp`, so binary responses and uploads work and count against the call's Volume write budget.

## Limits

Bashkit's limits apply, with the command and loop counts raised: a script may run 1,000,000 commands and 10,000,000 loop iterations in all, within a 30-second timeout, and keeps 1 MiB each of stdout and stderr. A script that crosses a limit fails the call with the limit named.

## How it works

- `src/main.rs` builds a Bashkit `Bash` for each call, mounts the Volume, registers `curl`, and runs the script on a single-threaded tokio runtime.
- `src/volume.rs` is the Volume as a Bashkit `FsBackend` over `std::fs`. Bashkit's own `realfs` feature uses `tokio::fs`, which tokio leaves out on WebAssembly.
- `src/curl.rs` is the curl builtin. Its HTTP call is a function it is handed, so tests hand it a fake worker.

A call takes about 90 ms in wasmtime on an M-series Mac.

## Build

```sh
cargo test                                             # natively
cargo test --target wasm32-wasip1                      # in wasmtime
./package.sh                                           # dist/bashkit-function.tar.gz
subako function create dist/bashkit-function.tar.gz
```

The wasm tests and local runs need [wasmtime](https://wasmtime.dev). It has no `subako_http` import, so `tests/subako_http_stub.wat` stands in for it and fails every request:

```sh
printf '%s' '{"script":"echo hello | tr a-z A-Z"}' |
    wasmtime run --preload subako_http=tests/subako_http_stub.wat --argv0 bash \
    --dir /path/to/scratch::/tmp target/wasm32-wasip1/release/bashkit-function.wasm run_bash
```

Bashkit is under the [MIT license](https://github.com/everruns/bashkit/blob/main/LICENSE).
