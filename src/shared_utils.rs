//! Small cross-cutting helpers that have no single obvious home module.

use std::path::Path;

/// The file stem of `path` (its name without an extension), or `fallback` when
/// the path has no usable stem. Accepts anything path-like (`&str`, `&Path`, …).
pub(crate) fn stem(path: impl AsRef<Path>, fallback: &str) -> String {
    path.as_ref()
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback.to_string())
}

/// How many leading / trailing characters a compacted string literal keeps.
const COMPACT_HEAD: usize = 4;
const COMPACT_TAIL: usize = 4;
const COMPACT_ELLIPSIS: &str = "...";

/// Produce a "compact overview" of a response body by shortening long
/// double-quoted string *literals* to a `"head...tail"` form — e.g.
/// `"anehusenhugroegureol…"` becomes `"aneh...ureol"` — while leaving structure,
/// numbers and short strings (JSON keys, enums, …) untouched. Used by the
/// Response viewer's "compact view" toggle so a body full of long opaque values
/// (tokens, base64, hashes) can be skimmed at a glance.
///
/// This is display-only: the full body is always what gets copied, so a
/// truncation that happens to split a `\uXXXX` / `\n` escape in the *shown*
/// text is harmless. Scanning is escape-aware so a `\"` inside a string doesn't
/// end it early. Operates on `char`s (never byte offsets) so it can't panic on
/// multi-byte UTF-8.
pub(crate) fn compact_long_strings(text: &str) -> String {
    // A literal is only worth compacting when the ellipsis actually saves room.
    let threshold = COMPACT_HEAD + COMPACT_TAIL + COMPACT_ELLIPSIS.chars().count();
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' {
            out.push(c);
            continue;
        }
        // Opening quote: gather the literal's content up to the closing
        // unescaped quote, then decide whether to shorten it.
        out.push('"');
        let mut content: Vec<char> = Vec::new();
        let mut closed = false;
        while let Some(nc) = chars.next() {
            if nc == '\\' {
                // A backslash escapes the next char; keep both verbatim so the
                // escape stays intact (and a `\"` doesn't close the literal).
                content.push('\\');
                if let Some(esc) = chars.next() {
                    content.push(esc);
                }
                continue;
            }
            if nc == '"' {
                closed = true;
                break;
            }
            content.push(nc);
        }
        if content.len() > threshold {
            out.extend(&content[..COMPACT_HEAD]);
            out.push_str(COMPACT_ELLIPSIS);
            out.extend(&content[content.len() - COMPACT_TAIL..]);
        } else {
            out.extend(&content);
        }
        if closed {
            out.push('"');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::compact_long_strings;

    #[test]
    fn long_values_are_shortened_but_keys_and_short_values_are_not() {
        let src = "{\n  \"Key1\": \"anehusenhugroegureolegkregulregurcgeolrgulrecgulrogeulrcgeolrucg\",\n  \"Key2\": \"short\"\n}";
        let out = compact_long_strings(src);
        assert!(out.contains("\"aneh...rucg\""));
        // Short value and keys are untouched.
        assert!(out.contains("\"Key2\": \"short\""));
        assert!(out.contains("\"Key1\""));
    }

    #[test]
    fn head_and_tail_are_four_chars_each() {
        let src = "\"0123456789abcdef\"";
        assert_eq!(compact_long_strings(src), "\"0123...cdef\"");
    }

    #[test]
    fn a_value_at_the_threshold_is_left_intact() {
        // 11 chars = HEAD(4)+TAIL(4)+ELLIPSIS(3); not longer than the threshold,
        // so shortening would save nothing — leave it verbatim.
        let src = "\"12345678901\"";
        assert_eq!(compact_long_strings(src), src);
    }

    #[test]
    fn escaped_quotes_do_not_end_the_literal_early() {
        let src = "\"aaaa\\\"bbbbbbbbbbbbbbbb\"";
        let out = compact_long_strings(src);
        // Still a single well-formed literal, shortened, with a closing quote.
        assert!(out.starts_with('"') && out.ends_with('"'));
        assert!(out.contains("..."));
        // The middle (including the escaped quote) is truncated away, leaving
        // only the two delimiter quotes.
        assert_eq!(out.matches('"').count(), 2);
    }

    #[test]
    fn non_string_structure_is_preserved() {
        let src = "[1, 2, 3, true, null]";
        assert_eq!(compact_long_strings(src), src);
    }

    #[test]
    fn multibyte_content_does_not_panic_and_counts_by_char() {
        let src = "\"ééééééééééééééééé\"";
        let out = compact_long_strings(src);
        assert!(out.contains("..."));
    }
}
