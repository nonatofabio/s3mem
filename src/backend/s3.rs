//! S3 backend (`s3` feature).
//!
//! Stores the exact same bundle as [`LocalStore`](super::local::LocalStore), only the
//! "directory" is an S3 key prefix:
//!
//! ```text
//! s3://<bucket>/<prefix>/<namespace>/
//!   memories/<encode_id(id)>.md   one OKF record per object
//!   manifest.json                 derived frontmatter digest (rewritten on every put/delete)
//!   index.md                      derived navigation entrypoint
//! ```
//!
//! The [`Store`] trait is synchronous, so this backend owns a Tokio runtime and bridges each
//! call with `block_on` rather than forcing async on every caller and on the local backend.
//!
//! Like the local backend, indexing reads every object in the bundle on each write. That is
//! O(n) GETs per `put`/`delete` — fine for modest bundles and correct by construction; an
//! incremental manifest update is the obvious future optimization.

use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use tokio::runtime::Runtime;

use crate::backend::common::{encode_id, safe_segments, validate_id};
use crate::error::{Error, Result};
use crate::record::Record;
use crate::store::{Manifest, ManifestEntry, Store};

/// A memory bundle backed by objects under an S3 key prefix.
#[derive(Debug)]
pub struct S3Store {
    runtime: Runtime,
    client: Client,
    bucket: String,
    prefix: String,
    namespace: String,
}

impl S3Store {
    /// Open a bundle at `s3://<bucket>/<namespace>/`, loading credentials/region from the
    /// standard AWS provider chain (env, shared config, IMDS, …).
    pub fn new(bucket: impl Into<String>, namespace: impl Into<String>) -> Result<Self> {
        Self::with_prefix(bucket, String::new(), namespace)
    }

    /// Open a bundle at `s3://<bucket>/<prefix>/<namespace>/`, using the default AWS config.
    pub fn with_prefix(
        bucket: impl Into<String>,
        prefix: impl Into<String>,
        namespace: impl Into<String>,
    ) -> Result<Self> {
        let runtime = build_runtime()?;
        let client = runtime.block_on(async {
            let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
                .load()
                .await;
            Client::new(&config)
        });
        Ok(Self::from_parts(runtime, client, bucket, prefix, namespace))
    }

    /// Build from an explicit [`aws_sdk_s3::Config`] — for custom endpoints (LocalStack /
    /// MinIO), injected credentials, or tests. `Client::from_conf` is lazy, so it is safe to
    /// construct here without an active runtime.
    pub fn from_config(
        config: aws_sdk_s3::Config,
        bucket: impl Into<String>,
        prefix: impl Into<String>,
        namespace: impl Into<String>,
    ) -> Result<Self> {
        let runtime = build_runtime()?;
        let client = Client::from_conf(config);
        Ok(Self::from_parts(runtime, client, bucket, prefix, namespace))
    }

    fn from_parts(
        runtime: Runtime,
        client: Client,
        bucket: impl Into<String>,
        prefix: impl Into<String>,
        namespace: impl Into<String>,
    ) -> Self {
        S3Store {
            runtime,
            client,
            bucket: bucket.into(),
            prefix: prefix.into(),
            namespace: namespace.into(),
        }
    }

    // --- key construction (pure; see unit tests) --------------------------------------

    fn bundle_prefix(&self) -> String {
        build_bundle_prefix(&self.prefix, &self.namespace)
    }

    fn object_key(&self, id: &str) -> String {
        build_object_key(&self.bundle_prefix(), id)
    }

    fn memories_prefix(&self) -> String {
        join_key(&self.bundle_prefix(), "memories/")
    }

    fn meta_key(&self, name: &str) -> String {
        join_key(&self.bundle_prefix(), name)
    }

    /// Strip the bundle prefix from a full object key to get the bundle-relative key
    /// (`memories/<file>.md`) recorded in the manifest.
    fn relative_key(&self, full: &str) -> String {
        let bp = self.bundle_prefix();
        if bp.is_empty() {
            full.to_string()
        } else {
            full.strip_prefix(&format!("{bp}/"))
                .unwrap_or(full)
                .to_string()
        }
    }

