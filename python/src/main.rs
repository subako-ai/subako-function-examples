use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use subako_function_sdk::{ToolError, function, package};

// Only the wasm build runs Python, so only it draws; the tests run natively too.
#[cfg(any(target_os = "wasi", test))]
mod svg;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunInput {
    /// The Python to run.
    source: Source,
    /// Command-line arguments, as `sys.argv[1:]`.
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Source {
    /// Python source given as a string, run like `python -c`.
    Code { code: String },
    /// A script on the Volume, under `/volume/tmp` or `/volume/data`, run like
    /// `python <path>`: its directory is first on `sys.path`, so it can
    /// import modules next to it.
    File { path: String },
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct Ran {
    /// What the script printed to `sys.stdout`.
    stdout: String,
    /// What the script printed to `sys.stderr`.
    stderr: String,
    outcome: Outcome,
}

/// How the script ended.
#[derive(Debug, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Outcome {
    /// It ran to the end, or called `sys.exit()` with 0.
    Completed,
    /// It called `sys.exit()` with a nonzero code.
    Exited { code: i64 },
    /// An exception escaped it.
    Raised {
        /// The exception's type and message.
        exception: String,
        /// The traceback, from the script's own frames.
        traceback: String,
    },
}

/// Run Python 3.15 (CPython, compiled to WebAssembly) and return what it
/// printed. Takes source code as a string or a script file on the Volume,
/// which is at `/volume/tmp` and `/volume/data`. The standard library is available except
/// what needs native libraries or the host: compression (zlib, gzip, bz2,
/// lzma), sqlite3, ssl, ctypes, subprocess, threads, sockets and the asyncio
/// event loop. HTTPS works through `urllib.request`, or `subako.request(url,
/// method=, headers=, body=, json=, save_to=)` which returns `.status`,
/// `.headers`, `.text` and `.json()`; only hosts the function's network
/// setting allows are reachable. For charts, pygal is installed (Bar, Line,
/// Pie, XY, ...): `chart.render_to_file(path)` writes SVG and
/// `chart.render_to_png(path)` writes PNG. `subako.svg_to_png(svg, path,
/// scale=1.0)` draws any SVG as a PNG; text can be Latin or Japanese.
#[function]
fn run_python(input: RunInput) -> Result<Ran, ToolError> {
    let request = serde_json::to_string(&input)
        .map_err(|e| ToolError::new(format!("cannot encode the request: {e}")))?;
    let result = python::run(&request).map_err(ToolError::new)?;
    serde_json::from_str(&result)
        .map_err(|e| ToolError::new(format!("unreadable result from Python: {e}")))
}

package! {
    name: "python",
    description: "Runs Python code or scripts with CPython compiled to WebAssembly.",
    functions: [run_python],
}

/// The embedded interpreter, through the handful of C API calls this needs.
#[cfg(target_os = "wasi")]
mod python {
    use std::ffi::{CString, c_char, c_int, c_void};

    type PyObject = c_void;

    unsafe extern "C" {
        fn Py_InitializeEx(initsigs: c_int);
        fn Py_IsInitialized() -> c_int;
        fn PyImport_AddModule(name: *const c_char) -> *mut PyObject;
        fn PyModule_GetDict(module: *mut PyObject) -> *mut PyObject;
        fn Py_CompileString(
            source: *const c_char,
            filename: *const c_char,
            start: c_int,
        ) -> *mut PyObject;
        fn PyEval_EvalCode(
            code: *mut PyObject,
            globals: *mut PyObject,
            locals: *mut PyObject,
        ) -> *mut PyObject;
        fn PyUnicode_FromStringAndSize(text: *const c_char, size: isize) -> *mut PyObject;
        fn PyUnicode_AsUTF8AndSize(text: *mut PyObject, size: *mut isize) -> *const c_char;
        fn PyObject_SetAttrString(object: *mut PyObject, name: *const c_char, value: *mut PyObject)
        -> c_int;
        fn PyObject_GetAttrString(object: *mut PyObject, name: *const c_char) -> *mut PyObject;
        fn PyObject_Str(object: *mut PyObject) -> *mut PyObject;
        fn PyErr_GetRaisedException() -> *mut PyObject;
        fn Py_DecRef(object: *mut PyObject);
        fn PyCMethod_New(
            def: *mut MethodDef,
            slf: *mut PyObject,
            module: *mut PyObject,
            class: *mut PyObject,
        ) -> *mut PyObject;
    }

