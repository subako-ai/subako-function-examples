//! SVG to PNG with resvg, for charts: Python has the SVG chart libraries, but
//! the raster ones need native code this build doesn't have.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use resvg::{tiny_skia, usvg};

/// The largest PNG this draws, in pixels, so a huge `scale` fails cleanly
/// instead of running out of memory.
const MAX_PIXELS: u64 = 50_000_000;

/// Draws `svg` at `scale` times its own size, writes the PNG to `path` and
/// returns its width and height. Text uses the fonts in `fonts`.
pub fn to_png(svg: &str, path: &Path, scale: f32, fonts: &Path) -> Result<(u32, u32), String> {
    if !(scale.is_finite() && scale > 0.0) {
        return Err(format!("scale must be a positive number, not {scale}"));
    }
    let options = usvg::Options { fontdb: font_database(fonts), ..usvg::Options::default() };
    let tree = usvg::Tree::from_str(svg, &options).map_err(|e| format!("invalid SVG: {e}"))?;
    let size = tree.size().to_int_size().scale_by(scale).ok_or("the image is empty")?;
    if u64::from(size.width()) * u64::from(size.height()) > MAX_PIXELS {
        return Err(format!(
            "{} x {} pixels is too large; use a smaller scale",
            size.width(),
            size.height()
        ));
    }
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height()).ok_or("the image is empty")?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let png = pixmap.encode_png().map_err(|e| format!("cannot encode the PNG: {e}"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, png).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok((size.width(), size.height()))
}

/// The fonts in `dir`, read once per process. CSS generic families map to
/// them, and so does every family they don't have: usvg falls back to
/// `serif`, which is Inter here. Characters a font lacks, such as Japanese,
/// come from any font that has them.
fn font_database(dir: &Path) -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                if let Ok(data) = std::fs::read(entry.path()) {
                    db.load_font_data(data);
                }
            }
            db.set_sans_serif_family("Inter");
            db.set_serif_family("Inter");
            db.set_cursive_family("Inter");
            db.set_fantasy_family("Inter");
            db.set_monospace_family("IBM Plex Mono");
            Arc::new(db)
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fonts() -> &'static Path {
        Path::new(if cfg!(target_os = "wasi") { "/package/fonts" } else { "assets/fonts" })
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        // The wasm runner maps target/ to /tmp.
        let dir = if cfg!(target_os = "wasi") { "/tmp" } else { "target" };
        Path::new(dir).join(format!("svg-test-{name}"))
    }

    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">
        <style>.label { font-family: Consolas, monospace; font-size: 20px }</style>
        <rect width="200" height="100" fill="#16221F"/>
        <text class="label" x="10" y="60" fill="#FFFFF6">Sales 売上</text>
    </svg>"##;

    #[test]
    fn draws_a_png_at_the_scale_asked() {
        let path = scratch("scaled/chart.png");
        let _ = std::fs::remove_file(&path);
        assert_eq!(to_png(SVG, &path, 2.0, fonts()).unwrap(), (400, 200));
        let png = std::fs::read(&path).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
    }

    #[test]
    fn draws_text_with_the_bundled_fonts() {
        // The same picture without the text, to tell whether any was drawn.
        let blank = SVG.replace("Sales 売上", "");
        let with_text = scratch("text.png");
        let without = scratch("blank.png");
        to_png(SVG, &with_text, 1.0, fonts()).unwrap();
        to_png(&blank, &without, 1.0, fonts()).unwrap();
        assert_ne!(std::fs::read(with_text).unwrap(), std::fs::read(without).unwrap());
        let db = font_database(fonts());
        assert!(db.faces().any(|f| f.families.iter().any(|(name, _)| name == "IBM Plex Mono")));
    }

    #[test]
    fn rejects_bad_input() {
        let path = scratch("bad.png");
        assert!(to_png("<svg", &path, 1.0, fonts()).unwrap_err().starts_with("invalid SVG"));
        assert!(to_png(SVG, &path, 0.0, fonts()).is_err());
        assert!(to_png(SVG, &path, 1000.0, fonts()).unwrap_err().contains("too large"));
    }
}