    /// Re-attach the bundle prefix to a relative key (inverse of [`relative_key`]).
    fn full_key(&self, relative: &str) -> String {
        join_key(&self.bundle_prefix(), relative)
    }

    // --- async primitives -------------------------------------------------------------

    async fn put_object(&self, key: &str, body: String) -> Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(body.into_bytes()))
            .send()
            .await
            .map_err(|e| Error::Backend(format!("put_object {key}: {e}")))?;
        Ok(())
    }

    /// `Ok(None)` for a missing object, so callers can distinguish absence from failure.
    async fn get_object(&self, key: &str) -> Result<Option<String>> {
        match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(out) => {
                let data = out
                    .body
                    .collect()
                    .await
                    .map_err(|e| Error::Backend(format!("read body {key}: {e}")))?;
                let text = String::from_utf8(data.into_bytes().to_vec())
                    .map_err(|e| Error::Backend(format!("utf8 {key}: {e}")))?;
                Ok(Some(text))
            }
            Err(err) => {
                if err
                    .as_service_error()
                    .map(|e| e.is_no_such_key())
                    .unwrap_or(false)
                {
                    Ok(None)
                } else {
                    Err(Error::Backend(format!("get_object {key}: {err}")))
                }
            }
        }
    }

    async fn delete_object(&self, key: &str) -> Result<()> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| Error::Backend(format!("delete_object {key}: {e}")))?;
        Ok(())
    }

    /// List every object key under `prefix`, following pagination.
    async fn list_keys(&self, prefix: &str) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix);
            if let Some(t) = &token {
                req = req.continuation_token(t);
            }
            let out = req
                .send()
                .await
                .map_err(|e| Error::Backend(format!("list_objects_v2 {prefix}: {e}")))?;
            for obj in out.contents() {
                if let Some(k) = obj.key() {
                    keys.push(k.to_string());
                }
            }
            if out.is_truncated().unwrap_or(false) {
                token = out.next_continuation_token().map(str::to_string);
                if token.is_none() {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(keys)
    }

    // --- bundle operations ------------------------------------------------------------

    /// Read every parseable record in the bundle (sorted by id), paired with its relative
    /// key. Foreign/corrupt objects are skipped with a warning, not propagated — same
    /// poison-resistance as the local backend.
    async fn read_entries(&self) -> Result<Vec<(String, Record)>> {
        let keys = self.list_keys(&self.memories_prefix()).await?;
        let mut out = Vec::new();
        for key in keys {
            if !key.ends_with(".md") {
                continue;
            }
            let Some(text) = self.get_object(&key).await? else {
                continue; // raced delete
            };
            match Record::parse(&text) {
                Ok(record) => out.push((self.relative_key(&key), record)),
                Err(err) => eprintln!("s3mem: skipping unparseable object {key}: {err}"),
            }
        }
        out.sort_by(|a, b| a.1.meta.id.cmp(&b.1.meta.id));
        Ok(out)
    }

    async fn build_manifest(&self) -> Result<Manifest> {
        let entries = self
            .read_entries()
            .await?
            .into_iter()
            .map(|(key, record)| {
                let mut entry = ManifestEntry::from_record(&record);
                entry.key = key;
                entry
            })
            .collect();
        Ok(Manifest { entries })
    }

    async fn reindex(&self) -> Result<()> {
        let manifest = self.build_manifest().await?;
        let json = serde_json::to_string_pretty(&manifest)?;
        self.put_object(&self.meta_key("manifest.json"), json)
            .await?;
        self.put_object(&self.meta_key("index.md"), manifest.to_index_md())
            .await?;
        Ok(())
    }
}

impl Store for S3Store {
    fn put(&self, record: &Record) -> Result<()> {
        validate_id(&record.meta.id)?;
        let key = self.object_key(&record.meta.id);
        let markdown = record.to_markdown()?;
        self.runtime.block_on(async {
            self.put_object(&key, markdown).await?;
            self.reindex().await
        })
    }

