//! Storage backends implementing [`crate::store::Store`].
//!
//! [`local::LocalStore`] is the reference backend. Building it first makes the whole crate
//! testable without AWS and proves the portability invariant by construction; an S3 backend
//! is a later sibling module implementing the same trait.

pub mod local;
