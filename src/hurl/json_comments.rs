//! Comments inside a JSON request body.
//!
//! Hurl has no notion of a commented body — `hurl_core` rejects `//` outright —
//! but people arrive from Postman with bodies full of notes about which field
//! means what, and losing them at the door is a poor welcome. PaperBoy keeps
//! the body *as authored*, comments included, and strips them on the way to the
//! wire and on the way into a `.hurl` file, so the file every other Hurl runner
//! sees is ordinary strict JSON.
//!
//! Everything here is a pure function over body text. It knows nothing about
//! entries, files or how the authored text is stored alongside the stripped
//! one; that is [`super::entry`]'s problem.

use std::borrow::Cow;

/// What the scanner found at a given span of body text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Piece {
    /// Ordinary source outside any string, template or comment.
    Text,
    /// A complete string literal, both quotes included. Opaque: a `//` in here
    /// is data (`"url": "https://example.net"` is the case that bites everyone
    /// who reaches for a regular expression instead of a scanner).
    Str,
    /// A `{{ … }}` Hurl template, braces included. Also opaque — whatever is
    /// inside is not JSON and must not be read as code.
    Template,
    /// A `//` line comment or a `/* … */` block comment.
    Comment,
}

/// Split body text into classified spans.
///
/// One left-to-right pass with no backtracking. Unterminated strings and block
/// comments simply run to the end of the input rather than being treated as an
/// error: this runs against whatever the user has typed so far, so half-written
/// text is the normal case and must not panic or mis-classify the remainder.
fn scan(src: &str) -> Vec<(Piece, usize, usize)> {
    let b = src.as_bytes();
    let mut out: Vec<(Piece, usize, usize)> = Vec::new();
    let mut i = 0usize;
    let mut text_start = 0usize;
    // Every span boundary below sits on an ASCII byte, so slicing `src` at
    // these indices can never split a multi-byte character.
    while i < b.len() {
        let two = |j: usize, c: u8| b.get(j) == Some(&c);
        match b[i] {
            b'"' => {
                if i > text_start {
                    out.push((Piece::Text, text_start, i));
                }
                let start = i;
                i += 1;
                while i < b.len() {
                    match b[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                // A trailing backslash can push `i` one past the end.
                i = i.min(b.len());
                out.push((Piece::Str, start, i));
                text_start = i;
            }
            b'{' if two(i + 1, b'{') => {
                if i > text_start {
                    out.push((Piece::Text, text_start, i));
                }
                let start = i;
                i += 2;
                while i < b.len() && !(b[i] == b'}' && two(i + 1, b'}')) {
                    i += 1;
                }
                if i < b.len() {
                    i += 2;
                }
                out.push((Piece::Template, start, i));
                text_start = i;
            }
            b'/' if two(i + 1, b'/') => {
                if i > text_start {
                    out.push((Piece::Text, text_start, i));
                }
                let start = i;
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                out.push((Piece::Comment, start, i));
                text_start = i;
            }
            b'/' if two(i + 1, b'*') => {
                if i > text_start {
                    out.push((Piece::Text, text_start, i));
                }
                let start = i;
                i += 2;
                while i < b.len() && !(b[i] == b'*' && two(i + 1, b'/')) {
                    i += 1;
                }
                if i < b.len() {
                    i += 2;
                }
                out.push((Piece::Comment, start, i));
                text_start = i;
            }
            _ => i += 1,
        }
    }
    if b.len() > text_start {
        out.push((Piece::Text, text_start, b.len()));
    }
    out
}

/// Whether body text carries any JSON comment at all.
///
/// The overwhelming majority of bodies have none, so the cheap test for a
/// slash comes first and spares them the scan entirely.
pub fn has_comments(src: &str) -> bool {
    src.contains('/') && scan(src).iter().any(|(k, _, _)| *k == Piece::Comment)
}

/// Remove JSON comments from body text.
///
/// Comment spans are replaced by the newlines they contained rather than by
/// nothing, so the result has exactly as many lines as the input. That
/// alignment is the whole trick: it lets the second pass tell a line that was
/// *nothing but* a comment (drop it — the user never wanted a blank there) from
/// a line that was already blank (keep it, it is their spacing).
pub fn strip_comments(src: &str) -> String {
    let mut bare = String::with_capacity(src.len());
    for (kind, a, b) in scan(src) {
        if kind == Piece::Comment {
            bare.extend(src[a..b].chars().filter(|c| *c == '\n'));
        } else {
            bare.push_str(&src[a..b]);
        }
    }
    let mut kept: Vec<&str> = Vec::new();
    for (before, after) in src.lines().zip(bare.lines()) {
        if after.trim().is_empty() && !before.trim().is_empty() {
            continue;
        }
        kept.push(after.trim_end());
    }
    let mut out = kept.join("\n");
    if src.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Drop the commas that only separated something that is no longer there.
///
/// Commenting out the last field of an object or array is one of the main
/// reasons to want comments at all, and it leaves behind the comma that used to
/// separate it: `{"a": 1,\n// "b": 2\n}` strips to `{"a": 1,\n}`, which no JSON
/// parser will take. Removing that comma is the difference between the feature
/// working and it writing a file that cannot be read back.
///
/// Only commas in code are candidates — one inside a string or a `{{template}}`
/// is data — and only when the next thing that isn't whitespace closes the
/// collection they belong to.
fn drop_trailing_commas(src: &str) -> String {
    let b = src.as_bytes();
    let mut code = vec![false; b.len()];
    for (kind, a, e) in scan(src) {
        if kind == Piece::Text {
            code[a..e].fill(true);
        }
    }
    let mut out = String::with_capacity(src.len());
    // Only single ASCII commas are ever skipped, so every slice boundary below
    // lands on a character boundary.
    let mut copied = 0usize;
    for i in 0..b.len() {
        if b[i] != b',' || !code[i] {
            continue;
        }
        let mut j = i + 1;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < b.len() && code[j] && matches!(b[j], b'}' | b']') {
            out.push_str(&src[copied..i]);
            copied = i + 1;
        }
    }
    out.push_str(&src[copied..]);
    out
}

/// Body text as it goes on the wire and into a `.hurl` file: the authored text
/// with its JSON comments removed.
///
/// Stripping only applies when the result is actually JSON. A plain-text or
/// GraphQL body containing `//` is data, not commentary, and silently
/// truncating it at the slash would be far worse than not supporting comments
/// there at all — so anything that doesn't parse is handed back untouched.
///
/// Note what the last branch implies: for a body that isn't JSON, the authored
/// and wire texts are equal, so no `# [Body]` block is written and the slashes
/// stay in the body exactly as they were. That is the correct outcome for
/// GraphQL, and it is also the pre-existing behaviour for a body PaperBoy can't
/// make sense of.
pub fn wire_body(src: &str) -> Cow<'_, str> {
    if !has_comments(src) {
        return Cow::Borrowed(src);
    }
    let stripped = strip_comments(src);
    if parses_as_json(&stripped) {
        return Cow::Owned(stripped);
    }
    let tidied = drop_trailing_commas(&stripped);
    if parses_as_json(&tidied) {
        return Cow::Owned(tidied);
    }
    Cow::Borrowed(src)
}

/// Rewrite body text into something `serde_json` can parse.
///
/// A Hurl body is rarely valid JSON on its own: `{"id": {{user_id}}}` has a
/// bare template where a value belongs. Templates *inside* a string need no
/// help — `"https://{{host}}/x"` is already a perfectly good JSON string — so
/// only the unquoted ones are stood in for, and each is encoded with its own
/// text so that two different templates don't collapse into the same value and
/// compare equal.
fn json_shape(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for (kind, a, b) in scan(src) {
        match kind {
            Piece::Comment => {}
            Piece::Template => {
                let stand_in =
                    serde_json::Value::String(format!("\u{0}hurl-template:{}", &src[a..b]));
                out.push_str(&stand_in.to_string());
            }
            _ => out.push_str(&src[a..b]),
        }
    }
    out
}

/// Whether body text is JSON once its templates are stood in for.
fn parses_as_json(src: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(&json_shape(src)).is_ok()
}

/// Whether two bodies say the same thing.
///
/// Used to decide whether an authored, commented body still describes the body
/// actually stored in the file, after something other than PaperBoy has edited
/// it. The comparison is deliberately semantic rather than byte-for-byte: an
/// external reformat, or a re-ordering of keys, changes nothing about what gets
/// sent, and treating it as a divergence would orphan every comment in the
/// request over pure whitespace.
///
/// When either side isn't JSON there is nothing to compare structurally, and no
/// honest claim can be made about whitespace either: a space in XML text
/// content, or an indent in YAML, changes what the request means. So text we
/// can't parse has to match byte for byte, which the equality above already
/// covers — anything else and the file's own body wins.
// Reconciliation itself lands with the `# [Body]` block; this is its other
// half, kept here beside the scanner it shares.
pub fn bodies_equivalent(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (ja, jb) = (json_shape(a), json_shape(b));
    match (
        serde_json::from_str::<serde_json::Value>(&ja),
        serde_json::from_str::<serde_json::Value>(&jb),
    ) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slash_inside_a_string_is_data_not_a_comment() {
        let src = r#"{"url": "https://example.net/a//b"}"#;
        assert!(!has_comments(src));
        assert_eq!(wire_body(src), src);
    }

    #[test]
    fn a_line_that_is_only_a_comment_takes_its_line_with_it() {
        let src = "{\n  // who\n  \"id\": 1\n}";
        assert_eq!(strip_comments(src), "{\n  \"id\": 1\n}");
    }

    #[test]
    fn a_trailing_comment_leaves_the_line_it_sat_on() {
        let src = "{\n  \"id\": 1 // the caller\n}";
        assert_eq!(strip_comments(src), "{\n  \"id\": 1\n}");
    }

    #[test]
    fn blank_lines_the_user_typed_are_their_spacing_and_stay() {
        let src = "{\n  \"a\": 1,\n\n  // note\n  \"b\": 2\n}";
        assert_eq!(strip_comments(src), "{\n  \"a\": 1,\n\n  \"b\": 2\n}");
    }

    #[test]
    fn a_block_comment_spanning_lines_removes_all_of_them() {
        let src = "{\n  /* one\n     two */\n  \"id\": 1\n}";
        assert_eq!(strip_comments(src), "{\n  \"id\": 1\n}");
    }

    #[test]
    fn a_block_comment_inside_a_line_leaves_the_rest_of_it() {
        let src = r#"{"a": /* why */ 1}"#;
        assert_eq!(strip_comments(src), r#"{"a":  1}"#);
    }

    #[test]
    fn templates_survive_stripping_and_still_let_the_body_parse() {
        let src = "{\n  // who\n  \"id\": {{user_id}},\n  \"u\": \"https://{{host}}/x\"\n}";
        let out = wire_body(src);
        assert_eq!(
            out,
            "{\n  \"id\": {{user_id}},\n  \"u\": \"https://{{host}}/x\"\n}"
        );
    }

    #[test]
    fn a_plain_text_body_containing_slashes_is_left_alone() {
        // Stripping would truncate this at the slash, which is much worse than
        // not supporting comments in a body that was never JSON.
        let src = "see http://x for details // and this is not a comment";
        assert_eq!(wire_body(src), src);
        assert_eq!(strip_comments("a // b"), "a");
    }

    #[test]
    fn an_unterminated_string_does_not_swallow_the_rest_as_code() {
        let src = "{\"a\": \"unclosed";
        assert_eq!(wire_body(src), src);
    }

    #[test]
    fn reformatting_and_reordering_keys_is_not_a_divergence() {
        assert!(bodies_equivalent(
            "{\"a\":1,\"b\":2}",
            "{\n  \"b\": 2,\n  \"a\": 1\n}"
        ));
    }

    #[test]
    fn a_changed_value_is_a_divergence() {
        assert!(!bodies_equivalent("{\"a\":1}", "{\"a\":2}"));
    }

    #[test]
    fn two_different_templates_are_not_the_same_value() {
        // Both stand in for "something unparseable goes here"; collapsing them
        // would report a real edit as no change at all.
        assert!(!bodies_equivalent(
            "{\"id\": {{alice}}}",
            "{\"id\": {{bob}}}"
        ));
        assert!(bodies_equivalent(
            "{\"id\": {{alice}}}",
            "{\n  \"id\": {{alice}}\n}"
        ));
    }

    #[test]
    fn comments_do_not_change_what_a_body_says() {
        assert!(bodies_equivalent(
            "{\n  // note\n  \"a\": 1\n}",
            "{\"a\":1}"
        ));
    }

    /// C3: the squash this used to do let a hand-edit to a non-JSON body be
    /// overridden by a stale block, and PaperBoy then sent different bytes than
    /// the file did. Whitespace is meaningful in XML text and YAML, so text we
    /// can't parse has to match exactly.
    #[test]
    fn whitespace_matters_in_a_body_that_is_not_json() {
        assert!(!bodies_equivalent("<a>x  y</a>", "<a>x y</a>"));
        assert!(!bodies_equivalent("hello   world", "hello world"));
        assert!(!bodies_equivalent("hello world", "goodbye world"));
        assert!(bodies_equivalent("<a>x  y</a>", "<a>x  y</a>"));
    }

    /// Commenting out the last field of a collection is one of the main reasons
    /// to want comments, and it strands the comma that separated it. Without
    /// this the body isn't JSON, so it would be written to the file with its
    /// comments still in it — invalid Hurl, which reads back as an empty
    /// collection and takes every other request in the file with it.
    #[test]
    fn commenting_out_a_last_field_does_not_strand_its_comma() {
        assert_eq!(
            wire_body("{\n  \"a\": 1,\n  // \"b\": 2\n}").as_ref(),
            "{\n  \"a\": 1\n}"
        );
        assert_eq!(
            wire_body("{\n  \"a\": 1, // note\n}").as_ref(),
            "{\n  \"a\": 1\n}"
        );
        assert_eq!(wire_body("[\n  1,\n  // 2\n]").as_ref(), "[\n  1\n]");
    }

    /// A comma is only punctuation when it's in code — one inside a string or a
    /// template is somebody's data.
    #[test]
    fn a_comma_inside_a_string_is_not_punctuation() {
        assert_eq!(
            wire_body("{\n  \"a\": \"x,\", // note\n  \"b\": \"y,}\"\n}").as_ref(),
            "{\n  \"a\": \"x,\",\n  \"b\": \"y,}\"\n}"
        );
        // The `}` that closes a template must not look like a closing brace.
        assert_eq!(
            wire_body("{\n  \"a\": [{{x}}, {{y}}] // note\n}").as_ref(),
            "{\n  \"a\": [{{x}}, {{y}}]\n}"
        );
    }

    /// Text that still isn't JSON once the commas are tidied is data, not code,
    /// and is handed back untouched — the slashes in a GraphQL body are part of
    /// the query.
    #[test]
    fn a_body_that_is_not_json_keeps_its_slashes() {
        let src = "query { a } // not a comment, this is text";
        assert_eq!(wire_body(src).as_ref(), src);
        assert!(matches!(wire_body(src), Cow::Borrowed(_)));
    }

    #[test]
    fn a_body_with_no_comments_is_returned_without_copying_it() {
        let src = r#"{"a": 1}"#;
        assert!(matches!(wire_body(src), Cow::Borrowed(_)));
    }
}
