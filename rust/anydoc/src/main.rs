use std::path::Path;

use anydoc::{ConvertError, Format};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use subako_function_sdk::{ToolError, function, package};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ConvertInput {
    /// Path of the document on the session's Volume, under `/volume/tmp` or `/volume/data`.
    path: String,
    /// The format as a file extension, such as `csv` or `docx`. Only needed
    /// for CSV, which has no signature, or a file whose content and extension
    /// are both unrecognized; otherwise the format is read from the content.
    #[serde(default)]
    format: Option<String>,
    /// Where the Markdown goes.
    #[serde(default)]
    output: MarkdownOutput,
}

/// Where the Markdown goes, shaped like the SDK's HTTP `ResponseOutput`.
#[derive(Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum MarkdownOutput {
    /// Return the Markdown as text in the reply (the default).
    #[default]
    Inline,
    /// Save the Markdown to a Volume file and reply with its size. Use this
    /// for documents too long to read in one call.
    File {
        /// Absolute Volume path. Missing directories are created and an
        /// existing file is replaced, but never the input document itself.
        path: String,
    },
}

#[derive(Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Markdown {
    /// The document as GitHub-Flavored Markdown.
    Inline { text: String },
    /// The Volume file the Markdown was written to, and its size in bytes.
    File { path: String, bytes: usize },
}

#[derive(Serialize, JsonSchema)]
struct Converted {
    /// The format the document was read as.
    format: String,
    markdown: Markdown,
    /// 1-indexed PDF pages left out because they are scanned or image-only
    /// and need OCR. Each one is marked in the Markdown where it would be.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pages_needing_ocr: Vec<u32>,
}

/// Convert a document on the Volume to GitHub-Flavored Markdown. Reads Word
/// (doc, docx), PowerPoint (ppt, pptx), Excel (xls, xlsx, xlsb), OpenDocument
/// (odt, ods, odp), RTF, EPUB, CSV and text-based PDF. Does no OCR: scanned or
/// image-only PDF pages are left out and listed in `pages_needing_ocr`, and a
/// PDF with no text pages at all fails.
#[function]
fn convert_to_markdown(input: ConvertInput) -> Result<Converted, ToolError> {
    let path = Path::new(&input.path);
    if let MarkdownOutput::File { path: out } = &input.output {
        if same_path(path, Path::new(out)) {
            return Err(ToolError::new(
                "the output file is the input document; choose another path, such as one ending in .md",
            ));
        }
    }
    let bytes = std::fs::read(path)
        .map_err(|e| ToolError::new(format!("cannot read {}: {e}", path.display())))?;

    let format = match &input.format {
        Some(name) => Format::from_extension(name.trim_start_matches('.'))
            .ok_or_else(|| ToolError::new(format!("unknown format: {name}")))?,
        None => Format::from_bytes(&bytes)
            .or_else(|| Format::from_path(path))
            .ok_or_else(|| {
                ToolError::new(format!(
                    "unrecognized file content and extension: {}; pass `format`",
                    path.display()
                ))
            })?,
    };

    let (markdown, pages_needing_ocr) = match anydoc::to_markdown_bytes(&bytes, format) {
        Ok(markdown) => (markdown, Vec::new()),
        Err(ConvertError::NeedsOcr { pages, page_count }) if (pages.len() as u32) < page_count => {
            (text_pages(&bytes, &pages)?, pages)
        }
        Err(error) => return Err(describe(error)),
    };
    let markdown = match input.output {
        MarkdownOutput::Inline => Markdown::Inline { text: markdown },
        MarkdownOutput::File { path: out } => {
            if let Some(dir) = Path::new(&out).parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| ToolError::new(format!("cannot create {}: {e}", dir.display())))?;
            }
            std::fs::write(&out, &markdown)
                .map_err(|e| ToolError::new(format!("cannot write {out}: {e}")))?;
            Markdown::File { path: out, bytes: markdown.len() }
        }
    };
    Ok(Converted {
        format: format_name(format).to_string(),
        markdown,
        pages_needing_ocr,
    })
}

/// Whether two paths name the same file, after resolving `.` and `..`, which
/// WASI has no `canonicalize` for.
fn same_path(a: &Path, b: &Path) -> bool {
    fn normalize(path: &Path) -> Vec<std::path::Component<'_>> {
        use std::path::Component;
        let mut parts = Vec::new();
        for part in path.components() {
            match part {
                Component::CurDir => {}
                Component::ParentDir => {
                    parts.pop();
                }
                part => parts.push(part),
            }
        }
        parts
    }
    normalize(a) == normalize(b)
}

/// anydoc refuses a PDF with any page that needs OCR, so the pages with text
/// are extracted one by one instead, with a marker where each other page was.
fn text_pages(bytes: &[u8], needing_ocr: &[u32]) -> Result<String, ToolError> {
    let extraction = pdf_inspector::extract_pages_markdown_mem(bytes, None)
        .map_err(|e| ToolError::new(format!("cannot read PDF pages: {e}")))?;
    let page_count = extraction.pages.len();
    let pages: Vec<String> = extraction
        .pages
        .into_iter()
        .map(|page| {
            let number = page.page + 1;
            if needing_ocr.contains(&number) {
                format!("<!-- page {number} of {page_count} needs OCR and was not converted -->")
            } else {
                page.markdown.trim().to_string()
            }
        })
        .filter(|page| !page.is_empty())
        .collect();
    Ok(pages.join("\n\n") + "\n")
}

