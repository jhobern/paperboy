//! Small cross-cutting helpers that have no single obvious home module.

use std::path::Path;

/// An error's message with the C errno stripped, ready to show a person.
///
/// `std::io::Error`'s own text ends in the raw errno — "No such file or
/// directory (os error 2)" — which tells a user nothing and reads like a
/// crash. The reason before it is the part worth showing. Anything that
/// doesn't end that way is passed through unchanged, so this is safe to apply
/// to any error on its way to the status line.
///
/// The suffix has to be matched *anchored at the end*, digits and all:
/// checking only that the text contains `" (os error "` somewhere and happens
/// to end in `)` truncated a composed message like
/// `load failed (os error 2) for config(prod)` down to `load failed`, throwing
/// away the part that said which file.
pub(crate) fn friendly_error(e: &impl std::fmt::Display) -> String {
    let text = e.to_string();
    let stripped = text
        .strip_suffix(')')
        .and_then(|head| head.rfind(" (os error ").map(|i| (head, i)))
        .filter(|(head, i)| {
            let digits = &head[i + " (os error ".len()..];
            !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
        })
        .map(|(head, i)| head[..i].to_string());
    stripped.unwrap_or(text)
}

/// The file stem of `path` (its name without an extension), or `fallback` when
/// the path has no usable stem. Accepts anything path-like (`&str`, `&Path`, …).
pub(crate) fn stem(path: impl AsRef<Path>, fallback: &str) -> String {
    path.as_ref()
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback.to_string())
}

