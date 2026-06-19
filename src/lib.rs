//! # s3mem — OKF memory over S3
//!
//! Portable, vendor-neutral agent memory built on the [Open Knowledge Format][okf]:
//! each memory is a plain markdown file with YAML frontmatter, one concept per file.
//! A *memory bundle* is just a directory (or, later, an S3 prefix) of those files plus
//! a derived `manifest.json` and `index.md`.
//!
//! The core invariant: **a memory is a file; a bundle is a prefix.** Everything above the
//! [`Store`] trait is backend-agnostic, so a bundle round-trips between a local filesystem
//! and S3 unchanged — that portability is the whole point.
//!
//! This crate currently ships the format layer ([`Record`]) and the local-filesystem
//! backend ([`LocalStore`]). The S3 backend is a future implementation of the same
//! [`Store`] trait.
//!
//! ```
//! use s3mem::{LocalStore, MemoryType, Record, RecordMeta, Store, now_iso};
//!
//! let dir = std::env::temp_dir().join("s3mem-doctest");
//! let _ = std::fs::remove_dir_all(&dir);
//! let store = LocalStore::new(&dir, "agent-7");
//!
//! let now = now_iso();
//! let rec = Record::new(
//!     RecordMeta::new("prefers-rust", MemoryType::Semantic, "User leans toward Rust", &now),
//!     "The user prefers Rust unless it's prohibitive. Relates to [[stack-choice]].",
//! );
//! store.put(&rec).unwrap();
//!
//! let got = store.get("prefers-rust").unwrap();
//! assert_eq!(got.meta.description, "User leans toward Rust");
//! assert_eq!(store.manifest().unwrap().entries.len(), 1);
//! # std::fs::remove_dir_all(&dir).ok();
//! ```
//!
//! [okf]: https://github.com/GoogleCloudPlatform/knowledge-catalog/tree/main/okf

pub mod backend;
pub mod error;
pub mod recall;
pub mod record;
pub mod store;
mod util;

pub use backend::local::LocalStore;
#[cfg(feature = "s3")]
pub use backend::s3::S3Store;
pub use error::{Error, Result};
pub use recall::{bm25, grep, Filter, GrepOptions, Hit};
pub use record::{MemoryType, Record, RecordMeta};
pub use store::{Manifest, ManifestEntry, Store};
pub use util::now_iso;