    fn get(&self, id: &str) -> Result<Record> {
        validate_id(id)?;
        let key = self.object_key(id);
        self.runtime.block_on(async {
            // Fast path: the canonical object for this id.
            if let Some(text) = self.get_object(&key).await? {
                let record = Record::parse(&text)?;
                if record.meta.id == id {
                    return Ok(record);
                }
            }
            // Fallback: an object keyed differently from its frontmatter id — same
            // divergence handling as the local backend, so semantics match across backends.
            for (_, record) in self.read_entries().await? {
                if record.meta.id == id {
                    return Ok(record);
                }
            }
            Err(Error::NotFound(id.to_string()))
        })
    }

    fn list(&self) -> Result<Vec<String>> {
        self.runtime.block_on(async {
            Ok(self
                .read_entries()
                .await?
                .into_iter()
                .map(|(_, record)| record.meta.id)
                .collect())
        })
    }

    fn delete(&self, id: &str) -> Result<()> {
        validate_id(id)?;
        let canonical = self.object_key(id);
        self.runtime.block_on(async {
            // Resolve to the object that actually holds this id (canonical key if it matches,
            // else the diverging object found by scanning frontmatter ids).
            let canonical_matches = match self.get_object(&canonical).await? {
                Some(text) => Record::parse(&text)
                    .map(|r| r.meta.id == id)
                    .unwrap_or(false),
                None => false,
            };
            let target = if canonical_matches {
                Some(canonical)
            } else {
                let mut found = None;
                for (relative, record) in self.read_entries().await? {
                    if record.meta.id == id {
                        found = Some(self.full_key(&relative));
                        break;
                    }
                }
                found
            };

            match target {
                Some(key) => {
                    self.delete_object(&key).await?;
                    self.reindex().await
                }
                None => Err(Error::NotFound(id.to_string())),
            }
        })
    }

    fn manifest(&self) -> Result<Manifest> {
        self.runtime.block_on(self.build_manifest())
    }

    fn records(&self) -> Result<Vec<Record>> {
        self.runtime.block_on(async {
            Ok(self
                .read_entries()
                .await?
                .into_iter()
                .map(|(_, r)| r)
                .collect())
        })
    }
}

fn build_runtime() -> Result<Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|e| Error::Backend(format!("tokio runtime: {e}")))
}

fn join_key(bundle_prefix: &str, suffix: &str) -> String {
    if bundle_prefix.is_empty() {
        suffix.to_string()
    } else {
        format!("{bundle_prefix}/{suffix}")
    }
}

fn build_bundle_prefix(prefix: &str, namespace: &str) -> String {
    let mut segments = safe_segments(prefix);
    segments.extend(safe_segments(namespace));
    segments.join("/")
}

fn build_object_key(bundle_prefix: &str, id: &str) -> String {
    join_key(bundle_prefix, &format!("memories/{}.md", encode_id(id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pure key-construction tests — no AWS, no network. They pin the parity-critical rules.

    #[test]
    fn namespace_traversal_is_contained() {
        assert_eq!(build_bundle_prefix("", "ns"), "ns");
        assert_eq!(build_bundle_prefix("", "../escape"), "escape");
        assert_eq!(build_bundle_prefix("root", "../escape"), "root/escape");
        assert_eq!(build_bundle_prefix("a/b", "ns"), "a/b/ns");
    }

    #[test]
    fn object_key_encodes_case_like_local() {
        // Same encoding as the local backend, so a bundle is identical across backends.
        assert_eq!(build_object_key("ns", "alpha"), "ns/memories/alpha.md");
        assert_eq!(build_object_key("ns", "Alpha"), "ns/memories/%41lpha.md");
        assert_eq!(build_object_key("", "x"), "memories/x.md");
        // The `.md` extension is appended to the whole id, so a dotted id keeps its dot.
        assert_eq!(
            build_object_key("ns", "notes.md"),
            "ns/memories/notes.md.md"
        );
    }
}