/// The model reads only the message, so it says what to do next where there
/// is something to do.
fn describe(error: ConvertError) -> ToolError {
    match error {
        ConvertError::NeedsOcr { .. } => ToolError::new(format!(
            "{error}: this PDF has only scanned or image-only pages and cannot be converted without OCR"
        )),
        error => ToolError::new(error.to_string()),
    }
}

fn format_name(format: Format) -> &'static str {
    match format {
        Format::Doc => "doc",
        Format::Docx => "docx",
        Format::Odt => "odt",
        Format::Pdf => "pdf",
        Format::Ppt => "ppt",
        Format::Pptx => "pptx",
        Format::Rtf => "rtf",
        Format::Epub => "epub",
        Format::Excel => "excel",
        Format::Ods => "ods",
        Format::Odp => "odp",
        Format::Csv => "csv",
    }
}

package! {
    name: "anydoc",
    description: "Converts office documents, EPUB, CSV and text-based PDFs to Markdown with anydoc.",
    functions: [convert_to_markdown],
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    fn convert(path: String, format: Option<&str>) -> Result<Converted, ToolError> {
        convert_to_markdown(ConvertInput {
            path,
            format: format.map(str::to_string),
            output: MarkdownOutput::Inline,
        })
    }

    fn convert_to_file(path: String, out: &Path) -> Result<Converted, ToolError> {
        convert_to_markdown(ConvertInput {
            path,
            format: None,
            output: MarkdownOutput::File { path: out.display().to_string() },
        })
    }

    fn text(converted: Converted) -> String {
        match converted.markdown {
            Markdown::Inline { text } => text,
            other => panic!("expected inline Markdown, got {other:?}"),
        }
    }

    #[test]
    fn converts_docx() {
        let converted = convert(fixture("rich.docx"), None).unwrap();
        assert_eq!(converted.format, "docx");
        assert!(text(converted).contains("| Q1 | 10 |"));
    }

    #[test]
    fn converts_text_pdf() {
        let converted = convert(fixture("text.pdf"), None).unwrap();
        assert_eq!(converted.format, "pdf");
        assert!(!text(converted).trim().is_empty());
    }

    #[test]
    fn scanned_pdf_needs_ocr() {
        let error = convert(fixture("scanned.pdf"), None).err().unwrap();
        assert!(error.message().contains("OCR"), "{}", error.message());
    }

    #[test]
    fn partly_scanned_pdf_keeps_text_pages() {
        let converted = convert(fixture("mixed.pdf"), None).unwrap();
        assert_eq!(converted.pages_needing_ocr, [2]);
        let markdown = text(converted);
        assert!(markdown.contains("page 2 of 2 needs OCR"), "{markdown}");
        assert!(markdown.len() > "<!-- page 2 of 2 needs OCR and was not converted -->\n".len());
    }

    #[test]
    fn csv_from_extension_or_named_format() {
        let dir = std::env::temp_dir().join("anydoc-function-test");
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("table.csv");
        std::fs::write(&csv, "name,qty\napple,3\n").unwrap();
        let converted = convert(csv.display().to_string(), None).unwrap();
        assert!(text(converted).contains("| apple"));

        let unnamed = dir.join("table");
        std::fs::write(&unnamed, "name,qty\napple,3\n").unwrap();
        assert!(convert(unnamed.display().to_string(), None).is_err());
        let converted = convert(unnamed.display().to_string(), Some("csv")).unwrap();
        assert_eq!(converted.format, "csv");
    }

    #[test]
    fn writes_to_a_file() {
        let out = std::env::temp_dir().join("anydoc-function-test-out.md");
        let converted = convert_to_file(fixture("rich.docx"), &out).unwrap();
        let bytes = std::fs::read(&out).unwrap().len();
        assert_eq!(converted.markdown, Markdown::File { path: out.display().to_string(), bytes });
    }

    #[test]
    fn output_defaults_to_inline() {
        let input: ConvertInput = serde_json::from_str(r#"{"path": "/volume/tmp/a.docx"}"#).unwrap();
        assert_eq!(input.output, MarkdownOutput::Inline);
        let input: ConvertInput = serde_json::from_str(
            r#"{"path": "/volume/tmp/a.docx", "output": {"type": "file", "path": "/volume/tmp/a.md"}}"#,
        )
        .unwrap();
        assert_eq!(input.output, MarkdownOutput::File { path: "/volume/tmp/a.md".to_string() });
        let unknown = r#"{"path": "/volume/tmp/a.docx", "output_path": "/volume/tmp/a.md"}"#;
        assert!(serde_json::from_str::<ConvertInput>(unknown).is_err());
    }

    #[test]
    fn creates_output_directories() {
        let out = std::env::temp_dir().join("anydoc-function-test-dirs/a/b/out.md");
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("anydoc-function-test-dirs"));
        convert_to_file(fixture("rich.docx"), &out).unwrap();
        assert!(out.exists());
    }

    #[test]
    fn refuses_to_overwrite_the_input() {
        let dir = std::env::temp_dir().join("anydoc-function-test-same");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("rich.docx");
        std::fs::copy(fixture("rich.docx"), &input).unwrap();
        let error = convert_to_file(input.display().to_string(), &dir.join("./x/../rich.docx"))
            .err()
            .unwrap();
        assert!(error.message().contains("input document"), "{}", error.message());
        assert_eq!(std::fs::read(&input).unwrap(), std::fs::read(fixture("rich.docx")).unwrap());
    }
}
