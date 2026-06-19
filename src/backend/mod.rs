//! Storage backends implementing [`crate::store::Store`].
//!
//! [`local::LocalStore`] is the reference backend. Building it first makes the whole crate
//! testable without AWS and proves the portability invariant by construction. [`s3::S3Store`]
//! (behind the `s3` feature) is the sibling that implements the same trait over S3 objects.

pub(crate) mod common;
pub mod local;

#[cfg(feature = "s3")]
pub mod s3;
