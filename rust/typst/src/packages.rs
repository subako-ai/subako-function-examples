//! Typst packages: the ones bundled in the package's assets, then ones
//! downloaded earlier into a cache on the Volume, then `@preview` packages
//! downloaded from packages.typst.org through the Subako worker.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use typst::diag::PackageError;
use typst::syntax::package::PackageSpec;

/// Downloads `url` to the real file `dest`, returning the HTTP status:
/// through the worker in the function, a fake in tests.
pub type Fetch = Arc<dyn Fn(&str, &Path) -> Result<u16, String> + Send + Sync>;

pub struct Packages {
    /// Packages shipped in the package's assets, as `namespace/name/version`.
    pub bundled: PathBuf,
    /// Where downloaded packages are unpacked, kept across calls.
    pub cache: PathBuf,
    pub fetch: Fetch,
}

impl Packages {
    /// The real directory holding `spec`, downloading it if needed.
    pub fn dir(&self, spec: &PackageSpec) -> Result<PathBuf, PackageError> {
        let subdir = format!("{}/{}/{}", spec.namespace, spec.name, spec.version);
        for root in [&self.bundled, &self.cache] {
            let dir = root.join(&subdir);
            if dir.is_dir() {
                return Ok(dir);
            }
        }
        if spec.namespace != "preview" {
            return Err(PackageError::NotFound(spec.clone()));
        }
        self.download(spec, &self.cache.join(&subdir))?;
        Ok(self.cache.join(subdir))
    }

    fn download(&self, spec: &PackageSpec, dir: &Path) -> Result<(), PackageError> {
        let failed = |e: String| PackageError::NetworkFailed(Some(e.into()));
        let malformed = |e: std::io::Error| PackageError::MalformedArchive(Some(e.to_string().into()));
        std::fs::create_dir_all(&self.cache).map_err(|e| failed(e.to_string()))?;
        let url = format!("https://packages.typst.org/preview/{}-{}.tar.gz", spec.name, spec.version);
        let archive = self.cache.join(format!(".{}-{}.tar.gz", spec.name, spec.version));
        let status = (self.fetch)(&url, &archive);
        let result = match status {
            Ok(200) => {
                // Unpacked beside the cache entry and moved into place, so a
                // half-unpacked package is never mistaken for a whole one.
                let partial = self.cache.join(format!(".{}-{}", spec.name, spec.version));
                let _ = std::fs::remove_dir_all(&partial);
                let unpacked = unpack(&archive, &partial)
                    .and_then(|()| std::fs::create_dir_all(dir.parent().unwrap_or(dir)))
                    .and_then(|()| std::fs::rename(&partial, dir));
                if unpacked.is_err() {
                    let _ = std::fs::remove_dir_all(&partial);
                }
                unpacked.map_err(malformed)
            }
            Ok(404) => Err(PackageError::NotFound(spec.clone())),
            Ok(status) => Err(failed(format!("packages.typst.org answered {status} for {url}"))),
            Err(e) => Err(failed(format!("{e} (the function's network setting has to allow packages.typst.org)"))),
        };
        let _ = std::fs::remove_file(&archive);
        result
    }
}

/// Unpacks a `.tar.gz` into `dir`. `tar`'s own `unpack` checks each entry's
/// destination with `fs::canonicalize`, which WASI does not have, so this
/// accepts only plain relative paths instead, which cannot leave `dir`.
fn unpack(archive: &Path, dir: &Path) -> std::io::Result<()> {
    use std::path::Component;
    let invalid = |message: String| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
    let file = std::fs::File::open(archive)?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    std::fs::create_dir_all(dir)?;
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let plain = path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
        if !plain {
            return Err(invalid(format!("unsafe path in package: {}", path.display())));
        }
        let dest = dir.join(&path);
        match entry.header().entry_type() {
            tar::EntryType::Directory => std::fs::create_dir_all(&dest)?,
            tar::EntryType::Regular => {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::io::copy(&mut entry, &mut std::fs::File::create(&dest)?)?;
            }
            // Packages are plain files; links and the like are skipped.
            _ => {}
        }
    }
    Ok(())
}

