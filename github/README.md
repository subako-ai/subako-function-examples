# github

A Subako function package for asking questions about GitHub repositories. It reads only: any GET in the REST API, files and directories in a repository, and whole repositories downloaded to the Volume for other tools to search. Public repositories need no token. A token stored in Subako adds private repositories, code search, and a higher rate limit.

## Functions

### `github_api`

GETs any REST API path and returns its JSON.

| Input | |
| --- | --- |
| `path` | The path and query, such as `/repos/OWNER/REPO/issues?state=open&per_page=50`, or a full api.github.com URL |
| `accept` | Optional `Accept` media type. `application/vnd.github.diff` gets a pull request or commit as a diff, returned as text |
| `output` | Optional Volume path to save the body to instead of returning it |

```json
{ "path": "/repos/subako-ai/subako-function-template-rust/commits?per_page=30" }
```

returns

```json
{
  "status": 200,
  "body": [{ "sha": "8f54973c28b2d6b1a9991399fdf791f207e4382b", "commit": { "message": "Rust function package template", ... }, ... }],
  "bytes": 4327,
  "rate_limit": { "resource": "core", "limit": 60, "remaining": 50, "resets_in_minutes": 41 }
}
```

A list longer than a page also has `next_page`, the path of the next page, to pass back as `path`. Bodies over 100 KB are saved under `/tmp/github/` and `saved_to` names the file. Ask for fewer results, or read the file with another tool.

### `read_file`

Reads a file, or lists a directory, at any branch, tag or commit.

| Input | |
| --- | --- |
| `repo` | `OWNER/REPO`, or the repository's github.com URL |
| `path` | The file or directory, such as `src/main.rs`. Empty for the top directory |
| `ref` | Optional branch, tag or commit SHA. Defaults to the default branch |
| `start_line`, `end_line` | Optional lines to return, from 1 |
| `output` | Optional Volume path to save the file to instead, for binary files |

A file comes back as `{"type": "file", "text": ..., "lines": 120, "first_line": 1, "last_line": 120, "cut": false}`. Text past about 100 KB is cut at a line, with `cut: true`, and `start_line` gets the rest. A directory comes back as `{"type": "dir", "entries": [{"name": "src", "type": "dir"}, ...]}`.

### `download_repo`

Downloads a repository's files at a branch, tag or commit to the Volume: `/tmp/github/OWNER/REPO` by default, or `dest`. The files are there for questions that need the whole tree, such as grep in [bashkit](../bashkit/) or a script in [python](../python/). It returns the commit, the number of files and bytes, and the top directory's entries. It doesn't fetch git history; `github_api` covers commits.

A `dest` that already holds files is an error, unless `replace: true` asks for them to be deleted first. Symbolic links aren't created; `skipped_links` counts them.

## Network and tokens

WASI has no sockets, so requests go through the Subako worker with the SDK's [`http`](https://docs.rs/subako-function-sdk/latest/subako_function_sdk/http/) module. The function's network setting must allow:

- `api.github.com`
- `codeload.github.com`, for `download_repo`, which GitHub redirects to

The function never handles a token itself. Store a GitHub personal access token in a Subako vault as a credential for `https://api.github.com`, and grant it to the function. The worker then adds it to every request to that origin, and the code, the agent, and the Volume never see it. A fine-grained token for the repositories you pick, with read-only **Contents** (GitHub adds **Metadata** itself), covers questions about the code: files, downloads, commits, branches, tags, releases, comparisons and code search. A private repository's issues, pull requests, Actions runs and discussions each need their own read-only permission as well. A public repository's are readable with any token.

Without a token, GitHub allows:

- 60 requests an hour, and 10 searches a minute
- no code search (`/search/code`)
- public repositories only; a private repository answers 404

With one, the limit is 5,000 requests an hour, and GitHub shows whatever the token can read. `rate_limit` in each `github_api` reply shows which applies.

A failure says what GitHub said and what likely went wrong: a 404 that may be a private repository, a used-up rate limit and when it resets, a refused token, or a host the network setting doesn't allow.

## Build

```sh
cargo test                                     # the tests, on this machine
cargo test --target wasm32-wasip1              # the same tests in wasmtime
cargo test -- --ignored                        # against the real GitHub, through curl, without a token
./package.sh                                   # dist/github-function.tar.gz
subako function create dist/github-function.tar.gz
```

The tests pass a fake worker to the functions. wasmtime has no `subako_http` import, so `tests/subako_http_stub.wat` stands in for it.
