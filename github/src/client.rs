//! GET requests to the GitHub REST API through the Subako worker. The worker
//! attaches a stored token for api.github.com when the function has one, so
//! this code never sees or sends credentials itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use subako_function_sdk::ToolError;
use subako_function_sdk::http::{
    self, HttpError, HttpErrorKind, Request, Response, ResponseOutput,
};

pub const API: &str = "https://api.github.com";

/// Makes one request: `http::request` in the worker, a fake in tests.
pub type Transport = Arc<dyn Fn(&Request) -> Result<Response, HttpError> + Send + Sync>;

pub struct Client {
    transport: Transport,
    /// `/volume/tmp`, where large responses and downloads go by default.
    pub tmp: PathBuf,
    /// A real directory for response bodies in flight, which the worker
    /// writes as files.
    scratch: PathBuf,
}

/// A successful response, its body in a scratch file that is removed when
/// this is dropped unless it was moved elsewhere.
pub struct Reply {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: PathBuf,
}

impl Drop for Reply {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.body);
    }
}

impl Reply {
    pub fn read(&self) -> Result<Vec<u8>, ToolError> {
        std::fs::read(&self.body)
            .map_err(|e| ToolError::new(format!("cannot read the response: {e}")))
    }

    /// Moves the body to `path`, creating its directory.
    pub fn save(&self, path: &Path) -> Result<u64, ToolError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| ToolError::new(format!("cannot create {}: {e}", dir.display())))?;
        }
        // A rename fails across mounts, so fall back to a copy.
        if std::fs::rename(&self.body, path).is_err() {
            std::fs::copy(&self.body, path)
                .map_err(|e| ToolError::new(format!("cannot write {}: {e}", path.display())))?;
        }
        std::fs::metadata(path)
            .map(|m| m.len())
            .map_err(|e| ToolError::new(format!("cannot read {}: {e}", path.display())))
    }

    pub fn content_type(&self) -> &str {
        self.headers.get("content-type").map_or("", String::as_str)
    }

    pub fn rate_limit(&self) -> Option<RateLimit> {
        RateLimit::from_headers(&self.headers)
    }

    /// The `next` link of a paged list, as a path for `github_api`.
    pub fn next_page(&self) -> Option<String> {
        let link = self.headers.get("link")?;
        link.split(',').find_map(|part| {
            let (url, rel) = part.split_once(';')?;
            if !rel.contains("rel=\"next\"") {
                return None;
            }
            let url = url.trim().strip_prefix('<')?.strip_suffix('>')?;
            Some(url.strip_prefix(API).unwrap_or(url).to_string())
        })
    }
}

/// How many requests are left before GitHub refuses more.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RateLimit {
    /// Which limit: `core` for most of the API, `search` for search.
    pub resource: String,
    /// Requests allowed per hour (per minute for search): 60 for `core`
    /// without a token, 5,000 with one.
    pub limit: u64,
    pub remaining: u64,
    /// Minutes until `remaining` is back to `limit`.
    pub resets_in_minutes: u64,
}

impl RateLimit {
    fn from_headers(headers: &BTreeMap<String, String>) -> Option<RateLimit> {
        let number = |name: &str| headers.get(name)?.trim().parse::<u64>().ok();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Some(RateLimit {
            resource: headers
                .get("x-ratelimit-resource")
                .cloned()
                .unwrap_or_else(|| "core".into()),
            limit: number("x-ratelimit-limit")?,
            remaining: number("x-ratelimit-remaining")?,
            resets_in_minutes: number("x-ratelimit-reset")
                .map_or(0, |reset| reset.saturating_sub(now).div_ceil(60)),
        })
    }
}

impl Client {
    pub fn new(transport: Transport, tmp: PathBuf) -> Self {
        Client {
            transport,
            scratch: tmp.join(".github-function"),
            tmp,
        }
    }

    /// The worker's client.
    pub fn worker() -> Self {
        Client::new(Arc::new(http::request), PathBuf::from("/volume/tmp"))
    }

