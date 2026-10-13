use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyrender::{PaintScene as _, render_to_buffer};
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{BaseDocument, DocumentConfig, StyleThreading, util::Color};
use blitz_html::HtmlDocument;
use blitz_traits::net::Url;
use blitz_traits::shell::{ColorScheme, Viewport};
use peniko::{Fill, kurbo::Rect};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use subako_function_sdk::{ToolError, function, http, package};

mod fonts;
mod net;

use net::NotLoaded;

/// The tallest full-page capture, in CSS pixels; longer pages are cut here.
const MAX_PAGE_HEIGHT: f32 = 16_384.0;
/// The most pixels a PNG can have, which bounds the memory a call uses.
const MAX_PIXELS: u64 = 40_000_000;
/// How many elements that stick out past the viewport are named.
const MAX_OVERFLOWING: usize = 10;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RenderInput {
    /// The page to draw.
    source: Source,
    /// Volume path to write the PNG to, such as `/tmp/page.png`.
    output: String,
    /// The viewport's width in CSS pixels, as a browser window's. Defaults to
    /// 1280; 390 is a phone.
    #[serde(default = "default_width")]
    width: u32,
    /// The viewport's height in CSS pixels, which `vh` units and
    /// `full_page: false` use. Defaults to 800.
    #[serde(default = "default_height")]
    height: u32,
    /// Device pixels per CSS pixel: 2 draws a sharper image twice the size,
    /// as on a high-density screen. Defaults to 1.
    #[serde(default = "default_scale")]
    scale: f32,
    /// Capture the whole page (the default), or only the viewport.
    #[serde(default = "default_full_page")]
    full_page: bool,
    /// The `prefers-color-scheme` the page sees. Defaults to `light`.
    #[serde(default)]
    color_scheme: Scheme,
}

