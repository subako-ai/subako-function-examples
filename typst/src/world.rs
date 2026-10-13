//! What the Typst compiler sees: the real filesystem as the project, so
//! `/tmp/...` and `/data/...` mean the Volume, plus a few files held in
//! memory (the generated main file, and Markdown passed as text).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, Source, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};

use crate::packages::Packages;

pub struct SubakoWorld {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    main: FileId,
    memory: HashMap<FileId, (String, Bytes)>,
    sources: Mutex<HashMap<FileId, Source>>,
    packages: Packages,
}

impl SubakoWorld {
    /// `memory` holds files that exist only for this compilation, by id, with
    /// the names diagnostics give them; `main` may be one of them. Fonts are
    /// Typst's own and those in `font_dir`.
    pub fn new(
        main: FileId,
        memory: HashMap<FileId, (String, Bytes)>,
        font_dir: &Path,
        packages: Packages,
    ) -> Self {
        let mut fonts: Vec<Font> = typst_assets::fonts()
            .flat_map(|data| Font::iter(Bytes::new(data)))
            .collect();
        let mut files: Vec<PathBuf> = std::fs::read_dir(font_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == "ttf" || e == "otf"))
            .collect();
        files.sort();
        for file in files {
            if let Ok(data) = std::fs::read(&file) {
                fonts.extend(Font::iter(Bytes::new(data)));
            }
        }
        SubakoWorld {
            library: LazyHash::new(Library::default()),
            book: LazyHash::new(FontBook::from_fonts(&fonts)),
            fonts,
            main,
            memory,
            sources: Mutex::new(HashMap::new()),
            packages,
        }
    }

    /// The real path of a file in the project or in a package.
    fn real_path(&self, id: FileId) -> FileResult<PathBuf> {
        let root = match id.root() {
            VirtualRoot::Project => PathBuf::from("/"),
            VirtualRoot::Package(spec) => self.packages.dir(spec)?,
        };
        id.vpath().realize(&root).map_err(FileError::Realize)
    }

    fn read(&self, id: FileId) -> FileResult<Bytes> {
        if let Some((_, bytes)) = self.memory.get(&id) {
            return Ok(bytes.clone());
        }
        let path = self.real_path(id)?;
        if path.is_dir() {
            return Err(FileError::IsDirectory);
        }
        std::fs::read(&path).map(Bytes::new).map_err(|e| FileError::from_io(e, &path))
    }

    /// How diagnostics name a file: its path, the name given to a file held
    /// in memory, and `@namespace/name:version/path` inside a package.
    pub fn display(&self, id: FileId) -> String {
        if let Some((name, _)) = self.memory.get(&id) {
            return name.clone();
        }
        let path = id.vpath().get_with_slash().to_string();
        match id.root() {
            VirtualRoot::Package(spec) => format!("{spec}{path}"),
            VirtualRoot::Project => path,
        }
    }
}

impl World for SubakoWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        let mut sources = self.sources.lock().map_err(|_| FileError::AccessDenied)?;
        if let Some(source) = sources.get(&id) {
            return Ok(source.clone());
        }
        let bytes = self.read(id)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| FileError::InvalidUtf8)?;
        let source = Source::new(id, text.to_string());
        sources.insert(id, source.clone());
        Ok(source)
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.read(id)
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    fn today(&self, offset: Option<Duration>) -> Option<Datetime> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?;
        // WASI has no time zone, so without an offset the local date is UTC's.
        let offset = offset.map_or(0, |o| o.seconds() as i64);
        let days = (now.as_secs() as i64 + offset).div_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        Datetime::from_ymd(year, month, day)
    }
}

/// The calendar date `days` after 1970-01-01, by Howard Hinnant's algorithm.
fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    (year, month, day)
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates() {
        assert_eq!(super::civil_from_days(0), (1970, 1, 1));
        assert_eq!(super::civil_from_days(20_738), (2026, 10, 12));
        assert_eq!(super::civil_from_days(-1), (1969, 12, 31));
    }
}
