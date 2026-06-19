//! Shared tokenizer for the BM25 path. Deliberately minimal: lowercase, split on any
//! non-alphanumeric character (so `deploy-key` and `deploy_key` both yield `deploy` + `key`),
//! and drop single-character tokens. No stemming or stopword removal yet — both are obvious
//! future refinements but each adds a dependency or a wordlist we don't need for v1.

/// Split `text` into lowercase alphanumeric tokens of length ≥ 2.
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().take(2).count() == 2)
        .map(str::to_lowercase)
        .collect()
}

/// Truncate a snippet to at most `max` characters on a char boundary, appending `…` if cut.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lowercases_and_drops_short_tokens() {
        assert_eq!(
            tokenize("Deploy-Key for S3!"),
            vec!["deploy", "key", "for", "s3"]
        );
        // single-char tokens dropped ("a"), 2-char kept ("of")
        assert_eq!(tokenize("a list of x"), vec!["list", "of"]);
    }

    #[test]
    fn truncates_on_char_boundary() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello…");
    }
}