    /// `PyMethodDef`.
    #[repr(C)]
    struct MethodDef {
        name: *const c_char,
        method: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
        flags: c_int,
        doc: *const c_char,
    }

    /// `Py_file_input`: the source is a module, run as statements.
    const FILE_INPUT: c_int = 257;
    /// `METH_O`: the function takes one argument.
    const METH_O: c_int = 0x0008;

    const HARNESS: &str = include_str!("harness.py");
    const SUBAKO: &str = include_str!("subako.py");

    /// Runs harness.py on `request` and returns its result, both as JSON.
    pub fn run(request: &str) -> Result<String, String> {
        let harness = CString::new(HARNESS).map_err(|e| e.to_string())?;
        // SAFETY: a guest runs one call on one thread, so nothing else touches
        // the environment or the interpreter, and every pointer passed below
        // is checked for null first.
        unsafe {
            if Py_IsInitialized() == 0 {
                // The standard library is in the package's assets, which
                // Subako mounts read-only, so bytecode is never cached.
                std::env::set_var("PYTHONHOME", "/package");
                // The third-party packages fetch-cpython.sh bundles.
                std::env::set_var("PYTHONPATH", "/package/site-packages");
                std::env::set_var("PYTHONDONTWRITEBYTECODE", "1");
                // The guest has no /tmp of its own, so `tempfile` uses the Volume's.
                std::env::set_var("TMPDIR", "/volume/tmp");
                Py_InitializeEx(0);
            }
            let module = PyImport_AddModule(c"_subako".as_ptr());
            if module.is_null() {
                return Err(raised("cannot create the _subako module"));
            }
            set_str(module, c"request", request)?;
            set_str(module, c"subako_source", SUBAKO)?;
            set(module, c"http_request", http_function())?;
            set(module, c"svg_to_png", svg_function())?;
            // Under its own name, so tracebacks can leave its frames out:
            // code given as a string is `<string>`, as with `python -c`.
            let code = Py_CompileString(harness.as_ptr(), c"<subako harness>".as_ptr(), FILE_INPUT);
            if code.is_null() {
                return Err(raised("the harness does not compile"));
            }
            let globals = PyModule_GetDict(module);
            let ran = PyEval_EvalCode(code, globals, globals);
            Py_DecRef(code);
            if ran.is_null() {
                return Err(raised("the harness failed"));
            }
            Py_DecRef(ran);
            let result = PyObject_GetAttrString(module, c"result".as_ptr());
            if result.is_null() {
                return Err(raised("the harness left no result"));
            }
            let text = utf8(result);
            Py_DecRef(result);
            text.ok_or_else(|| raised("the result is not text"))
        }
    }

    /// Sets `module.name` to `value`, which it takes, and reports an error
    /// for a null one.
    unsafe fn set(
        module: *mut PyObject,
        name: &std::ffi::CStr,
        value: *mut PyObject,
    ) -> Result<(), String> {
        // SAFETY: called with the interpreter initialized, on its one thread.
        unsafe {
            if value.is_null() {
                return Err(raised(&format!("cannot create _subako.{}", name.to_string_lossy())));
            }
            let set = PyObject_SetAttrString(module, name.as_ptr(), value);
            Py_DecRef(value);
            if set != 0 {
                return Err(raised(&format!("cannot set _subako.{}", name.to_string_lossy())));
            }
            Ok(())
        }
    }

    unsafe fn set_str(module: *mut PyObject, name: &std::ffi::CStr, value: &str) -> Result<(), String> {
        // SAFETY: called with the interpreter initialized, on its one thread.
        unsafe { set(module, name, PyUnicode_FromStringAndSize(value.as_ptr().cast(), value.len() as isize)) }
    }

    /// A Python function for `method`, which takes one argument.
    unsafe fn c_function(
        name: &'static std::ffi::CStr,
        method: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
        doc: &'static std::ffi::CStr,
    ) -> *mut PyObject {
        // The interpreter keeps a pointer to the definition for good.
        let def = Box::leak(Box::new(MethodDef { name: name.as_ptr(), method, flags: METH_O, doc: doc.as_ptr() }));
        // SAFETY: called with the interpreter initialized, on its one thread.
        unsafe { PyCMethod_New(def, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) }
    }

