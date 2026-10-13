//! A real directory (a Volume mount) as a Bashkit filesystem backend, which
//! Bashkit's `PosixFs` wraps with the POSIX checks. Bashkit's own `realfs`
//! feature needs `tokio::fs`, which tokio leaves out on WebAssembly, so this
//! uses `std::fs`. Paths arrive relative to the mount point.

use std::path::{Path, PathBuf};

use bashkit::{DirEntry, FileType, FsBackend, Metadata, async_trait};

pub struct Volume {
    root: PathBuf,
}

impl Volume {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Volume { root: root.into() }
    }

    fn real(&self, path: &Path) -> PathBuf {
        let mut real = self.root.clone();
        for part in path.components() {
            if let std::path::Component::Normal(part) = part {
                real.push(part);
            }
        }
        real
    }
}

fn metadata(meta: std::fs::Metadata) -> Metadata {
    let file_type = if meta.is_dir() {
        FileType::Directory
    } else if meta.is_symlink() {
        FileType::Symlink
    } else {
        FileType::File
    };
    Metadata {
        file_type,
        size: meta.len(),
        mode: if meta.is_dir() { 0o755 } else { 0o644 },
        modified: meta.modified().unwrap_or(std::time::UNIX_EPOCH),
        created: meta.created().unwrap_or(std::time::UNIX_EPOCH),
    }
}

#[async_trait]
impl FsBackend for Volume {
    async fn read(&self, path: &Path) -> bashkit::Result<Vec<u8>> {
        Ok(std::fs::read(self.real(path))?)
    }
    async fn write(&self, path: &Path, content: &[u8]) -> bashkit::Result<()> {
        Ok(std::fs::write(self.real(path), content)?)
    }
    async fn append(&self, path: &Path, content: &[u8]) -> bashkit::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).create(true).open(self.real(path))?;
        Ok(file.write_all(content)?)
    }
    async fn mkdir(&self, path: &Path, recursive: bool) -> bashkit::Result<()> {
        let real = self.real(path);
        if recursive {
            std::fs::create_dir_all(real)?;
        } else {
            std::fs::create_dir(real)?;
        }
        Ok(())
    }
    async fn remove(&self, path: &Path, recursive: bool) -> bashkit::Result<()> {
        let real = self.real(path);
        let meta = std::fs::symlink_metadata(&real)?;
        match (meta.is_dir(), recursive) {
            (true, true) => std::fs::remove_dir_all(real)?,
            (true, false) => std::fs::remove_dir(real)?,
            _ => std::fs::remove_file(real)?,
        }
        Ok(())
    }
    async fn stat(&self, path: &Path) -> bashkit::Result<Metadata> {
        Ok(metadata(std::fs::symlink_metadata(self.real(path))?))
    }
    async fn read_dir(&self, path: &Path) -> bashkit::Result<Vec<DirEntry>> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(self.real(path))? {
            let entry = entry?;
            entries.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                metadata: metadata(std::fs::symlink_metadata(entry.path())?),
            });
        }
        Ok(entries)
    }
    async fn exists(&self, path: &Path) -> bashkit::Result<bool> {
        Ok(std::fs::symlink_metadata(self.real(path)).is_ok())
    }
    async fn rename(&self, from: &Path, to: &Path) -> bashkit::Result<()> {
        Ok(std::fs::rename(self.real(from), self.real(to))?)
    }
    async fn copy(&self, from: &Path, to: &Path) -> bashkit::Result<()> {
        std::fs::copy(self.real(from), self.real(to))?;
        Ok(())
    }
    async fn symlink(&self, _target: &Path, _link: &Path) -> bashkit::Result<()> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "symlinks are not supported on the Volume").into())
    }
    async fn read_link(&self, path: &Path) -> bashkit::Result<PathBuf> {
        Ok(std::fs::read_link(self.real(path))?)
    }
    /// WASI has no permission bits, so `chmod` succeeds and changes nothing.
    async fn chmod(&self, _path: &Path, _mode: u32) -> bashkit::Result<()> {
        Ok(())
    }
}