/// Turn a display name into a safe single-segment file stem (path separators and
/// other awkward characters → `_`), so a scratch report's name can't escape the
/// target directory when it is exported. Shared by the report writers and the
/// headless report CLI so an exported file lands in the same place either way.
pub(crate) fn sanitize_file_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "report".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Cut `text` to `width` display columns, marking the cut with an ellipsis.
/// Counts `char`s rather than bytes so it can't split a multi-byte sequence.
/// Shared by both front-ends' result grids, which cap cell width identically.
pub(crate) fn truncate_to_width(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// How many leading / trailing characters a compacted string literal keeps.
const COMPACT_HEAD: usize = 4;
const COMPACT_TAIL: usize = 4;
const COMPACT_ELLIPSIS: &str = "...";

/// Produce a "compact overview" of a response body by shortening long
/// double-quoted string *values* to a `"head...tail"` form — e.g.
/// `"anehusenhugroegureol…"` becomes `"aneh...ureol"` — while leaving structure,
/// numbers, short strings (enums, …) and object *keys* untouched.
///
/// Keys are never shortened however long they are: a compacted value is still
/// recognisable from the key that names it, but a compacted *key* leaves a body
/// that can't be read at all, which defeats the point of an overview. Used by the
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
    // Indexed rather than iterated: deciding whether a literal is a key needs
    // to look past its closing quote (and any spaces) for a `:`, which is more
    // lookahead than a `Peekable` gives.
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    let mut out = String::with_capacity(text.len());
    let mut maps: Vec<Vec<usize>> = Vec::new();
    // `cur` is the current line's compacted-col -> full-col map; `full_col` is
    // the full-text column of the next full-text char to be consumed on this
    // line. `push` a mapping entry for every compacted char we emit.
    let mut cur: Vec<usize> = Vec::new();
    let mut full_col: usize = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
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
        while i < chars.len() {
            let nc = chars[i];
            i += 1;
            if nc == '\\' {
                // A backslash escapes the next char; keep both verbatim so the
                // escape stays intact (and a `\"` doesn't close the literal).
                content.push('\\');
                if i < chars.len() {
                    content.push(chars[i]);
                    i += 1;
                }
                continue;
            }
            if nc == '"' {
                closed = true;
                break;
            }
            content.push(nc);
        }
        if content.len() > threshold && !(closed && is_object_key(&chars, i)) {
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

/// Whether the literal that ends at `after` (the index just past its closing
/// quote) is an object *key* — i.e. the next non-blank character is a `:`.
///
/// Only spaces and tabs are skipped, not newlines: a key and its colon are
/// always on one line in every serialiser worth worrying about, whereas a lone
/// `:` starting the next line is far more likely to be ordinary text in a
/// multi-line body than a delayed key separator.
fn is_object_key(chars: &[char], after: usize) -> bool {
    chars[after..].iter().find(|c| **c != ' ' && **c != '\t') == Some(&':')
}

/// The command that hands `path` to whatever the desktop thinks should open it.
///
/// Split out from [`open_in_desktop`] so the choice can be tested without
/// actually launching a browser: the test asserts the platform's opener and
/// that the path is passed as one argument (never interpolated into a shell
/// string, which would break on a space and would be an injection hole for a
/// filename the user didn't type).
fn desktop_open_command(path: &Path) -> (&'static str, Vec<String>) {
    let arg = path.to_string_lossy().into_owned();
    if cfg!(target_os = "macos") {
        ("open", vec![arg])
    } else if cfg!(target_os = "windows") {
        // `start` is a cmd builtin, not a program, and its first quoted
        // argument is taken as the *window title* — hence the empty one.
        ("cmd", vec!["/C".into(), "start".into(), String::new(), arg])
    } else {
        ("xdg-open", vec![arg])
    }
}

/// Open `path` in the desktop's default application for it (a browser for an
/// exported HTML report, a spreadsheet for an `.xlsx`, …).
///
/// Detached deliberately: the opener usually returns immediately, but some
/// (`xdg-open` delegating to a not-yet-running browser) linger for the life of
/// the app they start, and waiting for that would freeze the UI until the user
/// closed their browser. Stdio is silenced for the same reason a TUI can't
/// afford stray output on its screen.
pub(crate) fn open_in_desktop(path: impl AsRef<Path>) -> Result<(), String> {
    let path = path.as_ref();
    let (program, args) = desktop_open_command(path);
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{program}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::{compact_long_strings, desktop_open_command};

    /// A path with a space in it must survive as a single argument — the whole
    /// point of not building a shell string.
    #[test]
    fn the_desktop_opener_passes_the_path_as_one_argument() {
        let (program, args) =
            desktop_open_command(std::path::Path::new("/tmp/a report/out file.html"));
        assert!(!program.is_empty(), "every platform has an opener to name");
        assert!(
            args.iter().any(|a| a == "/tmp/a report/out file.html"),
            "the path is one whole argument, not split or quoted: {args:?}"
        );
    }

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

#[cfg(test)]
mod key_tests {
    use super::{compact_long_strings, compact_long_strings_mapped};

    /// However long a key is, it stays whole: the compact view is for skimming,
    /// and a body of `"aten...tion": "aneh...rucg"` rows can't be skimmed.
    #[test]
    fn a_long_key_is_never_shortened_however_long_its_value_is() {
        let src =
            "{\n  \"authenticationChallengeIdentifier\": \"anehusenhugroegureolegkregulregu\"\n}";
        let out = compact_long_strings(src);
        assert!(
            out.contains("\"authenticationChallengeIdentifier\""),
            "the key survives intact: {out}"
        );
        assert!(
            out.contains("\"aneh...regu\""),
            "but its value doesn't: {out}"
        );
    }

    /// A key is recognised by the `:` that follows it, whatever the serialiser
    /// put between the two — and a value that merely *contains* a colon is not
    /// a key.
    #[test]
    fn spacing_before_the_colon_does_not_hide_a_key() {
        let long = "abcdefghijklmnopqrstuvwxyz";
        for gap in ["", " ", "   ", "\t"] {
            let src = format!("{{\"{long}\"{gap}: 1}}");
            assert!(
                compact_long_strings(&src).contains(long),
                "a key followed by {gap:?} then `:` is still a key"
            );
        }
        // A colon on the *next* line isn't a key separator in any format we
        // render; treating it as one would leave long values uncompacted.
        let src = format!("[\n  \"{long}\"\n  : 1\n]");
        assert!(compact_long_strings(&src).contains("\"abcd...wxyz\""));
    }

    /// Nothing about the value side changes: a long array element, or a value
    /// with a colon in it, still compacts.
    #[test]
    fn values_still_compact_including_ones_containing_a_colon() {
        let src = "[\"https://example.com/a/very/long/path\"]";
        assert_eq!(compact_long_strings(src), "[\"http...path\"]");
    }

    /// The column map is what makes "copy the untruncated value" work, so an
    /// uncompacted key must map straight through — one entry per character,
    /// each mapping to itself — or a selection over a key would copy the wrong
    /// span of the full text.
    #[test]
    fn an_uncompacted_key_maps_one_to_one_onto_the_full_text() {
        let src = "{\"authenticationChallengeIdentifier\": \"anehusenhugroegureolegkregulregu\"}";
        let (out, maps) = compact_long_strings_mapped(src);
        assert_eq!(maps.len(), 1, "one line in, one line out");
        let line = &maps[0];
        assert_eq!(
            line.len(),
            out.chars().count() + 1,
            "one entry per compacted column, plus the past-the-end sentinel"
        );
        // Everything up to and including the key's closing quote is unchanged,
        // so those columns are identity.
        let key_end = out.find(':').unwrap();
        assert!(line[..=key_end].iter().enumerate().all(|(i, m)| *m == i));
        assert!(
            line.windows(2).all(|w| w[0] <= w[1]),
            "and the map is still monotonic across the compacted value"
        );
    }

    /// Regression: the errno suffix has to be matched at the *end*. Searching
    /// for it anywhere and only checking the text ends in some `)` truncated a
    /// composed message at the earlier paren, throwing away the part that said
    /// which file the failure was about.
    #[test]
    fn only_a_trailing_errno_is_stripped() {
        let strip = |t: &str| super::friendly_error(&t.to_string());
        assert_eq!(
            strip("No such file or directory (os error 2)"),
            "No such file or directory"
        );
        assert_eq!(
            strip("load failed (os error 2) for config(prod)"),
            "load failed (os error 2) for config(prod)"
        );
        assert_eq!(strip("plain failure"), "plain failure");
        assert_eq!(strip("weird (os error two)"), "weird (os error two)");
    }
}
