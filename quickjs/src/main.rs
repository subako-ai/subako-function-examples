use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rquickjs::context::EvalOptions;
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::{Context, Ctx, Exception, Function, IntoJs, Module, Object, Runtime, TypedArray, Value};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use subako_function_sdk::{ToolError, function, http, package};

const PRELUDE: &str = include_str!("prelude.js");

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunInput {
    /// The JavaScript to run.
    source: Source,
    /// Arguments for the script, as the global `args`.
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Source {
    /// JavaScript given as a string.
    Code { code: String },
    /// A script on the Volume, under `/volume/tmp` or `/volume/data`. Relative paths it
    /// uses resolve from its directory.
    File { path: String },
}

#[derive(Debug, Serialize, JsonSchema)]
struct Ran {
    /// What the script wrote with `console.log`, `info` and `debug`.
    stdout: String,
    /// What it wrote with `console.warn` and `error`.
    stderr: String,
    /// What the script returned, as JSON. Absent when it returned nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    outcome: Outcome,
}

/// How the script ended.
#[derive(Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Outcome {
    /// It ran to the end, and every promise it awaited settled.
    Completed,
    /// It threw, or a promise it awaited rejected.
    Raised {
        /// The error's name and message.
        exception: String,
        /// The stack, as `at function (file:line:column)` lines.
        #[serde(skip_serializing_if = "String::is_empty")]
        stack: String,
    },
    /// It ran past the time limit and was stopped.
    Stopped { seconds: u64 },
}

/// Run JavaScript (ES2023, in QuickJS) and return what it printed and
/// returned. The script is the body of an async function: it can `await` and
/// `return` a value, which comes back as JSON. Available beyond the language:
/// `console`, `setTimeout`, `TextEncoder`/`TextDecoder`, `fetch` (HTTPS to
/// hosts the function's network setting allows), `fs` for files on the Volume
/// (`readText`, `readBytes`, `writeText`, `writeBytes`, `appendText`, `list`,
/// `exists`, `remove`), `args`, and `await import("./lib.js")` for other
/// script files. There is no `require`, no static `import`, and no npm.
#[function]
fn run_javascript(input: RunInput) -> Result<Ran, ToolError> {
    let host = Host {
        scratch: PathBuf::from("/volume/tmp"),
        fetch: Arc::new(http::request),
        time_limit: Duration::from_secs(60),
    };
    run(input, host)
}

package! {
    name: "javascript",
    description: "Runs JavaScript in QuickJS, with fetch and the Volume's files.",
    functions: [run_javascript],
}

/// Makes one request: `http::request` in the worker, a fake in tests.
type Fetch = Arc<dyn Fn(&http::Request) -> Result<http::Response, http::HttpError> + Send + Sync>;

/// What a run uses from outside the engine, so tests can supply their own.
struct Host {
    /// Where relative paths of text scripts resolve, and where request and
    /// response bodies pass through.
    scratch: PathBuf,
    fetch: Fetch,
    /// The script is stopped once it has run this long.
    time_limit: Duration,
}

