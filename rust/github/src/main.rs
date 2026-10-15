use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use subako_function_sdk::{ToolError, function, package};

mod client;

use client::{Client, RateLimit, encode};

/// The most bytes of a response or file returned in the reply. Larger ones
/// are saved to the Volume, or cut, so they don't flood the model's context.
const MAX_INLINE: usize = 100_000;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApiInput {
    /// The API path and query, such as `/repos/OWNER/REPO/issues?state=open&per_page=50`
    /// or `/search/code?q=fn+main+repo:OWNER/REPO`, or a full api.github.com URL.
    path: String,
    /// Optional `Accept` media type, such as `application/vnd.github.diff` for
    /// a pull request or commit as a diff. Defaults to `application/vnd.github+json`.
    #[serde(default)]
    accept: Option<String>,
    /// Optional Volume path to save the body to instead of returning it,
    /// such as `/volume/tmp/issues.json`.
    #[serde(default)]
    output: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct ApiReply {
    status: u16,
    /// The response: JSON, or text for media types such as diffs. Absent when
    /// it was saved to a file instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<serde_json::Value>,
    /// Where the body was saved: `output`, or a file under `/volume/tmp/github` when
    /// it was too large to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    saved_to: Option<String>,
    /// The body's size in bytes.
    bytes: u64,
    /// The path of the next page of a paged list, to pass as `path`.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_page: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rate_limit: Option<RateLimit>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadFileInput {
    /// The repository, as `OWNER/REPO` or its github.com URL.
    repo: String,
    /// The file or directory in the repository, such as `src/main.rs`.
    /// Empty or absent for the top directory.
    #[serde(default)]
    path: String,
    /// Optional branch, tag or commit SHA. Defaults to the default branch.
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
    /// Optional first line to return, from 1.
    #[serde(default)]
    start_line: Option<usize>,
    /// Optional last line to return, inclusive.
    #[serde(default)]
    end_line: Option<usize>,
    /// Optional Volume path to save the file to instead of returning it,
    /// for binary or very large files.
    #[serde(default)]
    output: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ReadFileReply {
    File {
        path: String,
        /// The file's size in bytes.
        size: u64,
        /// The file's number of lines.
        lines: usize,
        /// The lines from `first_line` to `last_line`.
        text: String,
        first_line: usize,
        last_line: usize,
        /// True when the file goes on past `last_line` because it was too
        /// long to return whole; ask for more with `start_line`.
        cut: bool,
    },
    Dir {
        path: String,
        entries: Vec<Entry>,
    },
    Saved {
        output: String,
        size: u64,
    },
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Entry {
    name: String,
    /// `file`, `dir`, `symlink` or `submodule`.
    #[serde(rename = "type")]
    kind: String,
    /// Size in bytes, for files.
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DownloadInput {
    /// The repository, as `OWNER/REPO` or its github.com URL.
    repo: String,
    /// Optional branch, tag or commit SHA. Defaults to the default branch.
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
    /// Optional Volume directory to put the files in. Defaults to
    /// `/volume/tmp/github/OWNER/REPO`.
    #[serde(default)]
    dest: Option<String>,
    /// Delete what `dest` already holds first. Without it, a `dest` that
    /// isn't empty is an error.
    #[serde(default)]
    replace: bool,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Downloaded {
    dest: String,
    /// The commit the files are from.
    #[serde(skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
    files: u64,
    bytes: u64,
    /// What the top directory holds; directories end in `/`.
    top_level: Vec<String>,
    /// Symbolic links in the repository, which aren't created.
    #[serde(skip_serializing_if = "is_zero")]
    skipped_links: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// GET any GitHub REST API path, read-only, and return its JSON: a
/// repository (`/repos/OWNER/REPO`), its issues, pull requests, commits,
/// releases, contributors, languages, a pull request's diff (`accept:
/// application/vnd.github.diff`), searches (`/search/issues?q=...`), and so
/// on. Lists come 30 to a page; add `per_page=100` and follow `next_page`.
/// Public repositories need no token; private ones, code search and higher
/// rate limits need the token configured for this function in Subako, which
/// is added to the request for you. Bodies over 100 KB are saved to a file.
#[function]
fn github_api(input: ApiInput) -> Result<ApiReply, ToolError> {
    api(&Client::worker(), input)
}

/// Read a file from a GitHub repository, or list a directory, at any branch,
/// tag or commit. Long files come back cut at about 100 KB; ask for a range
/// with `start_line` and `end_line`. Binary files need `output`, a Volume
/// path to save them to.
#[function]
fn read_file(input: ReadFileInput) -> Result<ReadFileReply, ToolError> {
    read(&Client::worker(), input)
}

/// Download a whole GitHub repository's files to the Volume, at any branch,
/// tag or commit, to search or analyse them with other tools, such as grep
/// in bash or Python. Without `dest` they go to `/volume/tmp/github/OWNER/REPO`.
/// There is no git history; use `github_api` for commits.
#[function]
fn download_repo(input: DownloadInput) -> Result<Downloaded, ToolError> {
    download(&Client::worker(), input)
}

package! {
    name: "github",
    description: "Reads GitHub: any read-only REST API path, files and directories in a repository, \
                  and whole repositories downloaded to the Volume.",
    functions: [github_api, read_file, download_repo],
}

fn api(client: &Client, input: ApiInput) -> Result<ApiReply, ToolError> {
    let accept = input
        .accept
        .as_deref()
        .unwrap_or("application/vnd.github+json");
    let reply = client.get(&input.path, accept)?;
    let mut result = ApiReply {
        status: reply.status,
        body: None,
        saved_to: None,
        bytes: 0,
        next_page: reply.next_page(),
        rate_limit: reply.rate_limit(),
    };
    if let Some(output) = &input.output {
        result.bytes = reply.save(&volume_path(output)?)?;
        result.saved_to = Some(output.clone());
        return Ok(result);
    }
    let body = reply.read()?;
    result.bytes = body.len() as u64;
    if body.len() > MAX_INLINE {
        let saved = client
            .tmp
            .join(format!("github/response-{}.json", unique()));
        reply.save(&saved)?;
        result.saved_to = Some(saved.to_string_lossy().into_owned());
        return Ok(result);
    }
    result.body = Some(match serde_json::from_slice(&body) {
        Ok(json) => json,
        Err(_) if body.is_empty() => serde_json::Value::Null,
        Err(_) => match String::from_utf8(body) {
            Ok(text) => serde_json::Value::String(text),
            Err(_) => {
                return Err(ToolError::new(
                    "the response is binary; pass `output` to save it",
                ));
            }
        },
    });
    Ok(result)
}

fn read(client: &Client, input: ReadFileInput) -> Result<ReadFileReply, ToolError> {
    let (owner, repo) = parse_repo(&input.repo)?;
    let path = input.path.trim_matches('/');
    let mut url = format!("/repos/{owner}/{repo}/contents");
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        url.push('/');
        url.push_str(&encode(segment));
    }
    if let Some(git_ref) = &input.git_ref {
        url.push_str(&format!("?ref={}", encode(git_ref)));
    }
    // A file comes back raw; a directory still comes back as a JSON list.
    let reply = client.get(&url, "application/vnd.github.raw+json")?;
    if !reply.content_type().contains("vnd.github.raw") {
        let body = reply.read()?;
        let list: Vec<serde_json::Value> = match serde_json::from_slice(&body) {
            Ok(serde_json::Value::Array(list)) => list,
            Ok(other) => {
                let kind = other
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("not a file");
                return Err(ToolError::new(format!(
                    "{path} is a {kind}, which has no content to read"
                )));
            }
            Err(e) => {
                return Err(ToolError::new(format!(
                    "unexpected answer from GitHub: {e}"
                )));
            }
        };
        let entries = list
            .iter()
            .map(|item| Entry {
                name: item["name"].as_str().unwrap_or_default().to_string(),
                kind: item["type"].as_str().unwrap_or_default().to_string(),
                size: (item["type"] == "file")
                    .then(|| item["size"].as_u64())
                    .flatten(),
            })
            .collect();
        return Ok(ReadFileReply::Dir {
            path: path.to_string(),
            entries,
        });
    }

    if let Some(output) = &input.output {
        let size = reply.save(&volume_path(output)?)?;
        return Ok(ReadFileReply::Saved {
            output: output.clone(),
            size,
        });
    }
    let body = reply.read()?;
    let size = body.len() as u64;
    let text = String::from_utf8(body).map_err(|_| {
        ToolError::new(format!(
            "{path} is a binary file; pass `output` to save it to the Volume"
        ))
    })?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let first = input.start_line.unwrap_or(1).max(1);
    let last = input.end_line.unwrap_or(lines.len()).min(lines.len());
    if first > last && !lines.is_empty() {
        return Err(ToolError::new(format!(
            "{path} has {} lines; lines {first} to {last} aren't in it",
            lines.len()
        )));
    }
    let mut shown = String::new();
    let mut shown_last = first - 1;
    for line in lines.iter().take(last).skip(first - 1) {
        if shown.len() + line.len() > MAX_INLINE && shown_last >= first {
            break;
        }
        shown.push_str(line);
        shown_last += 1;
    }
    Ok(ReadFileReply::File {
        path: path.to_string(),
        size,
        lines: lines.len(),
        text: shown,
        first_line: first,
        last_line: shown_last,
        cut: shown_last < last,
    })
}

fn download(client: &Client, input: DownloadInput) -> Result<Downloaded, ToolError> {
    let (owner, repo) = parse_repo(&input.repo)?;
    let dest = match &input.dest {
        Some(dest) => volume_path(dest)?,
        None => client.tmp.join(format!("github/{owner}/{repo}")),
    };
    if std::fs::read_dir(&dest).is_ok_and(|mut d| d.next().is_some()) {
        if !input.replace {
            return Err(ToolError::new(format!(
                "{} already has files; pass `replace: true` to delete them first, or another `dest`",
                dest.display()
            )));
        }
        std::fs::remove_dir_all(&dest)
            .map_err(|e| ToolError::new(format!("cannot delete {}: {e}", dest.display())))?;
    }

    let mut url = format!("/repos/{owner}/{repo}/tarball");
    if let Some(git_ref) = &input.git_ref {
        url.push('/');
        url.push_str(&encode(git_ref));
    }
    // GitHub redirects to codeload.github.com with a short-lived link, which
    // works for private repositories too.
    let reply = client.get(&url, "application/vnd.github+json")?;
    let file = std::fs::File::open(&reply.body)
        .map_err(|e| ToolError::new(format!("cannot read the download: {e}")))?;
    let mut result = Downloaded {
        dest: dest.to_string_lossy().into_owned(),
        commit: None,
        files: 0,
        bytes: 0,
        top_level: Vec::new(),
        skipped_links: 0,
    };
    unpack(file, &dest, &mut result)
        .map_err(|e| ToolError::new(format!("cannot unpack the download: {e}")))?;

    let mut top: Vec<String> = std::fs::read_dir(&dest)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect();
    top.sort();
    result.top_level = top;
    Ok(result)
}

/// Writes the files of GitHub's tarball under `dest`, without the
/// `OWNER-REPO-SHA/` directory they all sit in.
fn unpack(file: std::fs::File, dest: &Path, result: &mut Downloaded) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            // GitHub's says `comment=<commit sha>`.
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            result.commit = text.lines().find_map(|line| {
                line.split_once(" comment=")
                    .map(|(_, sha)| sha.trim().to_string())
            });
            continue;
        }
        let path = entry.path()?.into_owned();
        let mut parts = path.components();
        parts.next();
        let relative = parts.as_path();
        if relative.as_os_str().is_empty() {
            continue;
        }
        if relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(std::io::Error::other(format!(
                "unsafe path {}",
                path.display()
            )));
        }
        let target = dest.join(relative);
        if kind.is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if kind.is_file() {
            if let Some(dir) = target.parent() {
                std::fs::create_dir_all(dir)?;
            }
            result.bytes += std::io::copy(&mut entry, &mut std::fs::File::create(&target)?)?;
            result.files += 1;
        } else if kind.is_symlink() || kind.is_hard_link() {
            result.skipped_links += 1;
        }
    }
    Ok(())
}

/// `OWNER/REPO` from that or a github.com URL.
fn parse_repo(repo: &str) -> Result<(String, String), ToolError> {
    let trimmed = repo.trim();
    let rest = ["https://github.com/", "http://github.com/", "github.com/"]
        .iter()
        .find_map(|prefix| trimmed.strip_prefix(prefix))
        .unwrap_or(trimmed);
    let mut parts = rest.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default().trim_end_matches(".git");
    let valid = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    if valid(owner) && valid(name) {
        Ok((owner.to_string(), name.to_string()))
    } else {
        Err(ToolError::new(format!(
            "{repo:?} isn't a repository; give it as OWNER/REPO"
        )))
    }
}

fn volume_path(path: &str) -> Result<PathBuf, ToolError> {
    let path = Path::new(path);
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Err(ToolError::new(format!(
            "{} isn't an absolute Volume path, such as /volume/tmp/out.json",
            path.display()
        )))
    }
}

