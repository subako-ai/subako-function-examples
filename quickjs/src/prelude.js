// Runs before the script: what QuickJS lacks that scripts expect. main.rs
// supplies the `__subako*` functions.
(function (global) {
  // Timers run in order once promise jobs settle; delays are ignored, since
  // nothing here waits on anything outside the script.
  var timers = [];
  global.setTimeout = function (fn) {
    var args = Array.prototype.slice.call(arguments, 2);
    timers.push(function () { fn.apply(null, args); });
    return timers.length;
  };
  global.clearTimeout = function () {};
  global.queueMicrotask = function (fn) { Promise.resolve().then(fn); };
  global.__subakoRunTimer = function () {
    var timer = timers.shift();
    if (!timer) return false;
    timer();
    return true;
  };

  // console writes to the call's stdout (log, info, debug) and stderr
  // (warn, error).
  var stdout = [];
  var stderr = [];
  function format(value) {
    if (typeof value === "string") return value;
    if (value instanceof Error) return value.stack ? value.name + ": " + value.message + "\n" + value.stack.trimEnd() : String(value);
    if (typeof value === "function") return "[Function " + (value.name || "anonymous") + "]";
    if (typeof value === "bigint") return value + "n";
    if (value === undefined) return "undefined";
    try {
      var json = JSON.stringify(value, function (k, v) { return typeof v === "bigint" ? v.toString() : v; });
      return json === undefined ? String(value) : json;
    } catch (e) {
      return String(value);
    }
  }
  function writer(lines) {
    return function () { lines.push(Array.prototype.map.call(arguments, format).join(" ")); };
  }
  global.console = {
    log: writer(stdout),
    info: writer(stdout),
    debug: writer(stdout),
    warn: writer(stderr),
    error: writer(stderr),
  };
  global.__subakoStdout = stdout;
  global.__subakoStderr = stderr;

  // UTF-8 only, which is all the platform deals in.
  global.TextEncoder = function TextEncoder() {};
  global.TextEncoder.prototype.encoding = "utf-8";
  global.TextEncoder.prototype.encode = function (text) { return __subakoEncode(String(text === undefined ? "" : text)); };
  global.TextDecoder = function TextDecoder() {};
  global.TextDecoder.prototype.encoding = "utf-8";
  global.TextDecoder.prototype.decode = function (bytes) {
    if (bytes === undefined) return "";
    if (bytes instanceof ArrayBuffer) bytes = new Uint8Array(bytes);
    else if (!(bytes instanceof Uint8Array)) bytes = new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    return __subakoDecode(bytes);
  };

  function bytesOf(body) {
    if (body === undefined || body === null) return undefined;
    if (typeof body === "string") return body;
    if (body instanceof ArrayBuffer) return new Uint8Array(body);
    if (ArrayBuffer.isView(body)) return new Uint8Array(body.buffer, body.byteOffset, body.byteLength);
    return String(body);
  }

  function Headers(entries) { this._map = entries || {}; }
  Headers.prototype.get = function (name) {
    var value = this._map[String(name).toLowerCase()];
    return value === undefined ? null : value;
  };
  Headers.prototype.has = function (name) { return String(name).toLowerCase() in this._map; };
  Headers.prototype.forEach = function (fn) { for (var k in this._map) fn(this._map[k], k, this); };
  Headers.prototype.entries = function () { var map = this._map; return Object.keys(map).map(function (k) { return [k, map[k]]; })[Symbol.iterator](); };
  Headers.prototype[Symbol.iterator] = Headers.prototype.entries;

  function Response(status, headers, body, url) {
    this.status = status;
    this.ok = status >= 200 && status < 300;
    this.headers = new Headers(headers);
    this.url = url;
    this._body = body;
  }
  Response.prototype.arrayBuffer = function () { var b = this._body; return Promise.resolve(b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength)); };
  Response.prototype.bytes = function () { return Promise.resolve(this._body); };
  Response.prototype.text = function () { return Promise.resolve(__subakoDecode(this._body)); };
  Response.prototype.json = function () { return this.text().then(JSON.parse); };

  /** HTTPS through the Subako worker, to hosts the function's network
   * setting allows. Redirects are followed unless `redirect: "manual"`. */
  global.fetch = function (url, init) {
    init = init || {};
    try {
      var headers = {};
      var given = init.headers || {};
      if (given instanceof Headers) given = given._map;
      if (Array.isArray(given)) given.forEach(function (pair) { headers[pair[0]] = String(pair[1]); });
      else for (var k in given) headers[k] = String(given[k]);
      var answer = __subakoFetch(String(url), String(init.method || "GET").toUpperCase(), headers, bytesOf(init.body), init.redirect !== "manual");
      return Promise.resolve(new Response(answer.status, answer.headers, answer.body, String(url)));
    } catch (e) {
      return Promise.reject(new TypeError("fetch failed: " + e.message));
    }
  };
  global.Headers = Headers;
  global.Response = Response;

  /** Files on the Volume. Relative paths resolve from `/tmp`, or from the
   * script's directory for a script file. */
  global.fs = {
    readText: function (path) { return __subakoDecode(__subakoRead(String(path))); },
    readBytes: function (path) { return __subakoRead(String(path)); },
    writeText: function (path, text) { __subakoWrite(String(path), String(text), false); },
    writeBytes: function (path, bytes) { __subakoWrite(String(path), bytesOf(bytes), false); },
    appendText: function (path, text) { __subakoWrite(String(path), String(text), true); },
    list: function (path) { return __subakoList(String(path === undefined ? "." : path)); },
    exists: function (path) { return __subakoExists(String(path)); },
    remove: function (path) { __subakoRemove(String(path)); },
  };
})(globalThis);