    /// `_subako.http_request`, which takes an SDK `http::Request` as JSON and
    /// returns an `http::Answer` as JSON, for subako.py to wrap.
    unsafe fn http_function() -> *mut PyObject {
        // SAFETY: as for `c_function`.
        unsafe {
            c_function(
                c"http_request",
                http_request,
                c"Make an HTTP request through the Subako worker, as JSON in and out.",
            )
        }
    }

    /// `_subako.svg_to_png`, which takes `{"svg", "path", "scale"}` as JSON and
    /// returns `{"ok": [width, height]}` or `{"error": message}` as JSON.
    unsafe fn svg_function() -> *mut PyObject {
        // SAFETY: as for `c_function`.
        unsafe { c_function(c"svg_to_png", svg_to_png, c"Draw an SVG as a PNG file, as JSON in and out.") }
    }

    unsafe extern "C" fn svg_to_png(_: *mut PyObject, request: *mut PyObject) -> *mut PyObject {
        #[derive(serde::Deserialize)]
        struct Request {
            svg: String,
            path: String,
            scale: f32,
        }
        // SAFETY: Python calls this with a live argument, on its one thread.
        let Some(request) = (unsafe { utf8(request) }) else {
            return std::ptr::null_mut();
        };
        let drawn = serde_json::from_str::<Request>(&request)
            .map_err(|e| format!("invalid request: {e}"))
            .and_then(|r| {
                super::svg::to_png(&r.svg, r.path.as_ref(), r.scale, "/package/fonts".as_ref())
            });
        let answer = match drawn {
            Ok(size) => serde_json::json!({ "ok": size }),
            Err(message) => serde_json::json!({ "error": message }),
        };
        let answer = answer.to_string();
        // SAFETY: as above.
        unsafe { PyUnicode_FromStringAndSize(answer.as_ptr().cast(), answer.len() as isize) }
    }

    unsafe extern "C" fn http_request(_: *mut PyObject, request: *mut PyObject) -> *mut PyObject {
        use subako_function_sdk::http::{self, Answer, HttpError, HttpErrorKind};
        // SAFETY: Python calls this with a live argument, on its one thread.
        let Some(request) = (unsafe { utf8(request) }) else {
            // `utf8` failed on a non-str, and Python has the TypeError.
            return std::ptr::null_mut();
        };
        let answer = match serde_json::from_str::<http::Request>(&request) {
            Ok(request) => match http::request(&request) {
                Ok(response) => Answer::Ok(response),
                Err(error) => Answer::Error(error),
            },
            Err(e) => Answer::Error(HttpError {
                kind: HttpErrorKind::InvalidRequest,
                message: format!("invalid request: {e}"),
            }),
        };
        // An `Answer` is plain data, so it always serializes.
        let answer = serde_json::to_string(&answer).unwrap_or_default();
        // SAFETY: as above.
        unsafe { PyUnicode_FromStringAndSize(answer.as_ptr().cast(), answer.len() as isize) }
    }

    /// `context`, with the pending Python exception when there is one.
    unsafe fn raised(context: &str) -> String {
        // SAFETY: called with the interpreter initialized, on its one thread.
        unsafe {
            let exception = PyErr_GetRaisedException();
            if exception.is_null() {
                return context.to_string();
            }
            let message = PyObject_Str(exception);
            Py_DecRef(exception);
            if message.is_null() {
                return context.to_string();
            }
            let detail = utf8(message);
            Py_DecRef(message);
            match detail {
                Some(detail) => format!("{context}: {detail}"),
                None => context.to_string(),
            }
        }
    }

    /// A copy of a Python `str`.
    unsafe fn utf8(text: *mut PyObject) -> Option<String> {
        let mut size = 0;
        // SAFETY: `text` is a live object; the buffer it returns lives as long
        // as it does, and is copied before the caller releases it.
        unsafe {
            let data = PyUnicode_AsUTF8AndSize(text, &mut size);
            if data.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(data.cast::<u8>(), size as usize);
            Some(String::from_utf8_lossy(bytes).into_owned())
        }
    }
}

/// Built for this machine, the package only prints its manifest.
#[cfg(not(target_os = "wasi"))]
mod python {
    pub fn run(_request: &str) -> Result<String, String> {
        Err("Python runs only in the wasm32-wasip1 build".to_string())
    }
}

/// Run with `cargo test --target wasm32-wasip1`, which runs them in wasmtime
/// (see .cargo/config.toml). They share one interpreter, as no call does.
#[cfg(all(test, target_os = "wasi"))]
mod tests {
    use super::*;

