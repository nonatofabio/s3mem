use std::path::PathBuf;

/// Errors produced by the format and store layers.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("missing or malformed YAML frontmatter (expected a leading `---` block)")]
    MissingFrontmatter,

    #[error("invalid record id `{0}` (ids must be a single path segment: no `/`, `\\`, or `..`)")]
    InvalidId(String),

    #[error("record `{0}` not found")]
    NotFound(String),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// A remote/backend failure (e.g. an S3 request error), rendered as a message so the
    /// error type stays free of any backend SDK in the default build.
    #[error("backend error: {0}")]
    Backend(String),

    /// An invalid `grep` search pattern (bad regex).
    #[error("invalid search pattern: {0}")]
    Pattern(String),
}

impl Error {
    /// Attach the offending path to an [`std::io::Error`].
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
