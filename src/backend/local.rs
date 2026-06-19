//! Local-filesystem backend.
//!
//! Bundle layout under `<root>/<namespace>/`:
//!
//! ```text
//! memories/<id>.md   one OKF record per file
//! manifest.json      derived frontmatter digest (rebuilt on every write)
//! index.md           derived human/agent navigation entrypoint
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::record::Record;
use crate::store::{Manifest, ManifestEntry, Store};

/// A memory bundle backed by a directory on the local filesystem.
#[derive(Debug, Clone)]
pub struct LocalStore {
    root: PathBuf,
    namespace: String,
}

impl LocalStore {
    /// Open (lazily — directories are created on first write) a bundle at
    /// `<root>/<namespace>/`.
    pub fn new(root: impl Into<PathBuf>, namespace: impl Into<String>) -> Self {
        LocalStore {
            root: root.into(),
            namespace: namespace.into(),
        }
    }

    /// The bundle root, `<root>/<namespace>/`.
    pub fn bundle_dir(&self) -> PathBuf {
        self.root.join(&self.namespace)
    }

    fn memories_dir(&self) -> PathBuf {
        self.bundle_dir().join("memories")
    }

    fn record_path(&self, id: &str) -> PathBuf {
        self.memories_dir().join(format!("{id}.md"))
    }

    /// Rebuild `manifest.json` and `index.md` from the records on disk.
    fn reindex(&self) -> Result<()> {
        let manifest = self.manifest()?;
        let dir = self.bundle_dir();
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;

        let manifest_path = dir.join("manifest.json");
        let json = serde_json::to_string_pretty(&manifest)?;
        fs::write(&manifest_path, json).map_err(|e| Error::io(&manifest_path, e))?;

        let index_path = dir.join("index.md");
        fs::write(&index_path, manifest.to_index_md()).map_err(|e| Error::io(&index_path, e))?;
        Ok(())
    }
}

/// Ids become filenames, so they must be a single, traversal-safe path segment.
fn validate_id(id: &str) -> Result<()> {
    let bad = id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || id.contains(std::path::MAIN_SEPARATOR);
    if bad {
        return Err(Error::InvalidId(id.to_string()));
    }
    Ok(())
}

impl Store for LocalStore {
    fn put(&self, record: &Record) -> Result<()> {
        validate_id(&record.meta.id)?;
        let dir = self.memories_dir();
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;

        let path = self.record_path(&record.meta.id);
        let markdown = record.to_markdown()?;
        fs::write(&path, markdown).map_err(|e| Error::io(&path, e))?;

        self.reindex()
    }

    fn get(&self, id: &str) -> Result<Record> {
        validate_id(id)?;
        let path = self.record_path(id);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(id.to_string()))
            }
            Err(e) => return Err(Error::io(&path, e)),
        };
        Record::parse(&text)
    }

    fn list(&self) -> Result<Vec<String>> {
        let dir = self.memories_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(&dir, e)),
        };

        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| Error::io(&dir, e))?;
            let path = entry.path();
            if is_markdown(&path) {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    ids.push(stem.to_string());
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    fn delete(&self, id: &str) -> Result<()> {
        validate_id(id)?;
        let path = self.record_path(id);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(id.to_string()))
            }
            Err(e) => return Err(Error::io(&path, e)),
        }
        self.reindex()
    }

    fn manifest(&self) -> Result<Manifest> {
        let mut entries = Vec::new();
        for id in self.list()? {
            let record = self.get(&id)?;
            entries.push(ManifestEntry::from_record(&record));
        }
        Ok(Manifest { entries })
    }
}

fn is_markdown(path: &Path) -> bool {
    path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("md")
}
