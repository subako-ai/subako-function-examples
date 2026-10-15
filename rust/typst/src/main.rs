use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use subako_function_sdk::{ToolError, function, http, package};
use typst::diag::{Severity, SourceDiagnostic};
use typst::foundations::Bytes;
use typst::syntax::{FileId, RootedPath, VirtualPath, VirtualRoot};
use typst::{World, WorldExt};
use typst_layout::PagedDocument;

mod packages;
mod world;

/// The Markdown renderer: cmarker, bundled in the package's assets.
const CMARKER: &str = "@preview/cmarker:0.1.10";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RenderInput {
    /// What to typeset.
    source: Source,
    /// Volume path to write the PDF to, such as `/volume/tmp/report.pdf`.
    output: String,
    /// The look of the document. Defaults to `subako`.
    #[serde(default)]
    template: Template,
    /// The document's title: shown large at the top and in the header by the
    /// Subako template, and set as the PDF's title.
    #[serde(default)]
    title: Option<String>,
    /// Typst code run before the source, after the template, to adjust it:
    /// for example `#set page(paper: "us-letter")` or `#set text(size: 11pt)`.
    #[serde(default)]
    preamble: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Template {
    /// The subako.ai look: the Subako logo and the document title in the
    /// header, page numbers in the footer, Playfair Display headings over
    /// Inter text on the site's paper color.
    #[default]
    Subako,
    /// Typst's defaults: plain A4, Libertinus Serif, no header or footer.
    Plain,
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Source {
    /// Typst markup.
    Typst { text: String },
    /// CommonMark Markdown, with GitHub tables, strikethrough and task lists.
    Markdown { text: String },
    /// A `.typ` file, or a `.md` file read as Markdown, on the Volume.
    File { path: String },
}

#[derive(Debug, Serialize, JsonSchema)]
struct Rendered {
    /// Where the PDF was written.
    path: String,
    pages: usize,
    bytes: usize,
    /// Warnings from Typst, as `file:line:column: message`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

/// Typeset Typst markup or Markdown into a PDF on the Volume, with Typst 0.15.
/// By default the PDF uses the Subako template (the Subako logo, the site's
/// fonts and colors); `template: "plain"` gives Typst's defaults. Paths in the
/// source resolve against the Volume: relative ones from the source file's
/// directory, or from `/volume/tmp` for text, so images and data files on the Volume
/// can be used. `@preview` packages are downloaded from packages.typst.org when
/// the function's network setting allows it. Errors come back as
/// `file:line:column: message`.
#[function]
fn render_pdf(input: RenderInput) -> Result<Rendered, ToolError> {
    let fetch: packages::Fetch = Arc::new(|url: &str, dest: &Path| {
        let mut request = http::Request::get(url);
        request.output = http::ResponseOutput::File { path: dest.display().to_string() };
        http::request(&request).map(|r| r.status).map_err(|e| e.message)
    });
    let setup = Setup { assets: PathBuf::from("/package"), cache: PathBuf::from("/volume/tmp/.typst-packages"), fetch };
    render(input, setup)
}

package! {
    name: "typst",
    description: "Typesets Typst or Markdown into PDF on the Volume, with a Subako template.",
    functions: [render_pdf],
}

/// Where the package's assets are and how packages are fetched, so tests can
/// use the repository's assets/ and a fake worker.
struct Setup {
    /// The package's assets: `templates/`, `fonts/` and `packages/`.
    assets: PathBuf,
    /// Where downloaded Typst packages are kept.
    cache: PathBuf,
    fetch: packages::Fetch,
}

fn render(input: RenderInput, setup: Setup) -> Result<Rendered, ToolError> {
    // The source as Typst code for the main file, and the directory its
    // relative paths resolve from. Typst source, given as text or a file, is
    // included rather than pasted, so its errors keep their own lines.
    let project = |path: &Path| -> Result<RootedPath, ToolError> {
        let vpath = VirtualPath::new(path.display().to_string())
            .map_err(|e| ToolError::new(format!("bad path {}: {e:?}", path.display())))?;
        Ok(RootedPath::new(VirtualRoot::Project, vpath))
    };
    let tmp = PathBuf::from("/volume/tmp");
    let mut memory = HashMap::new();
    let markdown = |text: &str| {
        // `image` is passed in so image paths resolve from the main file.
        format!(
            "#import \"{CMARKER}\"\n\
             #cmarker.render(\n  \
               \"{}\",\n  \
               scope: (image: (source, ..args) => image(source, ..args)),\n\
             )\n",
            typst_string(text),
        )
    };
    let (body, dir) = match &input.source {
        Source::Markdown { text } => (markdown(text), tmp),
        Source::Typst { text } => {
            // Held in memory under a path no file is likely to have.
            let path = tmp.join(".subako-input.typ");
            memory.insert(FileId::new(project(&path)?), ("<input>".to_string(), Bytes::from_string(text.clone())));
            (format!("#include \"{}\"\n", typst_string(&path.display().to_string())), tmp)
        }
        Source::File { path } => {
            let dir = Path::new(path).parent().map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
            if path.ends_with(".md") || path.ends_with(".markdown") {
                let text = std::fs::read_to_string(path)
                    .map_err(|e| ToolError::new(format!("cannot read {path}: {e}")))?;
                (markdown(&text), dir)
            } else {
                (format!("#include \"{}\"\n", typst_string(path)), dir)
            }
        }
    };

    let mut main_text = String::new();
    match input.template {
        Template::Subako => {
            let template = setup.assets.join("templates/subako/template.typ");
            let title = input.title.as_deref().map_or("none".to_string(), |t| format!("\"{}\"", typst_string(t)));
            main_text.push_str(&format!(
                "#import \"{}\": subako\n#show: subako.with(title: {title})\n",
                typst_string(&template.display().to_string()),
            ));
        }
        Template::Plain => {
            if let Some(title) = &input.title {
                main_text.push_str(&format!("#set document(title: \"{}\")\n", typst_string(title)));
            }
        }
    }
    if let Some(preamble) = &input.preamble {
        main_text.push_str(preamble);
        main_text.push('\n');
    }
    main_text.push_str(&body);
    // Placed in `dir`, so the source's relative paths resolve from there.
    let main = FileId::unique(project(&dir.join("main.typ"))?);
    memory.insert(main, ("<main>".to_string(), Bytes::from_string(main_text)));

    let packages = packages::Packages {
        bundled: setup.assets.join("packages"),
        cache: setup.cache,
        fetch: setup.fetch,
    };
    let world = world::SubakoWorld::new(main, memory, &setup.assets.join("fonts"), packages);
    let compiled = typst::compile::<PagedDocument>(&world);
    let warnings = compiled.warnings.iter().map(|d| describe(&world, d)).collect();
    let document = compiled.output.map_err(|errors| report(&world, &errors))?;
    let pdf = typst_pdf::pdf(&document, &typst_pdf::PdfOptions::default())
        .map_err(|errors| report(&world, &errors))?;

    let output = Path::new(&input.output);
    if let Source::File { path } = &input.source
        && Path::new(path) == output
    {
        return Err(ToolError::new("`output` is the source file; choose a path ending in .pdf"));
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ToolError::new(format!("cannot create {}: {e}", parent.display())))?;
    }
    std::fs::write(output, &pdf)
        .map_err(|e| ToolError::new(format!("cannot write {}: {e}", input.output)))?;
    Ok(Rendered { path: input.output, pages: document.pages().len(), bytes: pdf.len(), warnings })
}

/// `text` escaped for the inside of a Typst string literal.
fn typst_string(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c => escaped.push(c),
        }
    }
    escaped
}