    fn code(code: &str) -> Ran {
        run_python(RunInput { source: Source::Code { code: code.to_string() }, args: Vec::new() })
            .unwrap()
    }

    #[test]
    fn prints_to_stdout_and_stderr() {
        let ran = code("import sys\nprint('hello')\nprint('oops', file=sys.stderr)");
        assert_eq!(ran.stdout, "hello\n");
        assert_eq!(ran.stderr, "oops\n");
        assert_eq!(ran.outcome, Outcome::Completed);
    }

    #[test]
    fn uses_the_standard_library() {
        let ran = code(
            "import json, re, datetime, decimal, hashlib, collections\n\
             print(json.dumps({'sum': str(decimal.Decimal('0.1') + decimal.Decimal('0.2'))}))\n\
             print(hashlib.sha256(b'abc').hexdigest()[:8])",
        );
        assert_eq!(ran.stdout, "{\"sum\": \"0.3\"}\nba7816bf\n", "{ran:?}");
    }

    #[test]
    fn reports_an_exception_with_its_traceback() {
        let ran = code("def f():\n    return 1 / 0\n\nprint('before')\nf()");
        assert_eq!(ran.stdout, "before\n");
        let Outcome::Raised { exception, traceback } = ran.outcome else {
            panic!("expected an exception: {:?}", ran.outcome);
        };
        assert_eq!(exception, "ZeroDivisionError: division by zero");
        assert!(traceback.contains("return 1 / 0"), "{traceback}");
        assert!(!traceback.contains("harness"), "{traceback}");
        assert!(!traceback.contains("in run"), "{traceback}");
    }

    #[test]
    fn reports_a_syntax_error() {
        let ran = code("print(");
        let Outcome::Raised { exception, .. } = ran.outcome else {
            panic!("expected an exception: {:?}", ran.outcome);
        };
        assert!(exception.starts_with("SyntaxError"), "{exception}");
    }

    #[test]
    fn reports_the_exit_code() {
        assert_eq!(code("import sys; sys.exit(3)").outcome, Outcome::Exited { code: 3 });
        assert_eq!(code("import sys; sys.exit(0)").outcome, Outcome::Completed);
        let ran = code("import sys; sys.exit('bad input')");
        assert_eq!(ran.outcome, Outcome::Exited { code: 1 });
        assert_eq!(ran.stderr, "bad input\n");
    }

    #[test]
    fn runs_a_file_with_arguments_and_its_imports() {
        let ran = run_python(RunInput {
            source: Source::File { path: "/tests/scripts/greet.py".to_string() },
            args: vec!["Ada".to_string()],
        })
        .unwrap();
        assert_eq!(ran.stdout, "Hello, Ada!\n", "{ran:?}");
        assert_eq!(ran.outcome, Outcome::Completed);
    }

    #[test]
    fn calls_do_not_share_globals() {
        code("leftover = 1");
        let ran = code("print('leftover' in globals())");
        assert_eq!(ran.stdout, "False\n");
    }

    fn stdout(source: &str) -> String {
        let ran = code(source);
        assert_eq!(ran.outcome, Outcome::Completed, "{ran:?}");
        ran.stdout
    }

    /// Outside the worker, the stub in tests/ fails every request.
    #[test]
    fn http_reaches_the_worker_import() {
        let out = stdout(
            "import subako\n\
             try:\n    subako.request('https://example.com')\n\
             except subako.HTTPError as e:\n    print(e.kind)",
        );
        assert_eq!(out, "abi\n");
    }

    /// A fake worker for the urllib tests: it records each request and
    /// answers from `RESPONSES`, writing the body where the request says.
    const FAKE_WORKER: &str = r#"
import json, _subako
SENT = []
RESPONSES = []
def fake(request):
    request = json.loads(request)
    SENT.append(request)
    answer = RESPONSES.pop(0)
    if "error" in answer:
        return json.dumps(answer)
    status, headers, body = answer["ok"]
    with open(request["output"]["path"], "wb") as f:
        f.write(body)
    file = {"type": "file", "path": request["output"]["path"], "bytes": len(body)}
    return json.dumps({"ok": {"status": status, "headers": headers, "body": file}})
_subako.http_request = fake
"#;

