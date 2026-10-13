# anydoc

A Subako function package that converts documents to GitHub-Flavored Markdown with [anydoc](https://github.com/firecrawl/anydoc), so an agent can read files on its Volume.

It reads Word (`.doc`, `.docx`), PowerPoint (`.ppt`, `.pptx`), Excel (`.xls`, `.xlsx`, `.xlsb`), OpenDocument (`.odt`, `.ods`, `.odp`), RTF, EPUB, CSV and text-based PDF.

## Function

### `convert_to_markdown`

| Input | |
| --- | --- |
| `path` | The document on the Volume, under `/tmp` or `/data` |
| `format` | Optional. The format as an extension (`csv`, `docx`, ...). Only CSV, which has no signature, needs it when the file lacks a `.csv` extension; everything else is detected from the content |
| `output` | Optional. `{"type": "inline"}` (the default) returns the Markdown in the reply; `{"type": "file", "path": "/data/report.md"}` writes it to the Volume instead, for long documents. Missing directories are created; the input file is never overwritten |

```json
{ "path": "/data/report.docx" }
```

returns

```json
{ "format": "docx", "markdown": { "type": "inline", "text": "# Report\n\n..." } }
```

and with `"output": {"type": "file", "path": "/data/report.md"}`

```json
{ "format": "docx", "markdown": { "type": "file", "path": "/data/report.md", "bytes": 1234 } }
```

## PDFs that need OCR

anydoc does no OCR, and this package makes no network calls. anydoc itself refuses a PDF with any scanned or image-only page, so that output with pages missing never passes as complete. This package instead returns the pages that have text, marks each skipped page where it would be, and lists them in `pages_needing_ocr`:

```json
{
  "format": "pdf",
  "markdown": {
    "type": "inline",
    "text": "## Text on the first page\n\n<!-- page 2 of 2 needs OCR and was not converted -->\n"
  },
  "pages_needing_ocr": [2]
}
```

A PDF whose pages all need OCR has nothing to return, so it fails:

```json
{ "error": "all 2 pages need OCR: this PDF has only scanned or image-only pages and cannot be converted without OCR" }
```

## Build

```sh
cargo test
./package.sh                                          # dist/anydoc-function.tar.gz
subako function create dist/anydoc-function.tar.gz
```

To try the module locally with [wasmtime](https://wasmtime.dev), mount a directory as `/tmp`:

```sh
echo '{"path":"/tmp/text.pdf"}' | wasmtime run --argv0 anydoc --dir tests/fixtures::/tmp \
    target/wasm32-wasip1/release/anydoc-function.wasm convert_to_markdown
```

The files in `tests/fixtures/` come from anydoc's own test corpus, under its [MIT license](https://github.com/firecrawl/anydoc/blob/main/LICENSE).
