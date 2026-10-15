//! The bundled fonts, for both of Blitz's text engines: Parley for the page's
//! text, and usvg for text in SVG. WASI has no system fonts, so these are all
//! a page can use besides its own `@font-face` fonts.

use std::path::Path;
use std::sync::Arc;

use parley::FontContext;
use parley::fontique::{
    Blob, Collection, CollectionOptions, FallbackKey, FamilyId, GenericFamily, Script, SourceCache,
};
use usvg::fontdb;

const SANS: &str = "Inter";
const SERIF: &str = "Source Serif 4";
const MONO: &str = "IBM Plex Mono";
const JAPANESE: &str = "Noto Sans JP";

pub struct Fonts {
    pub page: FontContext,
    pub svg: Arc<fontdb::Database>,
}

/// Reads the fonts in `dir` and maps the CSS generic families to them:
/// sans-serif and system-ui to Inter, serif to Source Serif 4, monospace to
/// IBM Plex Mono, with Noto Sans JP after each for Japanese.
pub fn load(dir: &Path) -> Result<Fonts, String> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read the fonts in {}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| matches!(path.extension().and_then(|e| e.to_str()), Some("ttf" | "otf")))
        .collect();
    paths.sort();

    let mut page = FontContext {
        source_cache: SourceCache::new_shared(),
        collection: Collection::new(CollectionOptions { shared: false, system_fonts: false }),
    };
    let mut svg = fontdb::Database::new();
    for path in paths {
        let data = std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        svg.load_font_data(data.clone());
        page.collection.register_fonts(Blob::new(Arc::new(data) as _), None);
    }

    let collection = &mut page.collection;
    let mut family = |name: &str| collection.family_id(name);
    let [sans, serif, mono, japanese] = [SANS, SERIF, MONO, JAPANESE].map(&mut family);
    let list = |first: Option<FamilyId>| first.into_iter().chain(japanese).collect::<Vec<_>>();
    for (generic, families) in [
        (GenericFamily::SansSerif, list(sans)),
        (GenericFamily::SystemUi, list(sans)),
        (GenericFamily::UiSansSerif, list(sans)),
        (GenericFamily::UiRounded, list(sans)),
        (GenericFamily::Cursive, list(sans)),
        (GenericFamily::Fantasy, list(sans)),
        (GenericFamily::Serif, list(serif)),
        (GenericFamily::UiSerif, list(serif)),
        (GenericFamily::Monospace, list(mono)),
        (GenericFamily::UiMonospace, list(mono)),
    ] {
        collection.set_generic_families(generic, families.into_iter());
    }
    // For characters the chosen font lacks.
    for script in [*b"Latn", *b"Grek", *b"Cyrl", *b"Zyyy"] {
        collection.set_fallbacks(FallbackKey::from(Script::from_bytes(script)), list(sans).into_iter());
    }
    for script in [*b"Hira", *b"Kana", *b"Hani", *b"Jpan"] {
        collection.set_fallbacks(FallbackKey::from(Script::from_bytes(script)), japanese.into_iter());
    }

    // usvg falls back to `serif` for families it doesn't have, as browsers
    // fall back to their default font, which is a serif one too.
    svg.set_sans_serif_family(SANS);
    svg.set_serif_family(SERIF);
    svg.set_cursive_family(SANS);
    svg.set_fantasy_family(SANS);
    svg.set_monospace_family(MONO);
    Ok(Fonts { page, svg: Arc::new(svg) })
}