/// A name for a saved response that won't clash with earlier ones.
fn unique() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}-{}", now.as_secs(), NEXT.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use subako_function_sdk::http::{
        self, HttpError, HttpErrorKind, Request, Response, ResponseBody,
    };

    use super::*;

    type Answer = Result<(u16, Vec<(&'static str, String)>, Vec<u8>), HttpError>;

    /// A fresh real directory standing in for `/volume/tmp`.
    fn volume(name: &str) -> PathBuf {
        // WASI has no `temp_dir`; the wasmtime runner maps target/ to /volume/tmp.
        let base = if cfg!(target_os = "wasi") {
            PathBuf::from("/volume/tmp")
        } else {
            std::env::temp_dir()
        };
        let dir = base.join(format!("github-function-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake worker: records each request's URL and answers with `answer`,
    /// writing the body where the request says.
    fn worker(
        dir: &Path,
        answer: impl Fn(&str) -> Answer + Send + Sync + 'static,
    ) -> (Client, Arc<Mutex<Vec<Request>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        let transport: client::Transport = Arc::new(move |request: &Request| {
            log.lock().unwrap().push(request.clone());
            let (status, headers, body) = answer(&request.url)?;
            let http::ResponseOutput::File { path } = &request.output else {
                panic!("the body should go to a file");
            };
            std::fs::write(path, &body).unwrap();
            Ok(Response {
                status,
                headers: headers
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
                body: ResponseBody::File {
                    path: path.clone(),
                    bytes: body.len() as u64,
                },
            })
        });
        (Client::new(transport, dir.to_path_buf()), sent)
    }

    fn json(status: u16, body: &str) -> Answer {
        Ok((
            status,
            vec![("content-type", "application/json; charset=utf-8".into())],
            body.as_bytes().to_vec(),
        ))
    }

    fn raw(body: &[u8]) -> Answer {
        Ok((
            200,
            vec![(
                "content-type",
                "application/vnd.github.raw+json; charset=utf-8".into(),
            )],
            body.to_vec(),
        ))
    }

    fn get(path: &str) -> ApiInput {
        ApiInput {
            path: path.to_string(),
            accept: None,
            output: None,
        }
    }

    fn file(repo: &str, path: &str) -> ReadFileInput {
        ReadFileInput {
            repo: repo.to_string(),
            path: path.to_string(),
            git_ref: None,
            start_line: None,
            end_line: None,
            output: None,
        }
    }

    #[test]
    fn gets_json_with_paging_and_rate_limit() {
        let dir = volume("api");
        let (client, sent) = worker(&dir, |_| {
            let mut answer = json(200, r#"[{"number": 1}]"#)?;
            answer.1.extend([
                ("link", r#"<https://api.github.com/repositories/1/issues?page=2>; rel="next", <https://api.github.com/repositories/1/issues?page=5>; rel="last""#.into()),
                ("x-ratelimit-limit", "60".into()),
                ("x-ratelimit-remaining", "59".into()),
                ("x-ratelimit-reset", "0".into()),
            ]);
            Ok(answer)
        });
        let reply = api(&client, get("/repos/a/b/issues")).unwrap();
        assert_eq!(reply.body, Some(serde_json::json!([{"number": 1}])));
        assert_eq!(
            reply.next_page.as_deref(),
            Some("/repositories/1/issues?page=2")
        );
        assert_eq!(reply.rate_limit.unwrap().remaining, 59);

        let sent = sent.lock().unwrap();
        assert_eq!(sent[0].url, "https://api.github.com/repos/a/b/issues");
        assert_eq!(sent[0].method, http::Method::Get);
        assert_eq!(sent[0].headers["accept"], "application/vnd.github+json");
        assert!(
            !sent[0].headers.contains_key("authorization"),
            "the worker adds the token"
        );
    }

    #[test]
    fn returns_diffs_as_text_and_saves_large_bodies() {
        let dir = volume("api-text");
        let big = format!("[{}0]", "0,".repeat(MAX_INLINE));
        let (client, _) = worker(&dir, move |url| {
            if url.ends_with("/pulls/7") {
                Ok((
                    200,
                    vec![("content-type", "application/vnd.github.diff".into())],
                    b"diff --git a/x b/x\n".to_vec(),
                ))
            } else {
                json(200, &big)
            }
        });
        let diff = ApiInput {
            accept: Some("application/vnd.github.diff".into()),
            ..get("/repos/a/b/pulls/7")
        };
        assert_eq!(
            api(&client, diff).unwrap().body,
            Some(serde_json::json!("diff --git a/x b/x\n"))
        );

        let large = api(&client, get("/repos/a/b/commits")).unwrap();
        assert!(large.body.is_none());
        let saved = large.saved_to.unwrap();
        assert!(
            saved.starts_with(&dir.join("github").to_string_lossy().into_owned()),
            "{saved}"
        );
        assert_eq!(std::fs::metadata(&saved).unwrap().len(), large.bytes);

        let output = dir.join("out/commits.json");
        let asked = api(
            &client,
            ApiInput {
                output: Some(output.to_string_lossy().into()),
                ..get("/x")
            },
        )
        .unwrap();
        assert_eq!(asked.saved_to.unwrap(), output.to_string_lossy());
        assert_eq!(std::fs::metadata(&output).unwrap().len(), asked.bytes);
        // Nothing is left behind in the scratch directory.
        assert_eq!(
            std::fs::read_dir(dir.join(".github-function"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn explains_errors() {
        let dir = volume("errors");
        let (client, _) = worker(&dir, |url| {
            if url.contains("private") {
                json(404, r#"{"message": "Not Found"}"#)
            } else if url.contains("limited") {
                let mut answer = json(
                    403,
                    r#"{"message": "API rate limit exceeded for 1.2.3.4."}"#,
                )?;
                answer.1.extend([
                    ("x-ratelimit-limit", "60".into()),
                    ("x-ratelimit-remaining", "0".into()),
                ]);
                Ok(answer)
            } else {
                Err(HttpError {
                    kind: HttpErrorKind::HostNotAllowed,
                    message: "host not allowed".into(),
                })
            }
        });
        let message = |path: &str| api(&client, get(path)).unwrap_err().message().to_string();
        assert!(
            message("/repos/a/private")
                .contains("404: Not Found. GitHub also answers 404 for a private")
        );
        let limited = message("/repos/a/limited");
        assert!(
            limited.contains("rate limit exceeded for 1.2.3.4. The core rate limit of 60"),
            "{limited}"
        );
        assert!(limited.contains("A token raises it"));
        assert!(
            message("/repos/a/b").contains("network setting in Subako must allow api.github.com")
        );
        assert!(message("https://example.com/").contains("not on api.github.com"));
    }

    #[test]
    fn reads_files_and_lists_directories() {
        let dir = volume("read");
        let (client, sent) = worker(&dir, |url| {
            if url.contains("/contents/src/main.rs") {
                raw(b"one\ntwo\nthree\nfour\n")
            } else if url.contains("/contents/logo.png") {
                raw(&[0x89, b'P', b'N', b'G', 0xff, 0xfe])
            } else {
                json(
                    200,
                    r#"[{"name": "src", "type": "dir", "size": 0}, {"name": "README.md", "type": "file", "size": 42}]"#,
                )
            }
        });
        let ReadFileReply::File {
            text,
            lines,
            last_line,
            cut,
            ..
        } = read(&client, file("https://github.com/a/b.git", "/src/main.rs")).unwrap()
        else {
            panic!("expected a file");
        };
        assert_eq!(
            (text.as_str(), lines, last_line, cut),
            ("one\ntwo\nthree\nfour\n", 4, 4, false)
        );

        let range = ReadFileInput {
            start_line: Some(2),
            end_line: Some(3),
            git_ref: Some("v1.0 beta".into()),
            ..file("a/b", "src/main.rs")
        };
        let ReadFileReply::File {
            text,
            first_line,
            last_line,
            ..
        } = read(&client, range).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (text.as_str(), first_line, last_line),
            ("two\nthree\n", 2, 3)
        );
        assert_eq!(
            sent.lock().unwrap()[1].url,
            "https://api.github.com/repos/a/b/contents/src/main.rs?ref=v1.0%20beta"
        );
        assert_eq!(
            sent.lock().unwrap()[1].headers["accept"],
            "application/vnd.github.raw+json"
        );

        let ReadFileReply::Dir { entries, .. } = read(&client, file("a/b", "")).unwrap() else {
            panic!()
        };
        assert_eq!(entries.len(), 2);
        assert_eq!((entries[0].kind.as_str(), entries[0].size), ("dir", None));
        assert_eq!(
            (entries[1].name.as_str(), entries[1].size),
            ("README.md", Some(42))
        );

        assert!(
            read(&client, file("a/b", "logo.png"))
                .unwrap_err()
                .message()
                .contains("binary")
        );
        let output = dir.join("logo.png");
        let saved = ReadFileInput {
            output: Some(output.to_string_lossy().into()),
            ..file("a/b", "logo.png")
        };
        assert!(matches!(
            read(&client, saved).unwrap(),
            ReadFileReply::Saved { size: 6, .. }
        ));

        assert!(read(&client, file("not a repo", "x")).is_err());
        let past = ReadFileInput {
            start_line: Some(9),
            ..file("a/b", "src/main.rs")
        };
        assert!(
            read(&client, past)
                .unwrap_err()
                .message()
                .contains("has 4 lines")
        );
    }

    #[test]
    fn cuts_long_files() {
        let dir = volume("long");
        let (client, _) = worker(&dir, |_| {
            raw(format!("{}\n", "x".repeat(99)).repeat(2000).as_bytes())
        });
        let ReadFileReply::File {
            lines,
            last_line,
            cut,
            text,
            ..
        } = read(&client, file("a/b", "big.txt")).unwrap()
        else {
            panic!()
        };
        assert_eq!((lines, last_line, cut), (2000, 1000, true));
        assert_eq!(text.len(), MAX_INLINE);
    }

    /// A tarball shaped like GitHub's: a pax header with the commit, and
    /// everything in one `OWNER-REPO-SHA/` directory.
    fn tarball() -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let pax = b"52 comment=0123456789abcdef0123456789abcdef01234567\n";
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(tar::EntryType::XGlobalHeader);
        header.set_size(pax.len() as u64);
        builder
            .append_data(&mut header, "pax_global_header", &pax[..])
            .unwrap();
        for (path, body) in [
            ("a-b-0123456/README.md", &b"# b\n"[..]),
            ("a-b-0123456/src/lib.rs", b"pub fn f() {}\n"),
        ] {
            let mut header = tar::Header::new_ustar();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            builder.append_data(&mut header, path, body).unwrap();
        }
        let mut link = tar::Header::new_ustar();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        builder
            .append_link(&mut link, "a-b-0123456/docs", "README.md")
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn downloads_a_repository() {
        let dir = volume("download");
        let (client, sent) = worker(&dir, |_| Ok((200, vec![], tarball())));
        let input = || DownloadInput {
            repo: "a/b".into(),
            git_ref: Some("main".into()),
            dest: None,
            replace: false,
        };
        let done = download(&client, input()).unwrap();
        assert_eq!(
            sent.lock().unwrap()[0].url,
            "https://api.github.com/repos/a/b/tarball/main"
        );
        assert!(sent.lock().unwrap()[0].follow_redirects);
        assert_eq!(done.dest, dir.join("github/a/b").to_string_lossy());
        assert_eq!(
            done.commit.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!((done.files, done.bytes, done.skipped_links), (2, 18, 1));
        assert_eq!(done.top_level, ["README.md", "src/"]);
        assert_eq!(
            std::fs::read_to_string(dir.join("github/a/b/src/lib.rs")).unwrap(),
            "pub fn f() {}\n"
        );

        assert!(
            download(&client, input())
                .unwrap_err()
                .message()
                .contains("replace: true")
        );
        std::fs::write(dir.join("github/a/b/stale.txt"), "old").unwrap();
        download(
            &client,
            DownloadInput {
                replace: true,
                ..input()
            },
        )
        .unwrap();
        assert!(!dir.join("github/a/b/stale.txt").exists());
    }

    /// Against the real GitHub through `curl`, with no token:
    /// `cargo test -- --ignored`.
    #[test]
    #[ignore]
    #[cfg(not(target_os = "wasi"))]
    fn live() {
        let dir = volume("live");
        let transport: client::Transport = Arc::new(|request: &Request| {
            let http::ResponseOutput::File { path } = &request.output else {
                unreachable!()
            };
            let mut curl = std::process::Command::new("curl");
            curl.args(["-sSL", "-o", path, "-D", "-", &request.url]);
            for (name, value) in &request.headers {
                curl.args(["-H", &format!("{name}: {value}")]);
            }
            let out = String::from_utf8(curl.output().unwrap().stdout).unwrap();
            // With -L the last header block is the final response's.
            let block = out.trim_end().rsplit("\r\n\r\n").next().unwrap();
            let mut lines = block.lines();
            let status = lines
                .next()
                .unwrap()
                .split(' ')
                .nth(1)
                .unwrap()
                .parse()
                .unwrap();
            let headers = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.to_lowercase(), v.trim().to_string()))
                .collect();
            let bytes = std::fs::metadata(path).unwrap().len();
            Ok(Response {
                status,
                headers,
                body: ResponseBody::File {
                    path: path.clone(),
                    bytes,
                },
            })
        });
        let client = Client::new(transport, dir.clone());
        let repo = "subako-ai/subako-function-template-rust";

        let info = api(&client, get(&format!("/repos/{repo}"))).unwrap();
        assert_eq!(info.body.unwrap()["full_name"], repo);
        println!("{:?}", info.rate_limit);
        let ReadFileReply::Dir { entries, .. } = read(&client, file(repo, "")).unwrap() else {
            panic!()
        };
        assert!(entries.iter().any(|e| e.name == "Cargo.toml"));
        let ReadFileReply::File { text, .. } = read(&client, file(repo, "Cargo.toml")).unwrap()
        else {
            panic!()
        };
        assert!(text.contains("[package]"));
        let done = download(
            &client,
            DownloadInput {
                repo: repo.into(),
                git_ref: None,
                dest: None,
                replace: false,
            },
        )
        .unwrap();
        println!("{done:?}");
        assert!(done.files > 3 && done.commit.is_some());
        assert!(
            read(&client, file("subako-ai/no-such-repo-here", ""))
                .unwrap_err()
                .message()
                .contains("404")
        );
    }
}
