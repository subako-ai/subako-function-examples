use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rquickjs::context::EvalOptions;
use rquickjs::{Context, Ctx, Exception, Function, Runtime, TypedArray};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use subako_function_sdk::{ToolError, function, http, package};

const PPTXGENJS: &str = include_str!("../vendor/pptxgen.bundle.js");
const PRELUDE: &str = include_str!("prelude.js");
const SETUP: &str = include_str!("setup.js");
const LOGO_INK: &[u8] = include_bytes!("../brand/logo-ink.png");
const LOGO_PAPER: &[u8] = include_bytes!("../brand/logo-paper.png");

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateInput {
    /// The JavaScript that builds the deck.
    source: Source,
    /// Volume path to write the `.pptx` to, such as `/volume/tmp/deck.pptx`.
    output: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Source {
    /// JavaScript source given as a string.
    Code { code: String },
    /// A `.js` file on the Volume. Relative paths it passes to `image()` and
    /// `readText()` resolve from its directory.
    File { path: String },
}

#[derive(Debug, Serialize, JsonSchema)]
struct Created {
    /// Where the deck was written.
    path: String,
    slides: usize,
    bytes: usize,
    /// What the script passed to `console`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    logs: Vec<String>,
}

/// Build a PowerPoint deck by running JavaScript against PptxGenJS 4 (in
/// QuickJS) and write it to the Volume. The script gets `pptx`, a
/// PptxGenJS instance at 16:9 (10 x 5.625 in), to fill with
/// `pptx.addSlide()`, `slide.addText`, `addTable`, `addChart`, `addShape` and
/// `addImage`; the deck is written when the script ends, so don't call
/// `writeFile`. It may `await`, and may return another PptxGenJS instance to
/// write that instead. Subako slide masters are defined: `SUBAKO_TITLE`,
/// `SUBAKO_SECTION` and `SUBAKO_CONTENT` (placeholders `title`, `subtitle` and
/// `body`), with `subako.colors` and `subako.fonts` to match. For images use
/// `addImage({ data: image("/volume/tmp/chart.png") })`: `image()` reads a PNG, JPEG
/// or GIF on the Volume or fetches an https URL; SVG isn't supported.
/// `readText(path)` reads a text file. Relative paths resolve from `/volume/tmp`.
#[function]
fn create_pptx(input: CreateInput) -> Result<Created, ToolError> {
    let fetch: Fetch = Arc::new(|url: &str, dest: &Path| {
        let mut request = http::Request::get(url);
        request.output = http::ResponseOutput::File { path: dest.display().to_string() };
        let response = http::request(&request).map_err(|e| e.message)?;
        Ok((response.status, response.headers.get("content-type").cloned()))
    });
    let host = Host { scratch: PathBuf::from("/volume/tmp"), fetch, time_limit: Duration::from_secs(60) };
    create(input, host)
}

package! {
    name: "pptx",
    description: "Builds PowerPoint decks from JavaScript with PptxGenJS, with Subako slide masters.",
    functions: [create_pptx],
}

/// Downloads `url` to the real file `dest`, returning the status and the
/// content type: through the worker in the function, a fake in tests.
type Fetch = Arc<dyn Fn(&str, &Path) -> Result<(u16, Option<String>), String> + Send + Sync>;

/// What a run uses from outside the engine, so tests can supply their own.
struct Host {
    /// Where relative paths resolve, and where downloads pass through.
    scratch: PathBuf,
    fetch: Fetch,
    /// The script is stopped once it has run this long.
    time_limit: Duration,
}

fn create(input: CreateInput, host: Host) -> Result<Created, ToolError> {
    let (code, filename, base) = match input.source {
        Source::Code { code } => (code, "<script>".to_string(), host.scratch.clone()),
        Source::File { path } => {
            let code = std::fs::read_to_string(&path)
                .map_err(|e| ToolError::new(format!("cannot read {path}: {e}")))?;
            let base = Path::new(&path).parent().map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
            (code, path, base)
        }
    };

    let runtime = Runtime::new().map_err(js_error)?;
    runtime.set_memory_limit(1 << 30);
    let deadline = Instant::now() + host.time_limit;
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let context = Context::full(&runtime).map_err(js_error)?;

    // Past the deadline, every failure is the interrupt, however it surfaced.
    let stopped = |result: Result<(), ToolError>| {
        result.map_err(|e| if Instant::now() > deadline { ToolError::new(stopped_message(host.time_limit)) } else { e })
    };

    stopped(context.with(|ctx| -> Result<(), ToolError> {
        install_host(&ctx, base, host.scratch, host.fetch).map_err(js_error)?;
        evaluate(&ctx, PRELUDE, "prelude.js")?;
        evaluate(&ctx, PPTXGENJS, "pptxgen.bundle.js")?;
        evaluate(&ctx, SETUP, "setup.js")?;
        // The script runs as the body of an async function, opened on its
        // first line so its line numbers are its own, then the deck it
        // leaves is written.
        let run = format!(
            "(async function () {{{code}\n}})()\n\
             .then(async function (deck) {{\n\
               if (!(deck instanceof PptxGenJS)) deck = pptx;\n\
               var bytes = await deck.write({{ outputType: \"uint8array\" }});\n\
               globalThis.__subakoResult = {{ bytes: bytes, slides: deck.slides.length }};\n\
             }})\n\
             .catch(function (e) {{ globalThis.__subakoError = e; }});"
        );
        evaluate(&ctx, &run, &filename)
    }))?;

    // Promise jobs first, then timers, until nothing is left to run.
    loop {
        loop {
            match runtime.execute_pending_job() {
                Ok(true) => continue,
                Ok(false) => break,
                Err(_) => break,
            }
        }
        let ran_timer = context.with(|ctx| ctx.eval::<bool, _>("__subakoRunTimer()"));
        match ran_timer {
            Ok(true) => continue,
            Ok(false) => break,
            Err(_) => return Err(ToolError::new(stopped_message(host.time_limit))),
        }
    }

    let (bytes, slides, logs) = context.with(|ctx| -> Result<_, ToolError> {
        let globals = ctx.globals();
        let logs: Vec<String> = globals.get("__subakoLogs").unwrap_or_default();
        if let Ok(error) = globals.get::<_, rquickjs::Value>("__subakoError")
            && !error.is_undefined()
        {
            let message = if Instant::now() > deadline {
                stopped_message(host.time_limit)
            } else {
                describe(error, &filename)
            };
            return Err(ToolError::new(with_logs(message, &logs)));
        }
        let result: rquickjs::Object = globals
            .get("__subakoResult")
            .map_err(|_| ToolError::new(with_logs("the script never finished".to_string(), &logs)))?;
        let array: TypedArray<u8> = result.get("bytes").map_err(js_error)?;
        // SAFETY: no JavaScript runs while the slice is borrowed.
        let bytes = unsafe { array.as_slice() }.to_vec();
        let slides: usize = result.get("slides").map_err(js_error)?;
        Ok((bytes, slides, logs))
    })?;

    let output = Path::new(&input.output);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ToolError::new(format!("cannot create {}: {e}", parent.display())))?;
    }
    std::fs::write(output, &bytes).map_err(|e| ToolError::new(format!("cannot write {}: {e}", input.output)))?;
    Ok(Created { path: input.output, slides, bytes: bytes.len(), logs })
}