fn default_width() -> u32 {
    1280
}
fn default_height() -> u32 {
    800
}
fn default_scale() -> f32 {
    1.0
}
fn default_full_page() -> bool {
    true
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Source {
    /// HTML given as a string. Relative URLs in it resolve from `/tmp`.
    Html { html: String },
    /// An HTML file on the Volume. Relative URLs in it resolve from its
    /// directory, so stylesheets and images next to it load.
    File { path: String },
}

#[derive(Debug, Default, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Scheme {
    #[default]
    Light,
    Dark,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Rendered {
    /// Where the PNG was written.
    output: String,
    /// The PNG's size in pixels.
    image: Size,
    /// The page's full size in CSS pixels. A width over the viewport's means
    /// the page scrolls sideways.
    page: Size,
    /// Elements that stick out past the right edge of the viewport, outermost
    /// first, such as `section.panel.full (right edge at 512px)`. Empty when
    /// the page fits.
    overflowing: Vec<String>,
    /// Stylesheets, images and fonts the page asked for but didn't get.
    not_loaded: Vec<NotLoaded>,
    /// What else to know about the image: that scripts didn't run, or that
    /// the page was cut short.
    notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
struct Size {
    width: u32,
    height: u32,
}

/// Draw an HTML page to a PNG on the Volume, to see how it looks: check the
/// layout of HTML you wrote, at a desktop width or a phone's (`width: 390`).
/// Uses Blitz, an HTML and CSS engine built on Firefox's CSS code: flexbox,
/// grid, media queries, CSS variables, gradients, shadows, web fonts, and SVG
/// and raster images work. It runs no JavaScript, so content scripts would
/// create is missing. Stylesheets, images and fonts load from the Volume
/// (relative to the HTML file), from data: URLs, and from https URLs the
/// function's network setting allows. The result also lists elements that
/// stick out past the viewport's right edge, and resources that didn't load.
/// Without a font-family of its own the page uses Inter (sans-serif), Source
/// Serif 4 (serif), IBM Plex Mono (monospace) and Noto Sans JP (Japanese).
#[function]
fn render_html(input: RenderInput) -> Result<Rendered, ToolError> {
    let fetch: net::Fetch = Arc::new(|url: &str| {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = format!("/tmp/.subako-html-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        let mut request = http::Request::get(url);
        request.output = http::ResponseOutput::File { path: path.clone() };
        let response = http::request(&request).map_err(|e| format!("{:?}: {}", e.kind, e.message));
        let body = std::fs::read(&path);
        let _ = std::fs::remove_file(&path);
        let response = response?;
        if !(200..300).contains(&response.status) {
            return Err(format!("HTTP status {}", response.status));
        }
        body.map_err(|e| format!("cannot read the response: {e}"))
    });
    render(input, Path::new("/package/fonts"), fetch)
}

package! {
    name: "html",
    description: "Draws HTML pages to PNG with the Blitz engine, to check their layout.",
    functions: [render_html],
}

fn render(input: RenderInput, fonts_dir: &Path, fetch: net::Fetch) -> Result<Rendered, ToolError> {
    if !(1..=8192).contains(&input.width) || !(1..=8192).contains(&input.height) {
        return Err(ToolError::new("width and height must be between 1 and 8192"));
    }
    if !(input.scale.is_finite() && (0.1..=4.0).contains(&input.scale)) {
        return Err(ToolError::new("scale must be between 0.1 and 4"));
    }
    let output = absolute(&input.output);
    let (html, base) = match &input.source {
        Source::Html { html } => (html.clone(), PathBuf::from("/tmp/index.html")),
        Source::File { path } => {
            let path = absolute(path);
            if path == output {
                return Err(ToolError::new("output is the HTML file itself"));
            }
            let html = std::fs::read(&path)
                .map_err(|e| ToolError::new(format!("cannot read {}: {e}", path.display())))?;
            (String::from_utf8_lossy(&html).into_owned(), path)
        }
    };
    let base = Url::from_file_path(&base)
        .map_err(|()| ToolError::new(format!("bad path {}", base.display())))?;

    let fonts = fonts::load(fonts_dir).map_err(ToolError::new)?;
    // Only the first call in a process sets it, and a process is one call.
    blitz_dom::util::set_svg_font_db(fonts.svg);
    let loader = Arc::new(net::Loader::new(fetch));
    let scale = input.scale;
    let scheme = match input.color_scheme {
        Scheme::Light => ColorScheme::Light,
        Scheme::Dark => ColorScheme::Dark,
    };
    let device = |css: u32| (css as f32 * scale).round() as u32;
    let mut document = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            base_url: Some(base.to_string()),
            net_provider: Some(Arc::clone(&loader) as _),
            viewport: Some(Viewport::new(device(input.width), device(input.height), scale, scheme)),
            font_ctx: Some(fonts.page),
            // WASI has no threads.
            style_threading: StyleThreading::Sequential,
            ..Default::default()
        },
    );
    // Loading a stylesheet can ask for more: imports, images, fonts.
    for _ in 0..16 {
        document.as_mut().resolve(0.0);
        if !loader.load_queued() {
            break;
        }
    }
    document.as_mut().resolve(0.0);

    let doc: &BaseDocument = document.as_ref();
    let root = doc.root_element();
    let page_width = root.scroll_width().max(input.width as f32);
    let page_height = root.scroll_height().max(input.height as f32);
    let mut notes = Vec::new();
    let scripts = doc.query_selector_all("script").map(|found| found.len()).unwrap_or(0);
    if scripts > 0 {
        notes.push(format!(
            "{scripts} <script> element(s) didn't run: this engine runs no JavaScript, so \
             anything they would add or change is missing"
        ));
    }
    let mut height = if input.full_page { page_height } else { input.height as f32 };
    if height > MAX_PAGE_HEIGHT {
        notes.push(format!("the page is {page_height:.0}px tall; the image stops at {MAX_PAGE_HEIGHT:.0}px"));
        height = MAX_PAGE_HEIGHT;
    }
    let width = device(input.width);
    let mut pixel_height = (height * scale).round().max(1.0) as u32;
    if u64::from(width) * u64::from(pixel_height) > MAX_PIXELS {
        pixel_height = (MAX_PIXELS / u64::from(width)) as u32;
        notes.push(format!(
            "the image is cut at {pixel_height}px high, to stay under {MAX_PIXELS} pixels; \
             a smaller scale shows more"
        ));
    }
    let overflowing = overflowing(doc, input.width as f32);

    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| {
            // The canvas a browser shows where the page paints nothing.
            scene.fill(
                Fill::NonZero,
                Default::default(),
                Color::WHITE,
                Default::default(),
                &Rect::new(0.0, 0.0, width as f64, pixel_height as f64),
            );
            blitz_paint::paint_scene(scene, document.as_mut(), scale as f64, width, pixel_height, 0, 0);
        },
        width,
        pixel_height,
    );
    write_png(&output, &buffer, width, pixel_height, scale)?;

    Ok(Rendered {
        output: output.display().to_string(),
        image: Size { width, height: pixel_height },
        page: Size { width: page_width.ceil() as u32, height: page_height.ceil() as u32 },
        overflowing,
        not_loaded: loader.failed(),
        notes,
    })
}

