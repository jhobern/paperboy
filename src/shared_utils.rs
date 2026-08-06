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
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub(crate) fn compact_long_strings(text: &str) -> String {
    compact_long_strings_mapped(text).0
}

/// Like [`compact_long_strings`], but also returns a per-line column map that
/// translates a position in the *compacted* text back to the corresponding
/// column in the *full* text — the machinery behind "select a compacted string
/// and copy the untruncated value" (see `TuiApp::resp_full_selected_parts`).
///
/// The returned `Vec<Vec<usize>>` has one inner vector per line (compaction
/// never adds or removes newlines, so compacted and full line counts match).
/// For line `L`, `maps[L][col]` is the full-text column that compacted column
/// `col` maps to, for every `col` in `0..=compacted_line_len` (the extra
/// trailing entry maps the just-past-the-end position to the full line's
/// length, so an exclusive selection end still translates). Columns that fall
/// inside an inserted `...` ellipsis map to the start of the elided run, which
/// keeps the map monotonic and makes a selection spanning a whole compacted
/// literal expand to that literal's full text.
pub(crate) fn compact_long_strings_mapped(text: &str) -> (String, Vec<Vec<usize>>) {
    // A literal is only worth compacting when the ellipsis actually saves room.
    let threshold = COMPACT_HEAD + COMPACT_TAIL + COMPACT_ELLIPSIS.chars().count();
    let ellipsis_len = COMPACT_ELLIPSIS.chars().count();
    let mut out = String::with_capacity(text.len());
    let mut maps: Vec<Vec<usize>> = Vec::new();
    // `cur` is the current line's compacted-col -> full-col map; `full_col` is
    // the full-text column of the next full-text char to be consumed on this
    // line. `push` a mapping entry for every compacted char we emit.
    let mut cur: Vec<usize> = Vec::new();
    let mut full_col: usize = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            // Newlines separate logical lines and aren't part of a line's
            // selectable content: close the current line, recording the
            // past-the-end sentinel, and start a fresh one.
            cur.push(full_col);
            maps.push(std::mem::take(&mut cur));
            out.push('\n');
            full_col = 0;
            continue;
        }
        if c != '"' {
            cur.push(full_col);
            out.push(c);
            full_col += 1;
            continue;
        }
        // Opening quote: gather the literal's content up to the closing
        // unescaped quote, then decide whether to shorten it.
        cur.push(full_col);
        out.push('"');
        full_col += 1;
        let content_start = full_col;
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
            for k in 0..COMPACT_HEAD {
                cur.push(content_start + k);
            }
            out.extend(&content[..COMPACT_HEAD]);
            for _ in 0..ellipsis_len {
                // The ellipsis stands in for the elided run: map it to that
                // run's start so any selection touching it expands outward.
                cur.push(content_start + COMPACT_HEAD);
            }
            out.push_str(COMPACT_ELLIPSIS);
            let tail_start = content.len() - COMPACT_TAIL;
            for k in 0..COMPACT_TAIL {
                cur.push(content_start + tail_start + k);
            }
            out.extend(&content[tail_start..]);
        } else {
            for k in 0..content.len() {
                cur.push(content_start + k);
            }
            out.extend(&content);
        }
        full_col = content_start + content.len();
        if closed {
            cur.push(full_col);
            out.push('"');
            full_col += 1;
        }
    }
    cur.push(full_col);
    maps.push(cur);
    (out, maps)
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

    #[test]
    fn the_map_expands_a_selected_compacted_literal_to_its_full_text() {
        let full = "{\n  \"k\": \"0123456789abcdef\"\n}";
        let (compact, maps) = super::compact_long_strings_mapped(full);
        // The compacted text is exactly what the plain compactor produces.
        assert_eq!(compact, compact_long_strings(full));
        // One map per line, matching the compacted body's line count.
        let comp_lines: Vec<&str> = compact.split('\n').collect();
        assert_eq!(maps.len(), comp_lines.len());
        // Line 1 is `  "k": "0123...cdef"`; selecting from the value literal's
        // opening quote to the end of the line must translate to the full,
        // untruncated literal on the full body's matching line.
        let line = 1;
        let comp_line = comp_lines[line];
        let full_line: Vec<char> = full.split('\n').nth(line).unwrap().chars().collect();
        let open = comp_line.find("\"0123").unwrap(); // ASCII: byte idx == char idx
        let close = comp_line.chars().count(); // just past the closing quote
        let (full_open, full_close) = (maps[line][open], maps[line][close]);
        let extracted: String = full_line[full_open..full_close].iter().collect();
        assert_eq!(extracted, "\"0123456789abcdef\"");
    }

    #[test]
    fn per_line_maps_are_monotonic_and_sentinel_terminated() {
        let full = "{\n  \"k\": \"0123456789abcdef\",\n  \"n\": 12\n}";
        let (compact, maps) = super::compact_long_strings_mapped(full);
        let comp_lines: Vec<&str> = compact.split('\n').collect();
        let full_lines: Vec<&str> = full.split('\n').collect();
        assert_eq!(maps.len(), comp_lines.len());
        for (i, line_map) in maps.iter().enumerate() {
            // One entry per compacted column, plus the past-the-end sentinel.
            assert_eq!(line_map.len(), comp_lines[i].chars().count() + 1);
            // Never decreasing, and the sentinel is the full line's length.
            assert!(line_map.windows(2).all(|w| w[0] <= w[1]));
            assert_eq!(*line_map.last().unwrap(), full_lines[i].chars().count());
        }
    }
}
