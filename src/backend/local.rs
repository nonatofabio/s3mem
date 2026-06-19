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

use crate::backend::common::{encode_id, safe_segments, validate_id};
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
    ///
    /// The namespace is split on path separators and any empty / `.` / `..` component is
    /// dropped, so a hostile namespace like `../escape` is contained inside `root` rather
    /// than escaping it (record ids get the same treatment via `validate_id`).
    pub fn bundle_dir(&self) -> PathBuf {
        let mut dir = self.root.clone();
        for part in safe_segments(&self.namespace) {
            dir.push(part);
        }
        dir
    }

    fn memories_dir(&self) -> PathBuf {
        self.bundle_dir().join("memories")
    }

    fn record_path(&self, id: &str) -> PathBuf {
        self.memories_dir().join(format!("{}.md", encode_id(id)))
    }

    /// Read every parseable record in the bundle (sorted by id), pairing each with its
    /// bundle-relative key. Foreign or corrupt `.md` files are skipped with a warning
    /// instead of failing the whole bundle, so the manifest stays reconstructable even
    /// when a human drops a stray note into `memories/`.
    fn read_entries(&self) -> Result<Vec<(String, Record)>> {
        let dir = self.memories_dir();
        let read_dir = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(&dir, e)),
        };

        let mut out = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|e| Error::io(&dir, e))?;
            let path = entry.path();
            if !is_markdown(&path) {
                continue;
            }
            let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
            match Record::parse(&text) {
                Ok(record) => {
                    let file_name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or_default();
                    out.push((format!("memories/{file_name}"), record));
                }
                Err(err) => {
                    eprintln!(
                        "s3mem: skipping unparseable memory file {}: {err}",
                        path.display()
                    );
                }
            }
        }
        out.sort_by(|a, b| a.1.meta.id.cmp(&b.1.meta.id));
        Ok(out)
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
        // Fast path: the canonical filename for this id.
        if let Some(record) = read_record_at(&self.record_path(id))? {
            if record.meta.id == id {
                return Ok(record);
            }
        }
        // Fallback: a file whose on-disk name diverges from its frontmatter id (a rename or
        // hand-edit). `list()`/`manifest()` report records by frontmatter id, so `get` must
        // be able to reach them the same way.
        self.read_entries()?
            .into_iter()
            .find(|(_, record)| record.meta.id == id)
            .map(|(_, record)| record)
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn list(&self) -> Result<Vec<String>> {
        // Ids come from each record's frontmatter, not the (encoded) filename, so they read
        // back exactly as written regardless of the on-disk encoding.
        Ok(self
            .read_entries()?
            .into_iter()
            .map(|(_, record)| record.meta.id)
            .collect())
    }

    fn delete(&self, id: &str) -> Result<()> {
        validate_id(id)?;
        // Resolve to the file that actually holds this id: the canonical path if it matches,
        // otherwise the diverging file found by scanning frontmatter ids.
        let canonical = self.record_path(id);
        let target = if read_record_at(&canonical)?.is_some_and(|r| r.meta.id == id) {
            Some(canonical)
        } else {
            self.read_entries()?
                .into_iter()
                .find(|(_, record)| record.meta.id == id)
                .map(|(key, _)| self.bundle_dir().join(key))
        };

        match target {
            Some(path) => {
                fs::remove_file(&path).map_err(|e| Error::io(&path, e))?;
                self.reindex()
            }
            None => Err(Error::NotFound(id.to_string())),
        }
    }

    fn manifest(&self) -> Result<Manifest> {
        let entries = self
            .read_entries()?
            .into_iter()
            .map(|(key, record)| {
                let mut entry = ManifestEntry::from_record(&record);
                entry.key = key; // the real (encoded) filename, not a guess from the id
                entry
            })
            .collect();
        Ok(Manifest { entries })
    }
}

fn is_markdown(path: &Path) -> bool {
    path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("md")
}

/// Read and parse the record at `path`, or `Ok(None)` if the file doesn't exist.
fn read_record_at(path: &Path) -> Result<Option<Record>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(Record::parse(&text)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(path, e)),
    }
}