fn run(input: RunInput, host: Host) -> Result<Ran, ToolError> {
    let (code, filename, base) = match input.source {
        Source::Code { code } => (code, "<script>".to_string(), host.scratch.clone()),
        Source::File { path } => {
            let code = std::fs::read_to_string(&path)
                .map_err(|e| ToolError::new(format!("cannot read {path}: {e}")))?;
            let base = Path::new(&path).parent().map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
            (code, path, base)
        }
    };

    let runtime = Runtime::new().map_err(engine_error)?;
    runtime.set_memory_limit(1 << 30);
    let deadline = Instant::now() + host.time_limit;
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    runtime.set_loader(Modules { base: base.clone() }, Modules { base: base.clone() });
    let context = Context::full(&runtime).map_err(engine_error)?;

    let started = context.with(|ctx| -> Result<Result<(), (String, String)>, ToolError> {
        install_host(&ctx, base, host.scratch, host.fetch, input.args).map_err(engine_error)?;
        if let Err(e) = evaluate(&ctx, PRELUDE, "prelude.js") {
            return Err(ToolError::new(format!("the prelude failed: {}", e.0)));
        }
        // The script runs as the body of an async function, opened on its
        // first line so its line numbers are its own.
        let run = format!(
            "(async function () {{{code}\n}})().then(\n\
               function (value) {{ globalThis.__subakoDone = true; globalThis.__subakoValue = value; }},\n\
               function (error) {{ globalThis.__subakoDone = true; globalThis.__subakoError = error; }});"
        );
        Ok(evaluate(&ctx, &run, &filename))
    })?;

    // Promise jobs first, then timers, until nothing is left to run.
    let mut stopped = Instant::now() > deadline;
    while !stopped {
        while let Ok(true) = runtime.execute_pending_job() {}
        match context.with(|ctx| ctx.eval::<bool, _>("__subakoRunTimer()")) {
            Ok(true) => {}
            Ok(false) => break,
            Err(_) => stopped = true,
        }
        stopped |= Instant::now() > deadline;
    }

    context.with(|ctx| {
        let globals = ctx.globals();
        let stdout: Vec<String> = globals.get("__subakoStdout").unwrap_or_default();
        let stderr: Vec<String> = globals.get("__subakoStderr").unwrap_or_default();
        let mut result = None;
        let outcome = if stopped || Instant::now() > deadline {
            Outcome::Stopped { seconds: host.time_limit.as_secs() }
        } else if let Err((exception, stack)) = started {
            Outcome::Raised { exception, stack }
        } else if !globals.get::<_, bool>("__subakoDone").unwrap_or(false) {
            Outcome::Raised {
                exception: "the script never finished: it awaited a promise that never settled".to_string(),
                stack: String::new(),
            }
        } else if let Ok(error) = globals.get::<_, Value>("__subakoError")
            && !error.is_undefined()
        {
            let (exception, stack) = describe(error);
            Outcome::Raised { exception, stack }
        } else {
            let value: Value = globals.get("__subakoValue").unwrap_or_else(|_| Value::new_undefined(ctx.clone()));
            result = to_json(&ctx, value);
            Outcome::Completed
        };
        Ok(Ran { stdout: lines(stdout), stderr: lines(stderr), result, outcome })
    })
}

fn lines(lines: Vec<String>) -> String {
    lines.into_iter().map(|line| line + "\n").collect()
}

fn engine_error(error: rquickjs::Error) -> ToolError {
    ToolError::new(format!("JavaScript engine: {error}"))
}

/// Runs `code` as a sloppy-mode script, returning a thrown error described.
fn evaluate(ctx: &Ctx<'_>, code: &str, filename: &str) -> Result<(), (String, String)> {
    let mut options = EvalOptions::default();
    options.strict = false;
    options.filename = Some(filename.to_string());
    match ctx.eval_with_options::<(), _>(code, options) {
        Ok(()) => Ok(()),
        Err(rquickjs::Error::Exception) => Err(describe(ctx.catch())),
        Err(e) => Err((e.to_string(), String::new())),
    }
}

/// A thrown value as `Name: message` and its stack, leaving out the
/// prelude's frames and the wrapper that calls the script (`<eval>`).
fn describe(error: Value<'_>) -> (String, String) {
    let Some(exception) = error.as_exception() else {
        let text = error
            .as_string()
            .and_then(|s| s.to_string().ok())
            .unwrap_or_else(|| format!("{error:?}"));
        return (format!("Uncaught {text}"), String::new());
    };
    let name: String = exception.get("name").unwrap_or_else(|_| "Error".to_string());
    let message = exception.message().unwrap_or_default();
    let stack: Vec<String> = exception
        .stack()
        .unwrap_or_default()
        .lines()
        .filter(|frame| !frame.contains("prelude.js") && !frame.contains("(native)") && !frame.contains("at <eval>"))
        .map(|frame| frame.trim().to_string())
        .filter(|frame| !frame.is_empty())
        .collect();
    (format!("{name}: {message}"), stack.join("\n"))
}

