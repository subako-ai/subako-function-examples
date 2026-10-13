//! A limited `curl`, sent through the Subako worker instead of sockets.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bashkit::{Builtin, BuiltinContext, ExecResult, async_trait};
use subako_function_sdk::http::{self, HttpErrorKind, Method, Request, RequestBody, ResponseOutput};

/// Makes one request: `http::request` in the worker, a fake in tests.
pub type Transport = Arc<dyn Fn(&Request) -> Result<http::Response, http::HttpError> + Send + Sync>;

const USAGE: &str = "supported options: -X/--request, -H/--header, -d/--data, --data-raw, \
                     --data-binary, --json, -o/--output, -i/--include, -I/--head, -L/--location, \
                     -f/--fail, -s/--silent, -S/--show-error, -w/--write-out, -m/--max-time, \
                     -A/--user-agent";

pub struct Curl {
    pub transport: Transport,
    /// A real directory for response and upload bodies in flight, which
    /// the worker reads and writes as files.
    pub scratch: PathBuf,
}

#[derive(Default)]
struct Options {
    url: Option<String>,
    method: Option<String>,
    headers: Vec<(String, String)>,
    data: Vec<String>,
    json: Option<String>,
    output: Option<String>,
    include: bool,
    head: bool,
    location: bool,
    fail: bool,
    silent: bool,
    show_error: bool,
    write_out: Option<String>,
    max_time: Option<f64>,
}

/// Options that take a value, by short and long name.
fn takes_value(name: &str) -> bool {
    matches!(
        name,
        "X" | "H" | "d" | "o" | "w" | "m" | "A" | "request" | "header" | "data" | "data-raw"
            | "data-binary" | "json" | "output" | "write-out" | "max-time" | "user-agent"
    )
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        // `-fsSL` is four flags; `-XPOST` is a flag and its value.
        let flags: Vec<(String, Option<String>)> = if let Some(long) = arg.strip_prefix("--") {
            vec![(long.to_string(), None)]
        } else if let Some(short) = arg.strip_prefix('-').filter(|s| !s.is_empty()) {
            let mut flags = Vec::new();
            for (i, c) in short.char_indices() {
                let name = c.to_string();
                if takes_value(&name) {
                    let rest = &short[i + c.len_utf8()..];
                    flags.push((name, (!rest.is_empty()).then(|| rest.to_string())));
                    break;
                }
                flags.push((name, None));
            }
            flags
        } else {
            if options.url.replace(arg.clone()).is_some() {
                return Err("only one URL per call is supported".to_string());
            }
            continue;
        };
        for (name, inline) in flags {
            let mut value = || {
                inline
                    .clone()
                    .or_else(|| args.next().cloned())
                    .ok_or_else(|| format!("option -{name} needs a value"))
            };
            match name.as_str() {
                "X" | "request" => options.method = Some(value()?.to_uppercase()),
                "H" | "header" => {
                    let header = value()?;
                    let (k, v) = header
                        .split_once(':')
                        .ok_or_else(|| format!("bad header: {header}"))?;
                    options.headers.push((k.trim().to_string(), v.trim().to_string()));
                }
                "A" | "user-agent" => options.headers.push(("User-Agent".to_string(), value()?)),
                "d" | "data" | "data-binary" => options.data.push(value()?),
                // `--data-raw` never reads `@file`.
                "data-raw" => options.data.push(format!("\u{0}{}", value()?)),
                "json" => options.json = Some(value()?),
                "o" | "output" => options.output = Some(value()?),
                "w" | "write-out" => options.write_out = Some(value()?),
                "m" | "max-time" => {
                    let secs = value()?;
                    options.max_time =
                        Some(secs.parse().map_err(|_| format!("bad --max-time: {secs}"))?);
                }
                "i" | "include" => options.include = true,
                "I" | "head" => options.head = true,
                "L" | "location" => options.location = true,
                "f" | "fail" => options.fail = true,
                "s" | "silent" => options.silent = true,
                "S" | "show-error" => options.show_error = true,
                other => {
                    let dash = if other.len() == 1 { "-" } else { "--" };
                    return Err(format!("option {dash}{other} is not supported; {USAGE}"));
                }
            }
        }
    }
    Ok(options)
}

/// The result of a failed call, with the message unless `-s` hides it.
fn failure(options: &Options, code: i32, message: &str) -> ExecResult {
    let shown = !options.silent || options.show_error;
    let stderr = if shown { format!("curl: ({code}) {message}\n") } else { String::new() };
    ExecResult::err(stderr, code)
}

/// curl's exit code for each way the worker can fail a request.
fn exit_code(kind: HttpErrorKind) -> i32 {
    match kind {
        HttpErrorKind::InvalidRequest => 3,
        HttpErrorKind::Timeout => 28,
        HttpErrorKind::TooManyRedirects => 47,
        HttpErrorKind::WriteBudget | HttpErrorKind::BadPath => 23,
        HttpErrorKind::TooLarge => 63,
        _ => 7,
    }
}

