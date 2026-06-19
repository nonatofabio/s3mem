//! Backend-agnostic key rules shared by every [`Store`](crate::store::Store) implementation.
//!
//! These MUST be identical across backends: a bundle written by the local backend has to be
//! byte-for-byte readable by the S3 backend (and vice versa), so id validation, filename
//! encoding, and namespace containment all live here rather than in any one backend.

use crate::error::{Error, Result};

/// Hold ids to an allowlist (ASCII alphanumerics plus `-`, `_`, `.`) and never the traversal
/// tokens `.`/`..`. Rejects path separators, control characters, spaces and unicode up front.
pub(crate) fn validate_id(id: &str) -> Result<()> {
    let allowed = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.');
    let bad =
        id.is_empty() || id == "." || id == ".." || id.contains("..") || !id.bytes().all(allowed);
    if bad {
        return Err(Error::InvalidId(id.to_string()));
    }
    Ok(())
}

/// Encode an (already validated) id into a case-insensitive-collision-free filename/key stem.
///
/// Uppercase ASCII letters are percent-escaped with lowercase hex, so every stem is
/// all-lowercase and the encoding is injective. Two ids differing only by case (`Alpha` vs
/// `alpha`) map to distinct stems that don't collapse on a case-insensitive filesystem
/// (macOS APFS, Windows NTFS) — keeping the bundle identical whether it lands on local disk
/// or on (case-sensitive) S3.
pub(crate) fn encode_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for b in id.bytes() {
        if b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02x}"));
        }
    }
    out
}

/// Split a namespace/prefix into traversal-safe path segments: empty, `.` and `..` components
/// are dropped, so a hostile namespace like `../escape` is contained inside the bundle root
/// rather than escaping it. Works for both `/`- and `\`-separated inputs.
pub(crate) fn safe_segments(s: &str) -> Vec<&str> {
    s.split(['/', '\\'])
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .collect()
}

/// FNV-1a 64-bit hash, as lowercase hex. Deterministic and stable across runs (unlike the
/// std hashers), so it's safe to use for the recall-cache fingerprint that is compared between
/// separate process invocations.
pub(crate) fn fnv1a_hex(s: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}