/// The value as JSON; `undefined` as nothing, and what JSON can't hold as
/// its string form.
fn to_json<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> Option<serde_json::Value> {
    if value.is_undefined() {
        return None;
    }
    let json = ctx.json_stringify(value.clone()).ok().flatten().and_then(|s| s.to_string().ok());
    match json.and_then(|json| serde_json::from_str(&json).ok()) {
        Some(json) => Some(json),
        None => {
            let text = ctx
                .globals()
                .get::<_, Function>("String")
                .and_then(|string| string.call::<_, String>((value,)))
                .unwrap_or_default();
            Some(serde_json::Value::String(text))
        }
    }
}

/// `path`, from `base` when relative, with `.` and `..` resolved by name:
/// WASI has no `canonicalize`.
fn resolve(base: &Path, path: &str) -> PathBuf {
    let mut resolved = PathBuf::new();
    for part in base.join(path).components() {
        match part {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            part => resolved.push(part),
        }
    }
    resolved
}

/// Loads `import()`ed files: relative names from the importing file,
/// absolute ones as they are.
struct Modules {
    base: PathBuf,
}

impl Resolver for Modules {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        importer: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        let from = if importer.starts_with('<') {
            self.base.clone()
        } else {
            Path::new(importer).parent().map_or_else(|| self.base.clone(), Path::to_path_buf)
        };
        if !(name.starts_with("./") || name.starts_with("../") || name.starts_with('/')) {
            return Err(rquickjs::Error::new_resolving_message(
                importer,
                name,
                "only files can be imported, by a path like ./lib.js; there are no packages",
            ));
        }
        Ok(resolve(&from, name).display().to_string())
    }
}

impl Loader for Modules {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js, Declared>> {
        let source = std::fs::read(name)
            .map_err(|e| rquickjs::Error::new_loading_message(name, e.to_string()))?;
        Module::declare(ctx.clone(), name, source)
    }
}

/// Bytes of a `Uint8Array`, or of a string as UTF-8.
fn bytes_of(value: &Value<'_>) -> Option<Vec<u8>> {
    if let Some(text) = value.as_string() {
        return text.to_string().ok().map(String::into_bytes);
    }
    let array = TypedArray::<u8>::from_value(value.clone()).ok()?;
    // SAFETY: no JavaScript runs while the slice is borrowed.
    Some(unsafe { array.as_slice() }.to_vec())
}

fn scratch_path(dir: &Path, what: &str) -> PathBuf {
    // WASI has no process ids, and one call is one process, so the time will do.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    dir.join(format!(".js-{what}-{nanos}"))
}