fn scratch_path(dir: &Path, what: &str) -> PathBuf {
    // WASI has no process ids, and one call is one process, so the time will do.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    dir.join(format!(".curl-{what}-{nanos}"))
}

#[async_trait]
impl Builtin for Curl {
    async fn execute(&self, ctx: BuiltinContext<'_>) -> bashkit::Result<ExecResult> {
        let options = match parse(ctx.args) {
            Ok(options) => options,
            Err(message) => return Ok(ExecResult::err(format!("curl: {message}\n"), 2)),
        };
        let Some(url) = options.url.clone() else {
            return Ok(ExecResult::err(format!("curl: no URL given; {USAGE}\n"), 2));
        };

        // The body: `-d` values joined with `&` as curl does, `@file` read
        // from the shell's filesystem, or `--json`.
        let mut headers: Vec<(String, String)> = Vec::new();
        let mut body = None;
        if let Some(json) = &options.json {
            headers.push(("Content-Type".to_string(), "application/json".to_string()));
            headers.push(("Accept".to_string(), "application/json".to_string()));
            body = Some(json.clone().into_bytes());
        } else if !options.data.is_empty() {
            let mut parts = Vec::new();
            for data in &options.data {
                if let Some(raw) = data.strip_prefix('\u{0}') {
                    parts.push(raw.as_bytes().to_vec());
                } else if let Some(file) = data.strip_prefix('@') {
                    let path = ctx.cwd.join(file);
                    match ctx.fs.read_file(&path).await {
                        Ok(bytes) => parts.push(bytes),
                        Err(e) => return Ok(failure(&options, 26, &format!("cannot read {file}: {e}"))),
                    }
                } else {
                    parts.push(data.as_bytes().to_vec());
                }
            }
            headers.push((
                "Content-Type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            ));
            body = Some(parts.join(&b'&'));
        }
        // Explicit headers win over the defaults above.
        for (name, value) in &options.headers {
            headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
            headers.push((name.clone(), value.clone()));
        }

        let method = match (&options.method, options.head, body.is_some()) {
            (Some(method), _, _) => method.clone(),
            (None, true, _) => "HEAD".to_string(),
            (None, false, true) => "POST".to_string(),
            (None, false, false) => "GET".to_string(),
        };
        let Ok(method) = serde_json::from_value::<Method>(serde_json::Value::String(method.clone()))
        else {
            return Ok(failure(&options, 3, &format!("method {method} is not supported")));
        };

        let mut request = Request::get(url.clone());
        request.method = method;
        request.headers = headers.into_iter().collect();
        request.follow_redirects = options.location;
        request.timeout_ms = options.max_time.map(|secs| (secs * 1000.0).max(1.0) as u64);
        let response_path = scratch_path(&self.scratch, "response");
        request.output = ResponseOutput::File { path: response_path.display().to_string() };
        // Only text travels inline, so other bodies go through a file.
        let upload_path = scratch_path(&self.scratch, "upload");
        request.body = match body.map(String::from_utf8) {
            None => None,
            Some(Ok(text)) => Some(RequestBody::Text { text }),
            Some(Err(e)) => {
                std::fs::write(&upload_path, e.into_bytes())?;
                Some(RequestBody::File { path: upload_path.display().to_string() })
            }
        };

        let response = (self.transport)(&request);
        let _ = std::fs::remove_file(&upload_path);
        let response = match response {
            Ok(response) => response,
            Err(error) => return Ok(failure(&options, exit_code(error.kind), &error.message)),
        };
        let content = std::fs::read(&response_path).unwrap_or_default();
        let _ = std::fs::remove_file(&response_path);

        if options.fail && response.status >= 400 {
            let message = format!("The requested URL returned error: {}", response.status);
            return Ok(failure(&options, 22, &message));
        }

        let mut stdout = Vec::new();
        if options.include || options.head {
            stdout.extend(format!("HTTP/1.1 {}\r\n", response.status).bytes());
            for (name, value) in &response.headers {
                stdout.extend(format!("{name}: {value}\r\n").bytes());
            }
            stdout.extend(b"\r\n");
        }
        match &options.output {
            Some(file) => ctx.fs.write_file(&ctx.cwd.join(file), &content).await?,
            None if !options.head => stdout.extend(&content),
            None => {}
        }
        if let Some(format) = &options.write_out {
            let written = format
                .replace("%{http_code}", &response.status.to_string())
                .replace("%{response_code}", &response.status.to_string())
                .replace("%{size_download}", &content.len().to_string())
                .replace("%{url_effective}", &url)
                .replace("\\n", "\n")
                .replace("\\t", "\t");
            stdout.extend(written.bytes());
        }
        Ok(ExecResult::ok_bytes(stdout))
    }

    fn llm_hint(&self) -> Option<&'static str> {
        Some(
            "curl: HTTPS through the Subako worker, to hosts the function's network setting \
             allows. One URL per call; -X -H -d --json -o -i -I -L -f -s -S -w -m -A.",
        )
    }
}