    /// GETs `path` (such as `/repos/rust-lang/rust`) from the API, or a full
    /// api.github.com URL. Statuses other than 2xx are errors that say what
    /// GitHub said and what likely went wrong.
    pub fn get(&self, path: &str, accept: &str) -> Result<Reply, ToolError> {
        let url = api_url(path)?;
        static NEXT: AtomicU32 = AtomicU32::new(0);
        std::fs::create_dir_all(&self.scratch).map_err(|e| {
            ToolError::new(format!("cannot create {}: {e}", self.scratch.display()))
        })?;
        let body = self
            .scratch
            .join(format!("response-{}", NEXT.fetch_add(1, Ordering::Relaxed)));

        let mut request = Request::get(&url);
        request.output = ResponseOutput::File {
            path: body.to_string_lossy().into_owned(),
        };
        for (name, value) in [
            ("accept", accept),
            ("user-agent", "subako-github-function"),
            ("x-github-api-version", "2022-11-28"),
        ] {
            request.headers.insert(name.to_string(), value.to_string());
        }
        let response = (self.transport)(&request).map_err(|e| {
            let _ = std::fs::remove_file(&body);
            request_error(&url, e)
        })?;
        let reply = Reply {
            status: response.status,
            headers: response.headers,
            body,
        };
        if (200..300).contains(&reply.status) {
            return Ok(reply);
        }
        Err(status_error(&reply))
    }
}

/// `path` as a URL on api.github.com; other hosts are refused.
fn api_url(path: &str) -> Result<String, ToolError> {
    let path = path.trim();
    if let Some(rest) = path.strip_prefix(API) {
        if rest.is_empty() || rest.starts_with('/') {
            return Ok(path.to_string());
        }
    } else if path.starts_with('/') {
        return Ok(format!("{API}{path}"));
    } else if !path.contains("://") {
        return Ok(format!("{API}/{path}"));
    }
    Err(ToolError::new(format!(
        "{path} is not on api.github.com; give a path such as /repos/OWNER/REPO"
    )))
}

fn request_error(url: &str, error: HttpError) -> ToolError {
    let hint = match error.kind {
        HttpErrorKind::HostNotAllowed => {
            " The function's network setting in Subako must allow api.github.com (and codeload.github.com \
             for download_repo)."
        }
        HttpErrorKind::CredentialUnavailable => {
            " A token is set up for this function but none is usable in this session's vaults."
        }
        _ => "",
    };
    ToolError::new(format!(
        "GET {url} failed: {}.{hint}",
        error.message.trim_end_matches('.')
    ))
}

fn status_error(reply: &Reply) -> ToolError {
    let body = std::fs::read(&reply.body).unwrap_or_default();
    // GitHub's errors are JSON with a `message`.
    let said = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("message")?.as_str().map(str::to_string))
        .unwrap_or_else(|| String::from_utf8_lossy(&body).chars().take(300).collect());
    let said = said.trim().trim_end_matches('.');
    let limit = reply.rate_limit();
    let hint = match reply.status {
        403 | 429 if limit.as_ref().is_some_and(|l| l.remaining == 0) => {
            let l = limit.as_ref().unwrap();
            format!(
                " The {} rate limit of {} requests is used up; it resets in {} minutes.{}",
                l.resource,
                l.limit,
                l.resets_in_minutes,
                if l.resource == "core" && l.limit <= 60 { " A token raises it to 5,000 an hour." } else { "" }
            )
        }
        401 => " The token for api.github.com was refused; it may be wrong or expired.".to_string(),
        404 => " GitHub also answers 404 for a private repository when there's no token that can see it.".to_string(),
        _ => String::new(),
    };
    ToolError::new(format!("GitHub answered {}: {said}.{hint}", reply.status))
}

/// Percent-encodes `text` for a URL path segment or query value.
pub fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_api_urls() {
        assert_eq!(
            api_url("/repos/a/b").unwrap(),
            "https://api.github.com/repos/a/b"
        );
        assert_eq!(
            api_url("repos/a/b?x=1").unwrap(),
            "https://api.github.com/repos/a/b?x=1"
        );
        assert_eq!(
            api_url("https://api.github.com/user").unwrap(),
            "https://api.github.com/user"
        );
        assert!(api_url("https://example.com/repos").is_err());
        assert!(api_url("https://api.github.com.evil.com/x").is_err());
    }

    #[test]
    fn encodes_path_segments() {
        assert_eq!(encode("a b#c/d%é"), "a%20b%23c%2Fd%25%C3%A9");
    }
}