/// The `__subako*` functions prelude.js builds on, and `args`.
fn install_host(ctx: &Ctx<'_>, base: PathBuf, scratch: PathBuf, fetch: Fetch, args: Vec<String>) -> rquickjs::Result<()> {
    let globals = ctx.globals();
    globals.set("args", args)?;
    let throw = |ctx: &Ctx<'_>, message: String| Exception::throw_message(ctx, &message);

    globals.set(
        "__subakoEncode",
        Function::new(ctx.clone(), |text: String| Bytes(text.into_bytes()))?,
    )?;
    globals.set(
        "__subakoDecode",
        Function::new(ctx.clone(), |bytes: Value<'_>| {
            bytes_of(&bytes).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
        })?,
    )?;

    let dir = base.clone();
    globals.set(
        "__subakoRead",
        Function::new(ctx.clone(), move |ctx: Ctx<'_>, path: String| {
            let real = resolve(&dir, &path);
            std::fs::read(&real)
                .map(Bytes)
                .map_err(|e| throw(&ctx, format!("cannot read {}: {e}", real.display())))
        })?,
    )?;
    let dir = base.clone();
    globals.set(
        "__subakoWrite",
        Function::new(ctx.clone(), move |ctx: Ctx<'_>, path: String, data: Value<'_>, append: bool| {
            let real = resolve(&dir, &path);
            let bytes = bytes_of(&data).ok_or_else(|| throw(&ctx, "write takes a string or a Uint8Array".to_string()))?;
            let written = real
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| {
                    use std::io::Write;
                    let mut file = std::fs::OpenOptions::new()
                        .create(true)
                        .write(true)
                        .append(append)
                        .truncate(!append)
                        .open(&real)?;
                    file.write_all(&bytes)
                });
            written.map_err(|e| throw(&ctx, format!("cannot write {}: {e}", real.display())))
        })?,
    )?;
    let dir = base.clone();
    globals.set(
        "__subakoList",
        Function::new(ctx.clone(), move |ctx: Ctx<'_>, path: String| {
            let real = resolve(&dir, &path);
            let entries = std::fs::read_dir(&real).map_err(|e| throw(&ctx, format!("cannot list {}: {e}", real.display())))?;
            let mut names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            names.sort();
            Ok::<_, rquickjs::Error>(names)
        })?,
    )?;
    let dir = base.clone();
    globals.set(
        "__subakoExists",
        Function::new(ctx.clone(), move |path: String| std::fs::symlink_metadata(resolve(&dir, &path)).is_ok())?,
    )?;
    let dir = base;
    globals.set(
        "__subakoRemove",
        Function::new(ctx.clone(), move |ctx: Ctx<'_>, path: String| {
            let real = resolve(&dir, &path);
            let removed = match std::fs::symlink_metadata(&real) {
                Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&real),
                Ok(_) => std::fs::remove_file(&real),
                Err(e) => Err(e),
            };
            removed.map_err(|e| throw(&ctx, format!("cannot remove {}: {e}", real.display())))
        })?,
    )?;

    globals.set(
        "__subakoFetch",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'_>, url: String, method: String, headers: HashMap<String, String>, body: Value<'_>, follow: bool| {
                fetch_once(&scratch, &fetch, url, method, headers, &body, follow)
                    .map_err(|message| throw(&ctx, message))
            },
        )?,
    )?;
    Ok(())
}

/// Bytes that reach JavaScript as a `Uint8Array`.
struct Bytes(Vec<u8>);

impl<'js> IntoJs<'js> for Bytes {
    fn into_js(self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
        TypedArray::<u8>::new(ctx.clone(), self.0).map(TypedArray::into_value)
    }
}

/// A response as prelude.js's `fetch` takes it.
struct Answer {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Bytes,
}

impl<'js> IntoJs<'js> for Answer {
    fn into_js(self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
        let answer = Object::new(ctx.clone())?;
        answer.set("status", self.status)?;
        answer.set("headers", self.headers)?;
        answer.set("body", self.body)?;
        Ok(answer.into_value())
    }
}

