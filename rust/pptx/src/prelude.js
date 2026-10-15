// Runs before PptxGenJS: what it and scripts need that QuickJS doesn't have,
// and the helpers scripts use. main.rs supplies the `__subako*` functions.
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
  global.__subakoRunTimer = function () {
    var timer = timers.shift();
    if (!timer) return false;
    timer();
    return true;
  };

  // console writes to the `logs` the call returns.
  var logs = [];
  function format(value) {
    if (typeof value === "string") return value;
    if (value instanceof Error) return String(value);
    try {
      var json = JSON.stringify(value);
      return json === undefined ? String(value) : json;
    } catch (e) {
      return String(value);
    }
  }
  function logger(prefix) {
    return function () {
      logs.push(prefix + Array.prototype.map.call(arguments, format).join(" "));
    };
  }
  global.console = {
    log: logger(""),
    info: logger(""),
    debug: logger(""),
    warn: logger("warning: "),
    error: logger("error: "),
  };
  global.__subakoLogs = logs;

  /** Image data for `addImage({ data })` or a background: a PNG, JPEG or
   * GIF on the Volume, or an https URL fetched through the Subako worker. */
  global.image = function (source) {
    return __subakoImage(String(source));
  };

  /** A UTF-8 text file on the Volume, such as CSV or JSON to chart. */
  global.readText = function (path) {
    return __subakoReadText(String(path));
  };
})(globalThis);