fn stopped_message(limit: Duration) -> String {
    format!("the script was stopped after {} seconds", limit.as_secs())
}

/// The script's logs, after `message`, so a failure keeps them.
fn with_logs(message: String, logs: &[String]) -> String {
    if logs.is_empty() {
        return message;
    }
    format!("{message}\nlogs:\n{}", logs.join("\n"))
}

fn js_error(error: rquickjs::Error) -> ToolError {
    ToolError::new(format!("JavaScript engine: {error}"))
}

/// Runs `code` as a sloppy-mode script, as browser bundles expect.
fn evaluate(ctx: &Ctx<'_>, code: &str, filename: &str) -> Result<(), ToolError> {
    let mut options = EvalOptions::default();
    options.strict = false;
    options.filename = Some(filename.to_string());
    match ctx.eval_with_options::<(), _>(code, options) {
        Ok(()) => Ok(()),
        Err(rquickjs::Error::Exception) => Err(ToolError::new(describe(ctx.catch(), filename))),
        Err(e) => Err(js_error(e)),
    }
}

/// An exception as `Name: message`, with the stack frames in the script.
fn describe(error: rquickjs::Value<'_>, filename: &str) -> String {
    let Some(exception) = error.as_exception() else {
        return format!("the script threw {}", error.as_string().and_then(|s| s.to_string().ok()).unwrap_or_else(|| format!("{error:?}")));
    };
    let name: String = exception.get("name").unwrap_or_else(|_| "Error".to_string());
    let message = exception.message().unwrap_or_default();
    let mut text = format!("{name}: {message}");
    for frame in exception.stack().unwrap_or_default().lines() {
        if frame.contains(filename) {
            text.push('\n');
            text.push_str(frame);
        }
    }
    text
}