fn report(world: &world::SubakoWorld, errors: &[SourceDiagnostic]) -> ToolError {
    let lines: Vec<String> = errors.iter().map(|d| describe(world, d)).collect();
    ToolError::new(lines.join("\n"))
}

/// `file:line:column: error: message`, then its hints.
fn describe(world: &world::SubakoWorld, diagnostic: &SourceDiagnostic) -> String {
    let severity = match diagnostic.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    let location = diagnostic.span.id().map(|id| {
        let position = world
            .range(diagnostic.span)
            .zip(world.source(id).ok())
            .and_then(|(range, source)| source.lines().byte_to_line_column(range.start))
            .map(|(line, column)| format!(":{}:{}", line + 1, column + 1))
            .unwrap_or_default();
        format!("{}{position}: ", world.display(id))
    });
    let mut text = format!("{}{severity}: {}", location.unwrap_or_default(), diagnostic.message);
    for hint in &diagnostic.hints {
        text.push_str(&format!("\n  hint: {}", hint.v));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repository's assets/; in wasmtime, mounted at /assets.
    fn assets() -> PathBuf {
        if cfg!(target_os = "wasi") {
            PathBuf::from("/assets")
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets")
        }
    }

    /// The repository's assets, a fresh cache, and a worker that must not be
    /// called.
    fn setup(name: &str) -> (Setup, PathBuf) {
        // WASI has no `temp_dir`; the wasmtime runner maps target/ to /volume/tmp.
        let base = if cfg!(target_os = "wasi") { PathBuf::from("/volume/tmp") } else { std::env::temp_dir() };
        let dir = base.join(format!("typst-function-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let setup = Setup {
            assets: assets(),
            cache: dir.join("cache"),
            fetch: Arc::new(|url: &str, _: &Path| panic!("unexpected download of {url}")),
        };
        (setup, dir)
    }

    fn input(source: Source, output: &Path) -> RenderInput {
        RenderInput {
            source,
            output: output.display().to_string(),
            template: Template::Subako,
            title: None,
            preamble: None,
        }
    }

    fn render_ok(request: RenderInput, setup: Setup) -> Rendered {
        let output = PathBuf::from(&request.output);
        let rendered = render(request, setup).unwrap_or_else(|e| panic!("{}", e.message()));
        let pdf = std::fs::read(&output).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        assert_eq!(rendered.bytes, pdf.len());
        rendered
    }

    #[test]
    fn typesets_typst_text() {
        let (setup, dir) = setup("typst-text");
        let mut request = input(Source::Typst { text: "= Hello\n\n#lorem(800)".to_string() }, &dir.join("out.pdf"));
        request.template = Template::Plain;
        let rendered = render_ok(request, setup);
        assert_eq!(rendered.pages, 2);
        assert!(rendered.warnings.is_empty(), "{rendered:?}");
    }

    #[test]
    fn typesets_markdown_with_the_subako_template() {
        let (setup, dir) = setup("markdown");
        let markdown = "# Report\n\nSome **bold** text and `code`.\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\n- 日本語のテキスト\n";
        let mut request = input(Source::Markdown { text: markdown.to_string() }, &dir.join("report.pdf"));
        request.title = Some("Quarterly \"report\"".to_string());
        let rendered = render_ok(request, setup);
        assert_eq!(rendered.pages, 1);
        // Every glyph found a font, Japanese included.
        assert!(rendered.warnings.is_empty(), "{rendered:?}");
    }

    #[test]
    fn resolves_paths_from_the_source_file() {
        let (_, dir) = setup("files");
        std::fs::create_dir_all(dir.join("doc/img")).unwrap();
        std::fs::write(
            dir.join("doc/img/dot.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><circle cx="5" cy="5" r="5"/></svg>"#,
        )
        .unwrap();
        std::fs::write(dir.join("doc/data.csv"), "name,qty\napple,3\n").unwrap();
        std::fs::write(
            dir.join("doc/report.typ"),
            "#image(\"img/dot.svg\", width: 1cm)\n#table(columns: 2, ..csv(\"data.csv\").flatten())\n",
        )
        .unwrap();
        std::fs::write(dir.join("doc/notes.md"), "# Notes\n\n![a dot](img/dot.svg)\n").unwrap();
        for source in ["report.typ", "notes.md"] {
            let path = dir.join("doc").join(source).display().to_string();
            render_ok(input(Source::File { path }, &dir.join(format!("out/{source}.pdf"))), setup_from(&dir));
        }
    }

    fn setup_from(dir: &Path) -> Setup {
        Setup {
            assets: assets(),
            cache: dir.join("cache"),
            fetch: Arc::new(|url: &str, _: &Path| panic!("unexpected download of {url}")),
        }
    }

    #[test]
    fn reports_errors_with_their_location() {
        let (setup, dir) = setup("errors");
        let text = "= Title\n\n#let x = 1\n#undefined-thing\n";
        let error = render(input(Source::Typst { text: text.to_string() }, &dir.join("out.pdf")), setup)
            .err()
            .unwrap();
        assert!(
            error.message().starts_with("<input>:4:2: error: unknown variable: undefined-thing"),
            "{}",
            error.message()
        );
        assert!(!dir.join("out.pdf").exists());
    }

    #[test]
    fn applies_the_preamble() {
        let (setup, dir) = setup("preamble");
        let mut request = input(Source::Markdown { text: "Hello".to_string() }, &dir.join("out.pdf"));
        request.preamble = Some("#set page(paper: \"a6\")\n#set text(size: 30pt)".to_string());
        request.title = Some("A small page".to_string());
        assert!(render_ok(request, setup).pages > 1);
    }

    #[test]
    fn refuses_to_overwrite_the_source() {
        let (setup, dir) = setup("overwrite");
        let path = dir.join("doc.typ");
        std::fs::write(&path, "Hi").unwrap();
        let error = render(input(Source::File { path: path.display().to_string() }, &path), setup)
            .err()
            .unwrap();
        assert!(error.message().contains("is the source file"), "{}", error.message());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Hi");
    }

    /// `@preview/greet:0.1.0`, packed as packages.typst.org serves it.
    fn greet_package() -> Vec<u8> {
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (name, text) in [
            ("typst.toml", "[package]\nname = \"greet\"\nversion = \"0.1.0\"\nentrypoint = \"lib.typ\"\n"),
            ("lib.typ", "#let greet(name) = [Hello, #name!]\n"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(text.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, text.as_bytes()).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn downloads_preview_packages_once() {
        let (mut setup, dir) = setup("download");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        setup.fetch = Arc::new(move |url: &str, dest: &Path| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(url, "https://packages.typst.org/preview/greet-0.1.0.tar.gz");
            std::fs::write(dest, greet_package()).unwrap();
            Ok(200)
        });
        let cache = setup.cache.clone();
        let text = "#import \"@preview/greet:0.1.0\": greet\n#greet(\"Subako\")";
        render_ok(input(Source::Typst { text: text.to_string() }, &dir.join("a.pdf")), setup);
        assert!(cache.join("preview/greet/0.1.0/lib.typ").exists());
        // The second compilation finds it in the cache.
        let again = Setup { cache, ..setup_from(&dir) };
        render_ok(input(Source::Typst { text: text.to_string() }, &dir.join("b.pdf")), again);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn rejects_a_package_that_escapes_its_directory() {
        let (mut setup, dir) = setup("escape");
        setup.fetch = Arc::new(|_: &str, dest: &Path| {
            // `tar::Builder` refuses `..`, so the header is written by hand.
            let mut header = tar::Header::new_gnu();
            let name = b"../escaped.typ";
            header.as_old_mut().name[..name.len()].copy_from_slice(name);
            header.set_size(2);
            header.set_mode(0o644);
            header.set_cksum();
            let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
            tar.append(&header, &b"hi"[..]).unwrap();
            std::fs::write(dest, tar.into_inner().unwrap().finish().unwrap()).unwrap();
            Ok(200)
        });
        let cache = setup.cache.clone();
        let text = "#import \"@preview/greet:0.1.0\": greet";
        let error = render(input(Source::Typst { text: text.to_string() }, &dir.join("out.pdf")), setup)
            .err()
            .unwrap();
        assert!(error.message().contains("unsafe path"), "{}", error.message());
        assert!(!cache.join("escaped.typ").exists());
        assert!(!cache.join("preview/greet").exists());
    }

    #[test]
    fn explains_a_package_it_cannot_download() {
        let (mut setup, dir) = setup("offline");
        setup.fetch = Arc::new(|_: &str, _: &Path| Err("packages.typst.org is not allowed".to_string()));
        let text = "#import \"@preview/greet:0.1.0\": greet";
        let error = render(input(Source::Typst { text: text.to_string() }, &dir.join("out.pdf")), setup)
            .err()
            .unwrap();
        assert!(error.message().contains("network setting has to allow packages.typst.org"), "{}", error.message());
    }
}