/// The outermost elements whose right edge is past `viewport` CSS pixels,
/// skipping what scrolls or is clipped inside its own box.
fn overflowing(doc: &BaseDocument, viewport: f32) -> Vec<String> {
    use blitz_dom::Node;
    use style::values::computed::Overflow;

    fn visit(node: &Node, viewport: f32, found: &mut Vec<String>) {
        for &child in &node.children {
            let child = node.with(child);
            if found.len() >= MAX_OVERFLOWING {
                return;
            }
            if !child.is_element() {
                continue;
            }
            let layout = child.final_layout();
            let right = child.absolute_position(0.0, 0.0).x + layout.size.width;
            // Half a pixel of slack for rounding.
            if layout.size.width > 0.0 && right > viewport + 0.5 {
                found.push(format!("{} (right edge at {right:.0}px)", describe(child)));
                continue;
            }
            let clips = child
                .primary_styles()
                .is_some_and(|style| style.get_box().overflow_x != Overflow::Visible);
            if !clips {
                visit(child, viewport, found);
            }
        }
    }

    let mut found = Vec::new();
    visit(doc.root_element(), viewport, &mut found);
    found
}

/// `tag#id.class1.class2`, as a selector would name it.
fn describe(node: &blitz_dom::Node) -> String {
    let Some(element) = node.element_data() else { return String::new() };
    let mut name = element.name.local.to_string();
    if let Some(id) = node.attr(blitz_dom::local_name!("id")) {
        name.push('#');
        name.push_str(id);
    }
    if let Some(classes) = node.attr(blitz_dom::local_name!("class")) {
        for class in classes.split_whitespace() {
            name.push('.');
            name.push_str(class);
        }
    }
    name
}

fn write_png(path: &Path, rgba: &[u8], width: u32, height: u32, scale: f32) -> Result<(), ToolError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| ToolError::new(format!("cannot create {}: {e}", dir.display())))?;
    }
    let file = std::fs::File::create(path)
        .map_err(|e| ToolError::new(format!("cannot write {}: {e}", path.display())))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    // So viewers show a 2x image at its CSS size: 96 dpi per CSS pixel.
    let per_meter = (96.0 * f64::from(scale) / 0.0254).round() as u32;
    encoder.set_pixel_dims(Some(png::PixelDimensions {
        xppu: per_meter,
        yppu: per_meter,
        unit: png::Unit::Meter,
    }));
    let fail = |e: png::EncodingError| ToolError::new(format!("cannot write {}: {e}", path.display()));
    let mut writer = encoder.write_header().map_err(fail)?;
    writer.write_image_data(rgba).map_err(fail)?;
    writer.finish().map_err(fail)
}