/// The `__subako*` values prelude.js and setup.js build on.
fn install_host(ctx: &Ctx<'_>, base: PathBuf, scratch: PathBuf, fetch: Fetch) -> rquickjs::Result<()> {
    let globals = ctx.globals();
    globals.set("__subakoLogoInk", BASE64.encode(LOGO_INK))?;
    globals.set("__subakoLogoPaper", BASE64.encode(LOGO_PAPER))?;

    let text_base = base.clone();
    globals.set(
        "__subakoReadText",
        Function::new(ctx.clone(), move |ctx: Ctx<'_>, path: String| -> rquickjs::Result<String> {
            let real = text_base.join(&path);
            std::fs::read_to_string(&real)
                .map_err(|e| Exception::throw_message(&ctx, &format!("cannot read {}: {e}", real.display())))
        })?,
    )?;

    globals.set(
        "__subakoImage",
        Function::new(ctx.clone(), move |ctx: Ctx<'_>, source: String| -> rquickjs::Result<String> {
            image_data(&source, &base, &scratch, &fetch).map_err(|e| Exception::throw_message(&ctx, &e))
        })?,
    )?;
    Ok(())
}

/// `type;base64,data`, as PptxGenJS takes it, for a Volume file or an https URL.
fn image_data(source: &str, base: &Path, scratch: &Path, fetch: &Fetch) -> Result<String, String> {
    let (bytes, content_type) = if source.starts_with("https://") || source.starts_with("http://") {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dest = scratch.join(format!(".pptx-image-{nanos}"));
        let fetched = fetch(source, &dest);
        let bytes = std::fs::read(&dest);
        let _ = std::fs::remove_file(&dest);
        let (status, content_type) = fetched.map_err(|e| format!("cannot fetch {source}: {e}"))?;
        if !(200..300).contains(&status) {
            return Err(format!("cannot fetch {source}: HTTP {status}"));
        }
        (bytes.map_err(|e| format!("cannot fetch {source}: {e}"))?, content_type)
    } else {
        let real = base.join(source);
        (std::fs::read(&real).map_err(|e| format!("cannot read {}: {e}", real.display()))?, None)
    };
    let kind = image_type(&bytes).or_else(|| content_type.filter(|t| t.starts_with("image/")));
    match kind.as_deref() {
        Some("image/svg+xml") => Err(format!(
            "{source} is SVG, which PptxGenJS can only place with a browser canvas; use PNG or JPEG"
        )),
        Some(kind) => Ok(format!("{kind};base64,{}", BASE64.encode(&bytes))),
        None => Err(format!("{source} is not a PNG, JPEG or GIF image")),
    }
}

