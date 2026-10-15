use std::path::PathBuf;
use std::sync::Arc;

use bashkit::{Bash, Builtin, BuiltinContext, ExecResult, ExecutionLimits, PosixFs, async_trait};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use subako_function_sdk::{ToolError, function, http, package};

mod curl;
mod volume;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BashInput {
    /// The bash script to run.
    script: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct Ran {
    exit_code: i32,
    stdout: String,
    stderr: String,
    /// Output past 1 MiB is cut off; these say when it was.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stdout_truncated: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stderr_truncated: bool,
}

/// Run a bash script in Bashkit, a bash interpreter written in Rust: pipes,
/// redirects, variables, functions and arrays, with grep, sed, awk, jq, find,
/// sort, cut, tar and about 140 more built in. There are no other
/// programs to run. `/volume/tmp` and `/volume/data` are the Volume's real
/// files, and the script starts in `/volume/tmp`; anything written elsewhere
/// is gone after the call, as are variables and functions. `curl` makes HTTPS
/// requests to hosts the function's network setting allows, with a limited set
/// of options.
#[function]
fn run_bash(input: BashInput) -> Result<Ran, ToolError> {
    let setup = Setup {
        mounts: ["/volume/tmp", "/volume/data"]
            .into_iter()
            .filter(|dir| std::fs::metadata(dir).is_ok())
            .map(|dir| (dir.to_string(), PathBuf::from(dir)))
            .collect(),
        scratch: PathBuf::from("/volume/tmp"),
        transport: Arc::new(http::request),
    };
    run(&input.script, setup)
}

package! {
    name: "bash",
    description: "Runs bash scripts with Bashkit, on the session's Volume.",
    functions: [run_bash],
}

/// What a run uses from outside the interpreter, so tests can supply their
/// own directories and a fake worker.
struct Setup {
    /// Shell paths, and the real directories they show.
    mounts: Vec<(String, PathBuf)>,
    /// A real directory curl's bodies pass through.
    scratch: PathBuf,
    transport: curl::Transport,
}

/// Bashkit's commands that cannot work here: its own HTTP clients, which need
/// sockets, and the interactive ones.
const LEFT_OUT: &[&str] = &["wget", "http", "less", "watch", "clear", "history", "fc"];

/// Stands in for a left-out command. Bashkit 0.18 has no way to drop its own
/// builtins, but one registered under the same name replaces it.
struct LeftOut(&'static str);

#[async_trait]
impl Builtin for LeftOut {
    async fn execute(&self, _ctx: BuiltinContext<'_>) -> bashkit::Result<ExecResult> {
        let hint = if matches!(self.0, "wget" | "http") { "; use curl" } else { "" };
        Ok(ExecResult::err(format!("bash: {}: command not found{hint}\n", self.0), 127))
    }
}

fn run(script: &str, setup: Setup) -> Result<Ran, ToolError> {
    let curl = curl::Curl { transport: setup.transport, scratch: setup.scratch };
    // Bashkit's defaults stop a script at 10,000 commands or loop
    // iterations, which ordinary loops reach. Subako bounds a call's time
    // anyway, so these allow 100 times more; the 30 s timeout and the 1 MiB
    // output caps stay.
    let limits = ExecutionLimits::new()
        .max_commands(1_000_000)
        .max_loop_iterations(1_000_000)
        .max_total_loop_iterations(10_000_000);
    let mut builder = Bash::builder()
        .limits(limits)
        .cwd(setup.mounts.first().map_or("/", |(path, _)| path.as_str()))
        .builtin("curl", Box::new(curl));
    for name in LEFT_OUT {
        builder = builder.builtin(*name, Box::new(LeftOut(name)));
    }
    let mut bash = builder.build();
    for (path, real) in setup.mounts {
        bash.mount(&path, Arc::new(PosixFs::new(volume::Volume::new(real))))
            .map_err(|e| ToolError::new(format!("cannot mount {path}: {e}")))?;
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build()?;
    let result = runtime
        .block_on(bash.exec(script))
        .map_err(|e| ToolError::new(e.to_string()))?;
    Ok(Ran {
        exit_code: result.exit_code,
        stdout: String::from(&result.stdout),
        stderr: String::from(&result.stderr),
        stdout_truncated: result.stdout_truncated,
        stderr_truncated: result.stderr_truncated,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;

    use subako_function_sdk::http::{HttpError, HttpErrorKind, Request, RequestBody, Response, ResponseBody};

    use super::*;

    /// A fresh real directory, shown to the script as `/volume/tmp`.
    fn volume(name: &str) -> PathBuf {
        // WASI has no `temp_dir`; the wasmtime runner maps target/ to /volume/tmp.
        let base = if cfg!(target_os = "wasi") { PathBuf::from("/volume/tmp") } else { std::env::temp_dir() };
        let dir = base.join(format!("bashkit-function-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake worker: records each request and answers with `answer`,
    /// writing the body where the request says.
    fn worker(
        answer: impl Fn(&Request) -> Result<(u16, Vec<(&'static str, &'static str)>, Vec<u8>), HttpError>
        + Send
        + Sync
        + 'static,
    ) -> (curl::Transport, Arc<Mutex<Vec<Request>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        let transport: curl::Transport = Arc::new(move |request: &Request| {
            log.lock().unwrap().push(request.clone());
            let (status, headers, body) = answer(request)?;
            let http::ResponseOutput::File { path } = &request.output else {
                panic!("curl asks for the body in a file");
            };
            std::fs::write(path, &body).unwrap();
            Ok(Response {
                status,
                headers: headers.into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                body: ResponseBody::File { path: path.clone(), bytes: body.len() as u64 },
            })
        });
        (transport, sent)
    }

    fn bash(script: &str, dir: &Path, transport: curl::Transport) -> Ran {
        let setup = Setup {
            mounts: vec![("/volume/tmp".to_string(), dir.to_path_buf())],
            scratch: dir.to_path_buf(),
            transport,
        };
        run(script, setup).unwrap()
    }

    fn offline(script: &str, dir: &Path) -> Ran {
        let (transport, _) = worker(|_| panic!("no request expected"));
        bash(script, dir, transport)
    }

    #[test]
    fn runs_pipelines_and_builtins() {
        let dir = volume("pipes");
        let ran = offline(
            "echo hi | tr a-z A-Z\n\
             printf 'b\\na\\nb\\n' | sort | uniq -c\n\
             echo '{\"a\":[1,2]}' | jq '.a | add'\n\
             awk 'BEGIN { print 6 * 7 }'\n\
             f() { echo \"f $1\"; }; f x\n\
             arr=(a b c); echo ${#arr[@]} ${arr[2]}",
            &dir,
        );
        assert_eq!(ran.stdout, "HI\n      1 a\n      2 b\n3\n42\nf x\n3 c\n", "{ran:?}");
        assert_eq!(ran.exit_code, 0);
    }

    #[test]
    fn allows_long_loops() {
        let dir = volume("loops");
        let ran = offline("n=0; for i in $(seq 1 50000); do n=$((n + 1)); done; echo $n", &dir);
        assert_eq!(ran.stdout, "50000\n", "{ran:?}");
    }

    #[test]
    fn starts_in_tmp() {
        let dir = volume("cwd");
        assert_eq!(offline("pwd", &dir).stdout, "/volume/tmp\n");
    }

    #[test]
    fn reads_and_writes_the_real_files() {
        let dir = volume("files");
        std::fs::write(dir.join("in.txt"), "from the volume\n").unwrap();
        let ran = offline(
            "cat /volume/tmp/in.txt\n\
             echo one > out.txt; echo two >> out.txt\n\
             mkdir -p a/b && cp out.txt a/b/copy.txt\n\
             mv in.txt moved.txt\n\
             find /volume/tmp -name '*.txt' | sort",
            &dir,
        );
        assert_eq!(
            ran.stdout,
            "from the volume\n/tmp/a/b/copy.txt\n/tmp/moved.txt\n/tmp/out.txt\n",
            "{ran:?}"
        );
        assert_eq!(std::fs::read_to_string(dir.join("out.txt")).unwrap(), "one\ntwo\n");
        assert_eq!(std::fs::read_to_string(dir.join("a/b/copy.txt")).unwrap(), "one\ntwo\n");
        assert!(!dir.join("in.txt").exists());

        offline("rm -r a", &dir);
        assert!(!dir.join("a").exists());
    }

    #[test]
    fn leaves_out_commands_that_cannot_work() {
        let dir = volume("left-out");
        let ran = offline("wget https://example.com", &dir);
        assert_eq!(ran.exit_code, 127, "{ran:?}");
        assert_eq!(ran.stderr, "bash: wget: command not found; use curl\n");
    }

    #[test]
    fn curl_gets_a_page() {
        let dir = volume("curl-get");
        let (transport, sent) = worker(|_| Ok((200, vec![("content-type", "text/plain")], b"hello".to_vec())));
        let ran = bash("curl -s https://example.com/a", &dir, transport);
        assert_eq!(ran.stdout, "hello");
        let sent = sent.lock().unwrap();
        assert_eq!(sent[0].url, "https://example.com/a");
        assert_eq!(sent[0].method, http::Method::Get);
        assert!(!sent[0].follow_redirects);
        // The body passed through the scratch directory and is gone.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn curl_pipes_into_other_commands() {
        let dir = volume("curl-pipe");
        let (transport, _) = worker(|_| Ok((200, vec![], br#"{"items":[{"id":1},{"id":2}]}"#.to_vec())));
        let ran = bash("curl -fsSL https://api.example.com | jq '.items | length'", &dir, transport);
        assert_eq!(ran.stdout, "2\n", "{ran:?}");
    }

    #[test]
    fn curl_posts_data_and_json() {
        let dir = volume("curl-post");
        std::fs::write(dir.join("body.bin"), [0xff, 0x00]).unwrap();
        let (transport, sent) = worker(|_| Ok((201, vec![], Vec::new())));
        bash(
            "curl -s -d a=1 -d b=2 https://example.com\n\
             curl -s --json '{\"x\":1}' -H 'Accept: text/plain' https://example.com\n\
             curl -s -X PUT --data-binary @body.bin https://example.com",
            &dir,
            transport,
        );
        let sent = sent.lock().unwrap();
        assert_eq!(sent[0].method, http::Method::Post);
        assert_eq!(sent[0].body, Some(RequestBody::Text { text: "a=1&b=2".to_string() }));
        assert_eq!(sent[0].headers["Content-Type"], "application/x-www-form-urlencoded");
        assert_eq!(sent[1].body, Some(RequestBody::Text { text: "{\"x\":1}".to_string() }));
        assert_eq!(sent[1].headers["Content-Type"], "application/json");
        assert_eq!(sent[1].headers["Accept"], "text/plain");
        assert_eq!(sent[2].method, http::Method::Put);
        assert!(matches!(sent[2].body, Some(RequestBody::File { .. })));
    }

    #[test]
    fn curl_saves_includes_and_writes_out() {
        let dir = volume("curl-out");
        let (transport, sent) = worker(|_| Ok((200, vec![("x-a", "1")], vec![0x89, b'P'])));
        let ran = bash(
            "curl -s -o img.png -w '%{http_code} %{size_download}\\n' -L -m 2.5 https://example.com/img\n\
             curl -s -I https://example.com/img",
            &dir,
            transport,
        );
        assert_eq!(ran.stdout, "200 2\nHTTP/1.1 200\r\nx-a: 1\r\n\r\n", "{ran:?}");
        assert_eq!(std::fs::read(dir.join("img.png")).unwrap(), [0x89, b'P']);
        let sent = sent.lock().unwrap();
        assert!(sent[0].follow_redirects);
        assert_eq!(sent[0].timeout_ms, Some(2500));
        assert_eq!(sent[1].method, http::Method::Head);
    }

    #[test]
    fn curl_fails_like_curl() {
        let dir = volume("curl-fail");
        let (transport, _) = worker(|request| match request.url.as_str() {
            "https://example.com/missing" => Ok((404, vec![], b"gone".to_vec())),
            _ => Err(HttpError {
                kind: HttpErrorKind::HostNotAllowed,
                message: "blocked.example is not allowed".to_string(),
            }),
        });
        let ran = bash(
            "curl https://example.com/missing; echo \" $?\"\n\
             curl -f https://example.com/missing; echo \" $?\"\n\
             curl -s https://blocked.example; echo \" $?\"\n\
             curl https://blocked.example; echo \" $?\"",
            &dir,
            transport,
        );
        assert_eq!(ran.stdout, "gone 0\n 22\n 7\n 7\n", "{ran:?}");
        assert_eq!(
            ran.stderr,
            "curl: (22) The requested URL returned error: 404\n\
             curl: (7) blocked.example is not allowed\n"
        );
    }

    #[test]
    fn curl_rejects_options_it_lacks() {
        let dir = volume("curl-options");
        let ran = offline("curl --proxy x https://example.com; echo $?", &dir);
        assert_eq!(ran.stdout, "2\n");
        assert!(ran.stderr.contains("--proxy is not supported"), "{ran:?}");
    }
}
