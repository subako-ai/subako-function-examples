# git

A Subako function package that clones git repositories onto the Volume, and commits and pushes what other tools change there, with [go-git](https://github.com/go-git/go-git). It is written in Go with [subako-function-sdk-go](https://github.com/subako-ai/subako-function-sdk-go) and the [official MCP Go SDK](https://github.com/modelcontextprotocol/go-sdk).

A clone is a plain directory on the Volume, so the other functions an agent has work on it directly. To edit a website, for example:

1. `git_clone` the site to `/volume/data/site`.
2. Edit its files with the `files` functions.
3. Draw a page with [html](../../rust/html/)'s `render_html` to check it.
4. `git_checkout` a new branch, then `git_commit` and `git_push` it.

## Functions

Every function takes `dir`, the clone's directory on the Volume, except `git_clone`, which makes it. Every function that reports a commit returns it as `head`: `{"branch": "main", "commit": "84ff282f…", "subject": "Launch site"}`. `branch` is empty on a detached HEAD.

### `git_clone`

| Input | |
| --- | --- |
| `url` | The repository's https URL, such as `https://github.com/OWNER/REPO.git` |
| `dir` | Volume directory to clone into, such as `/volume/data/site`. It must not exist, or be empty |
| `branch` | Optional. The branch to check out; the remote's default branch otherwise |
| `depth` | Optional. Fetch only this many recent commits of that one branch, as `git clone --depth` does. A shallow clone is quicker for a repository with a long history, and still commits and pushes |

It returns `dir`, `head`, and how many `files` the checked-out tree has.

Use `/volume/data` for a clone that should last. `/volume/tmp` is cleared after a day without use.

### `git_status`

It returns `head`, the local `branches`, and `changes`: the files that differ from the last commit, such as `[{"path": "about.html", "change": "added"}, {"path": "index.html", "change": "modified"}]`. `change` is `added`, `modified`, `deleted`, `renamed` or `copied`.

### `git_diff`

| Input | |
| --- | --- |
| `paths` | Optional. Files to diff, relative to `dir`. Every changed file by default |

It returns the `changes`, and the `diff` from the last commit to the files on the Volume, unified as `git diff HEAD` writes it. A binary file shows only as changed. A diff past 100 KB is cut at a line, and `cut: true` says so. Pass fewer `paths` to see the rest.

### `git_commit`

| Input | |
| --- | --- |
| `message` | The commit message |
| `paths` | Optional. Files to commit, relative to `dir`. Every changed file by default, new and deleted files included |
| `author_name`, `author_email` | Optional. The author. The clone's `user.name` and `user.email` otherwise. A function has no global git config, so a commit with neither fails |

It returns the new `head`, and the `changes` the commit records.

### `git_push`

| Input | |
| --- | --- |
| `remote` | Optional. The remote's name; `origin` by default |
| `force` | Optional. Replace the remote branch even where it has commits the clone lacks, as `git push --force-with-lease` does |

It pushes the current branch to the branch of the same name on the remote, creating that branch when it is new. A remote that has moved on is an error that says to `git_pull` first. `up_to_date: true` means the remote already had the commit.

`force` replaces only what the clone has seen. When the remote branch has moved since the clone last fetched it, the push is refused, so a commit someone pushed in the meantime is never lost unseen. `git_pull` fetches the remote's commits even when it then can't fast-forward to them. After that, a push with `force` replaces them.

### `git_pull`

| Input | |
| --- | --- |
| `remote` | Optional. The remote's name; `origin` by default |

It fast-forwards the current branch to the remote's. The clone must have no uncommitted changes. When both sides have new commits, it can't merge them. Push the clone's commits to a new branch instead, or replace the remote's with `git_push`'s `force`. `up_to_date: true` means the remote had nothing new.

### `git_log`

| Input | |
| --- | --- |
| `limit` | Optional. How many commits to return, newest first. 20 by default, at most 200 |
| `path` | Optional. Only commits that change this file |

It returns `commits`: each one's `commit`, `author`, `date` and full `message`. `more: true` says older commits follow.

### `git_checkout`

| Input | |
| --- | --- |
| `branch` | The branch to switch to |
| `create` | Optional. Create `branch` at the current commit and switch to it, keeping uncommitted changes, as `git switch -c` does |

Without `create`, `branch` may also be one only the remote has, which gets a local branch, as `git switch` does. A clone with uncommitted changes must commit them first, or take them to a new branch with `create`.

## Network and credentials

WASI has no sockets, so git's requests go through the Subako worker. Only git's smart HTTP protocol works; `ssh://` and `git://` URLs don't. The function's network setting must allow the git host, such as `github.com`.

The function never handles a token itself. To clone a private repository or to push, store a credential for the host's origin in a Subako vault, and grant it to the function. The worker adds the credential's headers to every request to that origin, and the code, the agent, and the Volume never see it. For GitHub, the origin is `https://github.com`, and the header is:

```
Authorization: Basic <base64 of "x-access-token:<token>">
```

`<token>` is a fine-grained personal access token with read and write **Contents** permission on the repositories to push to. For a token without write permission, cloning works but pushing doesn't.

## How it works

`main.go` adds the tools; `git.go` holds them. `transport.go` is the HTTP client go-git talks through. git's request and response bodies are binary pack data, which the worker carries only as Volume files. So each request body is written under `/volume/tmp/.git-http/` and sent as a file, and each response is saved there and read back. Both files are deleted once used. The worker follows no redirects for it. net/http follows them instead, so go-git's own redirect policy still applies.

WASI leaves out a few things go-git expects, which the package makes up for:

- **Git config.** There is no `/etc/gitconfig` or home directory to read, so system and global config load empty.
- **Clone target.** `git.PlainClone`'s check that the target is empty stats from `/`, which WASI cannot open, so `git_clone` builds the repository with `git.CloneContext` instead.
- **Executable bit.** Files have none, so, as git does on such a filesystem, a clone sets `core.fileMode` to `false`. go-git still takes an index entry's mode from the file. So after every checkout and before every commit, entries that HEAD marks executable get that mode back, and a script edited here stays executable.
- **File times.** go-git's wasip1 code reads a file's change time as seconds, but WASI gives nanoseconds, which overflows the index's timestamp. `systeminfo_wasip1.go` replaces that hook.

go-git v6 is in beta. v5 doesn't build for wasip1.

## Limits

A call has 60 s by default, and up to 300 s if the agent's grant allows it, to fetch and write the whole clone. go-git's own repository, 12 MB of packs and 755 files, clones in about 17 s under [wazero](https://wazero.io), or 8 s with `depth: 1`. A repository with a long history of large images may need `depth`.

## Build

Needs Go 1.26.

```sh
go test ./...                          # needs the git CLI, which serves the tests' remote
./package.sh                           # dist/git.tar.gz
subako function create dist/git.tar.gz
```

The tests run each tool against a repository that `git http-backend` serves. The test's stand-in for the worker spools bodies to files, as the worker does.

To try the module locally with [wasmtime](https://wasmtime.dev), mount directories as `/volume/tmp` and `/volume/data`. wasmtime has no `subako_http` import, so `tests/subako_http_stub.wat` stands in for it. Every request fails, but the functions that read a clone work:

```sh
GOOS=wasip1 GOARCH=wasm go build -o dist/module.wasm .
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"git_log","arguments":{"dir":"/volume/data/site","limit":5}}}' |
    wasmtime run --preload subako_http=tests/subako_http_stub.wat --argv0 git \
    --dir /path/to/tmp::/volume/tmp --dir /path/to/data::/volume/data dist/module.wasm
```

go-git is under the [Apache License 2.0](https://github.com/go-git/go-git/blob/main/LICENSE).