/// The image type, from the file's own signature.
fn image_type(bytes: &[u8]) -> Option<String> {
    let kind = if bytes.starts_with(b"\x89PNG") {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.windows(4).take(512).any(|w| w == b"<svg") {
        "image/svg+xml"
    } else {
        return None;
    };
    Some(kind.to_string())
}


#[cfg(test)]
mod tests {
    use std::io::Read;

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
        let scratch = base.join(format!("pptx-function-test-{name}"));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        Host {
            scratch,
            fetch: Arc::new(|url: &str, _: &Path| panic!("unexpected fetch of {url}")),
            time_limit: Duration::from_secs(60),
        }
    }

    fn code(code: &str, host: Host) -> (Result<Created, ToolError>, PathBuf) {
        let output = host.scratch.join("deck.pptx");
        let input = CreateInput {
            source: Source::Code { code: code.to_string() },
            output: output.display().to_string(),
        };
        (create(input, host), output)
    }

    /// The deck's parts, by name.
    fn parts(path: &Path) -> Vec<(String, Vec<u8>)> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        (0..archive.len())
            .map(|i| {
                let mut file = archive.by_index(i).unwrap();
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).unwrap();
                (file.name().to_string(), bytes)
            })
            .collect()
    }

    fn part(parts: &[(String, Vec<u8>)], name: &str) -> String {
        let (_, bytes) = parts.iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("no {name}"));
        String::from_utf8_lossy(bytes).into_owned()
    }

    #[test]
    fn builds_a_deck() {
        let (created, output) = code(
            "const slide = pptx.addSlide();\n\
             slide.addText('Hello, Subako', { x: 1, y: 1, w: 8, h: 1, fontSize: 32 });\n\
             slide.addTable([['a', 'b'], ['1', '2']], { x: 1, y: 2.5, w: 6 });\n\
             pptx.addSlide().addChart(pptx.ChartType.bar,\n  \
               [{ name: 'Calls', labels: ['Q1', 'Q2'], values: [1204, 3918] }], { x: 1, y: 1, w: 8, h: 4 });",
            host("basic"),
        );
        let created = created.unwrap();
        assert_eq!(created.slides, 2);
        assert_eq!(std::fs::metadata(&output).unwrap().len() as usize, created.bytes);
        let parts = parts(&output);
        assert!(part(&parts, "ppt/slides/slide1.xml").contains("Hello, Subako"));
        assert!(parts.iter().any(|(name, _)| name.starts_with("ppt/charts/chart")));
    }

    #[test]
    fn defines_the_subako_masters() {
        let (created, output) = code(
            "const slide = pptx.addSlide({ masterName: 'SUBAKO_CONTENT' });\n\
             slide.addText('Results', { placeholder: 'title' });\n\
             slide.addText(subako.colors.ink, { placeholder: 'body' });",
            host("masters"),
        );
        assert_eq!(created.unwrap().slides, 1);
        let parts = parts(&output);
        let layouts: String = parts
            .iter()
            .filter(|(name, _)| name.starts_with("ppt/slideLayouts/slideLayout") && name.ends_with(".xml"))
            .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
            .collect();
        for master in ["SUBAKO_TITLE", "SUBAKO_SECTION", "SUBAKO_CONTENT"] {
            assert!(layouts.contains(master), "no {master}");
        }
        // The logo, in ink and in paper.
        assert!(parts.iter().filter(|(name, _)| name.starts_with("ppt/media/")).count() >= 2);
        assert!(part(&parts, "ppt/slides/slide1.xml").contains("16221F"));
    }

    #[test]
    fn runs_a_file_with_its_own_paths() {
        let host = host("file");
        let output = host.scratch.join("calls.pptx");
        let input = CreateInput {
            source: Source::File { path: fixtures().join("deck/build.js").display().to_string() },
            output: output.display().to_string(),
        };
        let created = create(input, host).unwrap();
        assert_eq!(created.logs, ["2 quarters"]);
        let parts = parts(&output);
        let png = std::fs::read(fixtures().join("deck/paper.png")).unwrap();
        assert!(parts.iter().any(|(name, bytes)| name.starts_with("ppt/media/") && *bytes == png));
    }

    #[test]
    fn fetches_images_through_the_worker() {
        let mut host = host("fetch");
        host.fetch = Arc::new(|url: &str, dest: &Path| {
            assert_eq!(url, "https://example.com/logo");
            std::fs::copy(fixtures().join("logo.png"), dest).unwrap();
            Ok((200, Some("image/png".to_string())))
        });
        let (created, output) = code(
            "pptx.addSlide().addImage({ data: image('https://example.com/logo'), x: 1, y: 1, w: 3, h: 1 });",
            host,
        );
        created.unwrap();
        let png = std::fs::read(fixtures().join("logo.png")).unwrap();
        assert!(parts(&output).iter().any(|(_, bytes)| *bytes == png));
    }

    #[test]
    fn explains_unusable_images() {
        let host = host("bad-image");
        std::fs::write(host.scratch.join("dot.svg"), "<svg xmlns='http://www.w3.org/2000/svg'/>").unwrap();
        let (created, _) = code("pptx.addSlide().addImage({ data: image('dot.svg') });", host);
        let error = created.err().unwrap();
        assert!(error.message().contains("dot.svg is SVG"), "{}", error.message());
    }

    #[test]
    fn reports_errors_at_the_script_line() {
        let (created, output) = code("const slide = pptx.addSlide();\nconsole.log('before');\nslide.nope();", host("error"));
        let message = created.err().unwrap().message().to_string();
        assert!(message.starts_with("TypeError: not a function"), "{message}");
        assert!(message.contains("<script>:3"), "{message}");
        assert!(message.ends_with("logs:\nbefore"), "{message}");
        assert!(!output.exists());
    }

    #[test]
    fn writes_a_returned_deck_and_waits_for_promises() {
        let (created, _) = code(
            "await new Promise((resolve) => setTimeout(resolve, 10));\n\
             const other = new PptxGenJS();\n\
             other.addSlide(); other.addSlide(); other.addSlide();\n\
             return other;",
            host("returned"),
        );
        assert_eq!(created.unwrap().slides, 3);
    }

    #[test]
    fn stops_scripts_that_run_too_long() {
        let mut host = host("loop");
        host.time_limit = Duration::from_millis(200);
        let (created, _) = code("while (true) {}", host);
        let message = created.err().unwrap().message().to_string();
        assert!(message.contains("stopped after"), "{message}");
    }
}
