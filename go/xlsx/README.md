# xlsx

A Subako function package that writes and reads Excel workbooks on the Volume with [excelize](https://github.com/qax-os/excelize), written in Go with [subako-function-sdk-go](https://github.com/subako-ai/subako-function-sdk-go) and the [official MCP Go SDK](https://github.com/modelcontextprotocol/go-sdk).

## Functions

### `create_xlsx`

| Input | |
| --- | --- |
| `path` | Volume path for the workbook, such as `/volume/tmp/report.xlsx`. Missing directories are created |
| `sheets` | The sheets, in order. Each has a `name` and `rows` of cells from A1: numbers, booleans, strings, or `null` for an empty cell. A string starting with `=` is a formula |
| `sheets[].header` | Optional. Bolds the first row and keeps it in view while scrolling |
| `sheets[].column_widths` | Optional. Widths of columns A, B, ... in characters |
| `sheets[].number_formats` | Optional. Number formats by column letter, such as `{"C": "#,##0.00"}` |

```json
{
  "path": "/volume/tmp/q3.xlsx",
  "sheets": [{
    "name": "Q3",
    "header": true,
    "number_formats": { "C": "#,##0.00" },
    "rows": [
      ["region", "units", "revenue"],
      ["Tokyo", 120, 15600.5],
      ["Osaka", 80, 9800],
      ["Total", "=SUM(B2:B3)", "=SUM(C2:C3)"]
    ]
  }]
}
```

returns what each formula calculates to, so a mistake shows up before anyone opens the file:

```json
{
  "path": "/volume/tmp/q3.xlsx",
  "bytes": 6359,
  "sheets": [{ "name": "Q3", "range": "A1:C4" }],
  "calculated": { "Q3!B4": "200", "Q3!C4": "25,400.50" }
}
```

### `read_xlsx`

| Input | |
| --- | --- |
| `path` | The workbook on the Volume |
| `sheet` | Optional. The sheet to read; the first sheet by default |
| `range` | Optional. The cells to read, such as `A1:F20`; the sheet's used range by default |

It returns every sheet's name and used range, the cells as Excel displays them with formulas calculated, and each formula. At most 500 rows come back; `truncated` says when the range had more.

```json
{
  "sheets": [{ "name": "Q3", "range": "A1:C4" }],
  "sheet": "Q3",
  "range": "A1:C4",
  "rows": [["region", "units", "revenue"], ["Tokyo", "120", "15,600.50"], ["Osaka", "80", "9,800.00"], ["Total", "200", "25,400.50"]],
  "formulas": { "B4": "SUM(B2:B3)", "C4": "SUM(C2:C3)" }
}
```

## Build

Needs Go 1.25.

```sh
go test ./...
./package.sh                           # dist/xlsx.tar.gz
subako function create dist/xlsx.tar.gz
```

To try the module locally with [wasmtime](https://wasmtime.dev), mount a directory as `/volume/tmp`. wasmtime has no `subako_http` import, so `tests/subako_http_stub.wat` stands in for it:

```sh
GOOS=wasip1 GOARCH=wasm go build -o dist/module.wasm .
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"create_xlsx","arguments":{"path":"/volume/tmp/hi.xlsx","sheets":[{"name":"Hi","rows":[["n",1],["double","=B1*2"]]}]}}}' |
    wasmtime run --preload subako_http=tests/subako_http_stub.wat --argv0 xlsx \
    --dir /path/to/scratch::/volume/tmp dist/module.wasm
```

excelize is under the [BSD 3-Clause license](https://github.com/qax-os/excelize/blob/master/LICENSE).
