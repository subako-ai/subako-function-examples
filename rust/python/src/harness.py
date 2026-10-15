# Runs one request inside the embedded interpreter. main.rs sets
# `_subako.request` to the request as JSON, `_subako.subako_source` to
# subako.py and `_subako.http_request` to the worker's HTTP, runs this file in
# the `_subako` module, and reads the result as JSON from `_subako.result`.
import io
import json
import linecache
import os
import runpy
import sys
import traceback
import types


def run(request):
    source = request["source"]
    filename = source["path"] if source["type"] == "file" else "<string>"
    stdout, stderr = io.StringIO(), io.StringIO()
    real_stdout, real_stderr = sys.stdout, sys.stderr
    sys.stdout, sys.stderr = stdout, stderr
    outcome = {"type": "completed"}
    try:
        if source["type"] == "code":
            # Lets tracebacks quote the lines of code that has no file.
            lines = source["code"].splitlines(keepends=True)
            linecache.cache[filename] = (len(source["code"]), None, lines, filename)
            sys.argv = ["-c", *request["args"]]
            main = types.ModuleType("__main__")
            sys.modules["__main__"] = main
            exec(compile(source["code"], filename, "exec"), main.__dict__)
        else:
            sys.argv = [filename, *request["args"]]
            sys.path.insert(0, os.path.dirname(os.path.abspath(filename)))
            runpy.run_path(filename, run_name="__main__")
    except SystemExit as exit:
        code = exit.code
        if code is None:
            code = 0
        elif not isinstance(code, int):
            # As the interpreter does: a non-integer is printed and exits 1.
            print(code, file=sys.stderr)
            code = 1
        if code != 0:
            outcome = {"type": "exited", "code": code}
    except BaseException as error:
        outcome = {
            "type": "raised",
            "exception": summary(error),
            "traceback": "".join(
                traceback.format_exception(error.__class__, error, user_frames(error, filename))
            ),
        }
    finally:
        sys.stdout, sys.stderr = real_stdout, real_stderr
    return {"stdout": stdout.getvalue(), "stderr": stderr.getvalue(), "outcome": outcome}


def summary(error):
    """`ValueError: message`, or the bare type when it has no message."""
    message = str(error)
    name = error.__class__.__name__
    return f"{name}: {message}" if message else name


def user_frames(error, filename):
    """The traceback from the first frame in the user's code, so this
    harness and runpy stay out of it."""
    tb = error.__traceback__
    while tb is not None and tb.tb_frame.f_code.co_filename != filename:
        tb = tb.tb_next
    return tb


def install_subako():
    """The `subako` module, for HTTP through the worker; once per process."""
    if "subako" in sys.modules:
        return
    filename = "<subako>"
    lines = subako_source.splitlines(keepends=True)  # noqa: F821 (set from main.rs)
    linecache.cache[filename] = (len(subako_source), None, lines, filename)  # noqa: F821
    module = types.ModuleType("subako")
    sys.modules["subako"] = module
    exec(compile(subako_source, filename, "exec"), module.__dict__)  # noqa: F821


install_subako()
result = json.dumps(run(json.loads(request)))  # noqa: F821 (`request` is set from main.rs)
