//! Shared tokenizer for the BM25 path. Lowercase, split on non-alphanumerics (so `deploy-key`
//! and `deploy_key` both yield `deploy` + `key`), drop single-character Latin tokens.
//!
//! CJK scripts aren't space-delimited, so `char::is_alphanumeric` would turn a whole run like
//! `日本語` into one token and BM25 would never match the substring `日本` (which the grep
//! path *does* find). To keep ranked recall from silently missing non-Latin memories, a CJK
//! run is emitted as per-character unigrams **and** overlapping bigrams, so `日本` matches
//! `日本語`. (Stemming / stopwords remain future refinements.)

/// Split `text` into search tokens (see module docs).
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut latin = String::new();
    let mut cjk: Vec<char> = Vec::new();

    for c in text.chars() {
        if is_cjk(c) {
            flush_latin(&mut latin, &mut out);
            cjk.push(c);
        } else if c.is_alphanumeric() {
            flush_cjk(&mut cjk, &mut out);
            latin.extend(c.to_lowercase());
        } else {
            flush_latin(&mut latin, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_latin(&mut latin, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out
}

/// Push the accumulated Latin run if it's at least 2 characters; reset it either way.
fn flush_latin(latin: &mut String, out: &mut Vec<String>) {
    if latin.chars().take(2).count() == 2 {
        out.push(std::mem::take(latin));
    } else {
        latin.clear();
    }
}

/// Push a CJK run as unigrams plus overlapping bigrams, so substrings still match.
fn flush_cjk(cjk: &mut Vec<char>, out: &mut Vec<String>) {
    for &c in cjk.iter() {
        out.push(c.to_string());
    }
    for window in cjk.windows(2) {
        out.push(window.iter().collect());
    }
    cjk.clear();
}

/// CJK ideographs / kana / hangul, where each character is a meaningful unit.
fn is_cjk(c: char) -> bool {
    let u = c as u32;
    (0x3040..=0x30FF).contains(&u)        // Hiragana + Katakana
        || (0x3400..=0x4DBF).contains(&u) // CJK Extension A
        || (0x4E00..=0x9FFF).contains(&u) // CJK Unified Ideographs
        || (0xAC00..=0xD7AF).contains(&u) // Hangul syllables
        || (0xF900..=0xFAFF).contains(&u) // CJK Compatibility Ideographs
        || (0x20000..=0x2FA1F).contains(&u) // CJK Extension B and beyond
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