    #[test]
    fn urllib_goes_through_the_worker() {
        let out = stdout(&format!(
            "{FAKE_WORKER}\n\
             import urllib.request\n\
             RESPONSES.append({{'ok': (200, {{'content-type': 'image/png'}}, b'\\x89PNG\\x00')}})\n\
             with urllib.request.urlopen('https://example.com/a.png') as r:\n    \
                 print(r.status, r.headers['Content-Type'], r.read())\n\
             print(SENT[0]['method'], SENT[0]['url'], SENT[0]['headers']['User-agent'][:6])\n\
             import os\n\
             print([f for f in os.listdir('/volume/tmp') if f.startswith('.subako-http')])"
        ));
        assert_eq!(
            out,
            "200 image/png b'\\x89PNG\\x00'\nGET https://example.com/a.png Python\n[]\n"
        );
    }

    #[test]
    fn urllib_posts_text_and_binary_bodies() {
        let out = stdout(&format!(
            "{FAKE_WORKER}\n\
             import urllib.request\n\
             RESPONSES.extend([{{'ok': (201, {{}}, b'')}}, {{'ok': (201, {{}}, b'')}}])\n\
             urllib.request.urlopen('https://example.com', data=b'a=1')\n\
             print(SENT[0]['method'], SENT[0]['body'], SENT[0]['headers']['Content-type'])\n\
             urllib.request.urlopen(urllib.request.Request('https://example.com', data=b'\\xff', method='PUT'))\n\
             print(SENT[1]['method'], SENT[1]['body']['type'])"
        ));
        assert_eq!(
            out,
            "POST {'type': 'text', 'text': 'a=1'} application/x-www-form-urlencoded\nPUT file\n"
        );
    }

    #[test]
    fn urllib_raises_for_error_statuses_and_worker_errors() {
        let out = stdout(&format!(
            "{FAKE_WORKER}\n\
             import urllib.request, urllib.error\n\
             RESPONSES.append({{'ok': (404, {{}}, b'gone')}})\n\
             try:\n    urllib.request.urlopen('https://example.com')\n\
             except urllib.error.HTTPError as e:\n    print(e.code, e.read())\n\
             RESPONSES.append({{'error': {{'kind': 'host_not_allowed', 'message': 'example.com is not allowed'}}}})\n\
             try:\n    urllib.request.urlopen('https://example.com')\n\
             except urllib.error.URLError as e:\n    print(e.reason)"
        ));
        assert_eq!(out, "404 b'gone'\nhost_not_allowed: example.com is not allowed\n");
    }

    #[test]
    fn draws_a_pygal_chart_as_svg_and_png() {
        let out = stdout(
            "import os, pygal\n\
             os.makedirs('/volume/tmp/chart-test', exist_ok=True)\n\
             chart = pygal.Bar(title='売上 by quarter')\n\
             chart.x_labels = ['Q1', 'Q2', 'Q3', 'Q4']\n\
             chart.add('2025', [3, 5, 4, 8])\n\
             chart.render_to_file('/volume/tmp/chart-test/sales.svg')\n\
             chart.render_to_png('/volume/tmp/chart-test/sales.png')\n\
             import subako\n\
             print(subako.svg_to_png(chart.render(), '/volume/tmp/chart-test/big.png', scale=2))\n\
             print(open('/volume/tmp/chart-test/sales.png', 'rb').read(4))\n\
             import cairosvg\n\
             print(cairosvg.svg2png(bytestring=chart.render(), output_width=400)[:4])",
        );
        assert_eq!(out, "(1600, 1200)\nb'\\x89PNG'\nb'\\x89PNG'\n");
    }

    #[test]
    fn bad_svg_is_a_value_error() {
        let out = stdout(
            "import subako\n\
             try:\n    subako.svg_to_png('<svg', '/volume/tmp/bad.png')\n\
             except ValueError as e:\n    print(str(e)[:11])",
        );
        assert_eq!(out, "invalid SVG\n");
    }

    #[test]
    fn a_missing_file_is_an_exception() {
        let ran = run_python(RunInput {
            source: Source::File { path: "/tests/scripts/missing.py".to_string() },
            args: Vec::new(),
        })
        .unwrap();
        assert!(matches!(ran.outcome, Outcome::Raised { .. }), "{:?}", ran.outcome);
    }
}
