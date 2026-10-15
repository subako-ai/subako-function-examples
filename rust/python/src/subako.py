"""HTTP through the Subako worker.

WASI has no sockets, so the worker makes requests on the function's behalf:
HTTPS only, to hosts the function's network setting allows, with any stored
credentials attached for their own origins. `urllib.request.urlopen` goes
through it as well, so code written for urllib works unchanged.
"""

import json as _json
import os
import sys

import _subako

__all__ = ["HTTPError", "Response", "request", "svg_to_png"]


class HTTPError(Exception):
    """The worker could not complete a request. `kind` says why, such as
    `host_not_allowed`, `forbidden_destination`, `timeout`, `too_large` or
    `not_text`. A response with an error status is not one of these."""

    def __init__(self, kind, message):
        super().__init__(message)
        self.kind = kind


class Response:
    """The final response, after redirects. `text` holds the body unless it
    was saved with `save_to`, in which case `path` and `bytes` describe the
    file."""

    def __init__(self, status, headers, text=None, path=None, size=None):
        self.status = status
        #: Lowercase names; repeated headers joined with a comma.
        self.headers = headers
        self.text = text
        self.path = path
        self.bytes = size

    def json(self):
        return _json.loads(self.text)

    def __repr__(self):
        where = f"{self.bytes} bytes at {self.path}" if self.path else f"{len(self.text)} chars"
        return f"<Response {self.status}, {where}>"


def request(
    url,
    *,
    method="GET",
    headers=None,
    body=None,
    json=None,
    save_to=None,
    follow_redirects=True,
    timeout=None,
):
    """Make an HTTPS request through the worker.

    `body` is `str` or `bytes`, or pass `json` to send a value as JSON. The
    response body comes back as `Response.text`, which has to be UTF-8 and at
    most 256 KiB; pass `save_to` a Volume path to write it there instead,
    which takes binary and larger bodies. `timeout` is in seconds.
    """
    req = {
        "method": method.upper(),
        "url": url,
        "headers": dict(headers or {}),
        "follow_redirects": follow_redirects,
    }
    if timeout is not None:
        req["timeout_ms"] = max(1, int(timeout * 1000))
    if save_to is not None:
        req["output"] = {"type": "file", "path": os.path.abspath(save_to)}
    upload = None
    if json is not None:
        req["body"] = {"type": "json", "json": json}
    elif isinstance(body, str):
        req["body"] = {"type": "text", "text": body}
    elif body is not None:
        try:
            req["body"] = {"type": "text", "text": bytes(body).decode("utf-8")}
        except UnicodeDecodeError:
            # Only text travels inline, so other bytes go through a file.
            upload = _temp_path()
            with open(upload, "wb") as f:
                f.write(body)
            req["body"] = {"type": "file", "path": upload}
    try:
        answer = _json.loads(_subako.http_request(_json.dumps(req)))
    finally:
        if upload is not None:
            _remove(upload)
    if "error" in answer:
        raise HTTPError(answer["error"]["kind"], answer["error"]["message"])
    response = answer["ok"]
    body = response["body"]
    if body["type"] == "file":
        return Response(response["status"], response["headers"], path=body["path"], size=body["bytes"])
    return Response(response["status"], response["headers"], text=body["text"])


def svg_to_png(svg, path, *, scale=1.0):
    """Draw `svg` (`str` or `bytes`) as a PNG file at `path`, at `scale`
    times its own size, and return the PNG's `(width, height)`.

    Text uses the bundled fonts: Inter for sans-serif, serif and any family
    that isn't bundled, IBM Plex Mono for monospace, and Noto Sans JP for
    Japanese. Raises `ValueError` for SVG it can't draw.
    """
    if isinstance(svg, (bytes, bytearray)):
        svg = bytes(svg).decode("utf-8")
    req = {"svg": svg, "path": os.path.abspath(path), "scale": float(scale)}
    answer = _json.loads(_subako.svg_to_png(_json.dumps(req)))
    if "error" in answer:
        raise ValueError(answer["error"])
    return tuple(answer["ok"])


def _temp_path():
    import secrets

    return f"/volume/tmp/.subako-http-{secrets.token_hex(8)}"


def _remove(path):
    try:
        os.remove(path)
    except FileNotFoundError:
        pass


def _install_urllib(urllib_request):
    """Route urllib through `request`, ahead of its socket-based handlers."""
    import http.client
    import io
    import urllib.error
    import urllib.response

    class Handler(urllib_request.AbstractHTTPHandler):
        handler_order = 100

        # The default headers urllib adds: User-Agent, Content-Type, ...
        http_request = https_request = urllib_request.AbstractHTTPHandler.do_request_

        def https_open(self, req):
            headers = {**req.unredirected_hdrs, **req.headers}
            data = req.data
            if data is not None and not isinstance(data, (bytes, bytearray)):
                data = data.read() if hasattr(data, "read") else b"".join(data)
            timeout = req.timeout if isinstance(req.timeout, (int, float)) else None
            # Every body goes through a file, so binary and large ones work.
            path = _temp_path()
            try:
                response = request(
                    req.full_url,
                    method=req.get_method(),
                    headers=headers,
                    body=data,
                    save_to=path,
                    timeout=timeout,
                )
                with open(path, "rb") as f:
                    content = f.read()
            except HTTPError as error:
                raise urllib.error.URLError(f"{error.kind}: {error}") from error
            finally:
                _remove(path)
            message = http.client.HTTPMessage()
            for name, value in response.headers.items():
                message[name] = value
            result = urllib.response.addinfourl(
                io.BytesIO(content), message, req.full_url, response.status
            )
            result.msg = http.client.responses.get(response.status, "")
            return result

        http_open = https_open

    urllib_request.install_opener(urllib_request.build_opener(Handler))


class _UrllibHook:
    """Installs the urllib handler when a script first imports
    urllib.request, so scripts that don't pay nothing for it."""

    def find_spec(self, name, path, target=None):
        if name != "urllib.request":
            return None
        sys.meta_path.remove(self)
        import importlib.util

        spec = importlib.util.find_spec(name)
        exec_module = spec.loader.exec_module

        def exec_and_install(module):
            exec_module(module)
            _install_urllib(module)

        spec.loader.exec_module = exec_and_install
        return spec


sys.meta_path.insert(0, _UrllibHook())
