use time::{format_description::well_known::Rfc3339, OffsetDateTime};

/// Current UTC time as an ISO-8601 / RFC-3339 string, for `created`/`updated` stamps.
pub fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("RFC-3339 formatting of a valid timestamp cannot fail")
}