/// One `fetch`: the body passes through a file, so binary bodies work.
fn fetch_once(
    scratch: &Path,
    fetch: &Fetch,
    url: String,
    method: String,
    headers: HashMap<String, String>,
    body: &Value<'_>,
    follow: bool,
) -> Result<Answer, String> {
    let method: http::Method = serde_json::from_value(serde_json::Value::String(method.clone()))
        .map_err(|_| format!("method {method} is not supported"))?;
    let mut request = http::Request::get(url);
    request.method = method;
    request.headers = headers.into_iter().collect::<BTreeMap<_, _>>();
    request.follow_redirects = follow;
    let upload = scratch_path(scratch, "upload");
    if !body.is_undefined() {
        let bytes = bytes_of(body).ok_or("the body must be a string, an ArrayBuffer or a typed array")?;
        request.body = Some(match String::from_utf8(bytes) {
            Ok(text) => http::RequestBody::Text { text },
            Err(e) => {
                std::fs::write(&upload, e.into_bytes()).map_err(|e| e.to_string())?;
                http::RequestBody::File { path: upload.display().to_string() }
            }
        });
    }
    let download = scratch_path(scratch, "response");
    request.output = http::ResponseOutput::File { path: download.display().to_string() };
    let response = fetch(&request);
    let _ = std::fs::remove_file(&upload);
    let response = response.map_err(|e| e.message)?;
    let bytes = std::fs::read(&download).unwrap_or_default();
    let _ = std::fs::remove_file(&download);
    Ok(Answer { status: response.status, headers: response.headers, body: Bytes(bytes) })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// The repository's tests/ directory; in wasmtime, mounted at /tests.
    fn fixtures() -> PathBuf {
        if cfg!(target_os = "wasi") {
            PathBuf::from("/tests")
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
        }
    }

    /// A fresh scratch directory and a worker that must not be called.
    fn host(name: &str) -> Host {
        // WASI has no `temp_dir`; the wasmtime runner maps target/ to /volume/tmp.
        let base = if cfg!(target_os = "wasi") { PathBuf::from("/volume/tmp") } else { std::env::temp_dir() };
        let scratch = base.join(format!("quickjs-function-test-{name}"));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        Host {
            scratch,
            fetch: Arc::new(|request: &http::Request| panic!("unexpected fetch of {}", request.url)),
            time_limit: Duration::from_secs(60),
        }
    }

    fn js(code: &str, host: Host) -> Ran {
        run(RunInput { source: Source::Code { code: code.to_string() }, args: Vec::new() }, host).unwrap()
    }

    #[test]
    fn prints_and_returns() {
        let ran = js(
            "console.log('hello', 42, { a: [1, 2] });\n\
             console.error('careful');\n\
             const words = ['b', 'a', 'c'].toSorted();\n\
             return { words, n: 2n ** 64n > 0n };",
            host("basic"),
        );
        assert_eq!(ran.stdout, "hello 42 {\"a\":[1,2]}\n");
        assert_eq!(ran.stderr, "careful\n");
        assert_eq!(ran.result, Some(serde_json::json!({ "words": ["a", "b", "c"], "n": true })));
        assert_eq!(ran.outcome, Outcome::Completed);
    }

    #[test]
    fn waits_for_timers_and_promises() {
        let ran = js(
            "const order = [];\n\
             setTimeout(() => order.push('timer'), 100);\n\
             await new Promise((resolve) => setTimeout(resolve, 0));\n\
             order.push('awaited');\n\
             await null;\n\
             return order;",
            host("async"),
        );
        assert_eq!(ran.result, Some(serde_json::json!(["timer", "awaited"])));
    }

    #[test]
    fn reports_errors_with_their_line() {
        let ran = js("console.log('before');\nconst x = null;\nx.y;", host("error"));
        assert_eq!(ran.stdout, "before\n");
        let Outcome::Raised { exception, stack } = ran.outcome else { panic!("{:?}", ran.outcome) };
        assert_eq!(exception, "TypeError: cannot read property 'y' of null");
        assert!(stack.contains("<script>:3"), "{stack}");
        assert!(!stack.contains("<eval>"), "{stack}");

        let ran = js("throw 'just a string';", host("throw"));
        assert_eq!(ran.outcome, Outcome::Raised { exception: "Uncaught just a string".to_string(), stack: String::new() });

        let ran = js("let x = ;", host("syntax"));
        let Outcome::Raised { exception, .. } = ran.outcome else { panic!("{:?}", ran.outcome) };
        assert!(exception.starts_with("SyntaxError"), "{exception}");
    }

    #[test]
    fn runs_files_that_import_modules() {
        let input = RunInput {
            source: Source::File { path: fixtures().join("scripts/main.js").display().to_string() },
            args: vec!["a".to_string(), "b".to_string()],
        };
        let ran = run(input, host("modules")).unwrap();
        assert_eq!(ran.outcome, Outcome::Completed, "{ran:?}");
        assert_eq!(ran.stdout, "args: a b\n");
        assert_eq!(ran.result, Some(serde_json::json!({ "total": 10 })));
    }

    #[test]
    fn imports_only_files() {
        let ran = js("await import('lodash');", host("packages"));
        let Outcome::Raised { exception, .. } = ran.outcome else { panic!("{:?}", ran.outcome) };
        assert!(exception.contains("only files can be imported"), "{exception}");
    }

    #[test]
    fn reads_and_writes_files() {
        let host = host("files");
        let dir = host.scratch.clone();
        let ran = js(
            "fs.writeText('out/a.txt', 'one\\n');\n\
             fs.appendText('out/a.txt', 'two\\n');\n\
             fs.writeBytes('out/b.bin', new Uint8Array([0, 255]));\n\
             const bytes = fs.readBytes('out/b.bin');\n\
             return { text: fs.readText('out/a.txt'), bytes: [...bytes], list: fs.list('out'),\n  \
               exists: [fs.exists('out/a.txt'), fs.exists('nope')],\n  \
               encoded: [...new TextEncoder().encode('é')], decoded: new TextDecoder().decode(new Uint8Array([104, 105])) };",
            host,
        );
        assert_eq!(
            ran.result,
            Some(serde_json::json!({
                "text": "one\ntwo\n", "bytes": [0, 255], "list": ["a.txt", "b.bin"],
                "exists": [true, false], "encoded": [195, 169], "decoded": "hi"
            })),
            "{ran:?}"
        );
        assert_eq!(std::fs::read(dir.join("out/b.bin")).unwrap(), [0, 255]);
    }

    #[test]
    fn fetches_through_the_worker() {
        let mut host = host("fetch");
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        host.fetch = Arc::new(move |request: &http::Request| {
            log.lock().unwrap().push(request.clone());
            if request.url.contains("blocked") {
                return Err(http::HttpError { kind: http::HttpErrorKind::HostNotAllowed, message: "blocked.example is not allowed".to_string() });
            }
            let http::ResponseOutput::File { path } = &request.output else { panic!("fetch asks for a file") };
            std::fs::write(path, br#"{"items":[1,2,3]}"#).unwrap();
            Ok(http::Response {
                status: 200,
                headers: [("content-type".to_string(), "application/json".to_string())].into(),
                body: http::ResponseBody::File { path: path.clone(), bytes: 17 },
            })
        });
        let dir = host.scratch.clone();
        let ran = js(
            "const response = await fetch('https://api.example.com/items', {\n  \
               method: 'post', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ q: 1 }) });\n\
             const data = await response.json();\n\
             let blocked;\n\
             try { await fetch('https://blocked.example'); } catch (e) { blocked = e.message; }\n\
             return { ok: response.ok, type: response.headers.get('Content-Type'), count: data.items.length, blocked };",
            host,
        );
        assert_eq!(
            ran.result,
            Some(serde_json::json!({
                "ok": true, "type": "application/json", "count": 3,
                "blocked": "fetch failed: blocked.example is not allowed"
            })),
            "{ran:?}"
        );
        let sent = sent.lock().unwrap();
        assert_eq!(sent[0].method, http::Method::Post);
        assert_eq!(sent[0].body, Some(http::RequestBody::Text { text: "{\"q\":1}".to_string() }));
        assert!(sent[0].follow_redirects);
        // Bodies passed through the scratch directory and are gone.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn stops_scripts_that_run_too_long() {
        let mut host = host("loop");
        host.time_limit = Duration::from_millis(200);
        let ran = js("console.log('started');\nwhile (true) {}", host);
        assert_eq!(ran.outcome, Outcome::Stopped { seconds: 0 });
        assert_eq!(ran.stdout, "started\n");
    }

    #[test]
    fn notices_promises_that_never_settle() {
        let ran = js("await new Promise(() => {});", host("pending"));
        let Outcome::Raised { exception, .. } = ran.outcome else { panic!("{:?}", ran.outcome) };
        assert!(exception.contains("never finished"), "{exception}");
    }
}