/// `path`, from `/tmp` if relative.
fn absolute(path: &str) -> PathBuf {
    Path::new("/tmp").join(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The repository's directories; in wasmtime, mounted where the runner
    /// in .cargo/config.toml puts them.
    fn dir(native: &str, wasm: &str) -> PathBuf {
        if cfg!(target_os = "wasi") {
            PathBuf::from(wasm)
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(native)
        }
    }

    fn page(name: &str) -> String {
        dir("tests/pages", "/tests/pages").join(name).display().to_string()
    }

    fn out(name: &str) -> String {
        // WASI has no `temp_dir`; the runner maps target/ to /tmp.
        let path = dir("target/test-output", "/tmp/test-output").join(name);
        let _ = std::fs::remove_file(&path);
        path.display().to_string()
    }

    fn input(source: Source, output: String) -> RenderInput {
        RenderInput {
            source,
            output,
            width: default_width(),
            height: default_height(),
            scale: default_scale(),
            full_page: default_full_page(),
            color_scheme: Scheme::Light,
        }
    }

    fn no_network() -> net::Fetch {
        Arc::new(|url: &str| panic!("unexpected request for {url}"))
    }

    fn run(input: RenderInput) -> Rendered {
        render(input, &dir("assets/fonts", "/package/fonts"), no_network()).unwrap()
    }

    /// The PNG's pixels as RGBA, with its width and height.
    fn pixels(path: &str) -> (Vec<u8>, u32, u32) {
        let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buffer).unwrap();
        buffer.truncate(info.buffer_size());
        (buffer, info.width, info.height)
    }

    fn dark_pixels(path: &str) -> usize {
        pixels(path).0.chunks(4).filter(|p| p[0] < 128 && p[1] < 128 && p[2] < 128).count()
    }

    #[test]
    fn draws_a_file_with_its_stylesheet_and_images() {
        let output = out("report.png");
        let rendered = run(input(Source::File { path: page("report.html") }, output.clone()));
        assert_eq!(rendered.output, output);
        assert_eq!(rendered.image.width, 1280);
        assert!(rendered.page.height > 300, "{rendered:?}");
        assert_eq!(rendered.image.height, rendered.page.height.max(800));
        let (_, width, height) = pixels(&output);
        assert_eq!((width, height), (rendered.image.width, rendered.image.height));
        // The stylesheet loaded: the header's gradient is dark.
        assert!(dark_pixels(&output) > 10_000);
        assert!(rendered.overflowing.is_empty(), "{:?}", rendered.overflowing);
        assert_eq!(rendered.not_loaded.len(), 1, "{:?}", rendered.not_loaded);
        assert!(rendered.not_loaded[0].url.ends_with("/tests/pages/missing.png"));
        assert!(rendered.notes[0].starts_with("1 <script> element(s) didn't run"), "{:?}", rendered.notes);
    }

    #[test]
    fn names_what_sticks_out_on_a_phone() {
        let mut phone = input(Source::File { path: page("report.html") }, out("phone.png"));
        phone.width = 390;
        let rendered = run(phone);
        assert!(rendered.page.width > 390, "{rendered:?}");
        assert!(
            rendered.overflowing.iter().any(|e| e.starts_with("section.panel (right edge at")),
            "{:?}",
            rendered.overflowing
        );
        // The table scrolls inside its wrapper, so it isn't named itself.
        assert!(!rendered.overflowing.iter().any(|e| e.starts_with("table")));
    }

    #[test]
    fn draws_text_in_svg_with_the_bundled_fonts() {
        let html = format!("<body style=margin:0><img src='{}'></body>", page("text-only.svg"));
        let output = out("svg-text.png");
        run(input(Source::Html { html }, output.clone()));
        assert!(dark_pixels(&output) > 200, "the SVG's text is missing");
    }

    #[test]
    fn loads_https_through_the_worker_and_refuses_http() {
        let svg = std::fs::read(page("text-only.svg")).unwrap();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let fetch: net::Fetch = {
            let asked = Arc::clone(&asked);
            Arc::new(move |url: &str| {
                asked.lock().unwrap().push(url.to_string());
                if url.ends_with("logo.svg") { Ok(svg.clone()) } else { Err("host_not_allowed".into()) }
            })
        };
        let html = "<img src='https://example.com/logo.svg'><img src='https://blocked.example/a.png'>\
                    <img src='http://example.com/b.png'>";
        let output = out("https.png");
        let rendered = render(
            input(Source::Html { html: html.to_string() }, output.clone()),
            &dir("assets/fonts", "/package/fonts"),
            fetch,
        )
        .unwrap();
        assert_eq!(*asked.lock().unwrap(), ["https://example.com/logo.svg", "https://blocked.example/a.png"]);
        assert_eq!(
            rendered.not_loaded,
            [
                NotLoaded { url: "https://blocked.example/a.png".into(), reason: "host_not_allowed".into() },
                NotLoaded { url: "http://example.com/b.png".into(), reason: "only https URLs can be loaded".into() },
            ]
        );
        assert!(dark_pixels(&output) > 200);
    }

    #[test]
    fn loads_data_urls() {
        // A 1x1 black GIF, stretched.
        let html = "<img style='width:100px;height:100px;display:block' \
                    src='data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAAAAAAALAAAAAABAAEAAAIBRAA7'>";
        let output = out("data.png");
        let rendered = run(input(Source::Html { html: html.to_string() }, output.clone()));
        assert!(rendered.not_loaded.is_empty(), "{:?}", rendered.not_loaded);
        assert!(dark_pixels(&output) >= 9_000);
    }

    #[test]
    fn captures_the_viewport_at_a_scale() {
        let mut viewport = input(Source::File { path: page("report.html") }, out("viewport.png"));
        viewport.full_page = false;
        viewport.width = 600;
        viewport.height = 300;
        viewport.scale = 2.0;
        let rendered = run(viewport);
        assert_eq!(rendered.image, Size { width: 1200, height: 600 });
        assert_eq!(rendered.page.width, 600);
    }

    #[test]
    fn rejects_bad_input() {
        let error = |input| render(input, &dir("assets/fonts", "/package/fonts"), no_network()).err().unwrap();
        let mut wide = input(Source::Html { html: String::new() }, out("x.png"));
        wide.width = 100_000;
        assert!(error(wide).message().contains("width"));
        let same = input(Source::File { path: page("report.html") }, page("report.html"));
        assert!(error(same).message().contains("HTML file itself"));
        let missing = input(Source::File { path: page("nope.html") }, out("x.png"));
        assert!(error(missing).message().contains("cannot read"));
    }
}
