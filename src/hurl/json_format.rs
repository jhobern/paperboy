//! Lay a JSON request body out again without changing what it says.
//!
//! The obvious implementation — parse with `serde_json`, print with its pretty
//! formatter — is wrong four times over for a *Hurl* body:
//!
//! - it throws away the `//` and `/* */` notes PaperBoy goes to some trouble to
//!   keep (see [`super::json_comments`]);
//! - it cannot read a bare `{{ template }}`, and `{"n": {{ COUNT }}}` is a
//!   perfectly ordinary body here while being invalid JSON everywhere else;
//! - it rewrites the author's own data on the way through — `1.50` comes back
//!   `1.5`, `1e3` comes back `1000.0`, and a 20-digit id loses its tail to an
//!   `f64`;
//! - and a map drops one of a pair of duplicate keys, silently changing what is
//!   sent.
//!
//! So nothing is parsed. The body is split into the spans the comment scanner
//! already produces, the code spans are chopped into JSON's punctuation, and
//! the lot is written back out with fresh indentation. **Every token is copied
//! byte for byte**; only the whitespace *between* tokens is this module's to
//! decide. Numbers, duplicate keys, key order, comments and templates therefore
//! survive by construction rather than by care.

use super::json_comments::{Piece, bodies_equivalent, parses_as_json, scan, wire_body};

/// One level of nesting. Two spaces is what every JSON formatter the user has
/// already met does, and a body is nested deeply enough that four gets wide.
const INDENT: &str = "  ";

/// What a token is, as far as the layout rules care.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// `{` or `[`.
    Open,
    /// `}` or `]`.
    Close,
    Comma,
    Colon,
    /// A string, a template, a number, `true`/`false`/`null` — anything that
    /// isn't punctuation. Never inspected, only copied.
    Value,
    /// `// …`. Everything after it on the line is commentary, so whatever
    /// follows it *must* start a new line or it would be swallowed.
    LineComment,
    /// `/* … */`, which may run over several lines.
    BlockComment,
}

struct Tok<'a> {
    kind: Kind,
    text: &'a str,
    /// Newlines between the previous token and this one. Two or more means the
    /// author left a blank line, and a blank line between two fields is
    /// grouping they meant — a formatter that flattens it is losing something
    /// the same way a formatter that drops comments is.
    breaks: usize,
    /// Roughly the column the token started at, used only to keep the inner
    /// shape of a multi-line block comment. Counted in bytes, so a multi-byte
    /// character earlier on the line skews it; the cost of being wrong is a
    /// comment indented a space or two oddly, so exactness isn't worth a
    /// char-by-char walk.
    col: usize,
}

/// Split body text into layout tokens.
///
/// Strings, templates and comments arrive whole from [`scan`] and are taken as
/// they are — that is what makes a `,` inside a string, or a `}` inside a
/// `{{ template }}`, harmless here. Only the code spans are chopped further.
fn tokens(src: &str) -> Vec<Tok<'_>> {
    let b = src.as_bytes();
    let mut out: Vec<Tok<'_>> = Vec::new();
    let mut breaks = 0usize;
    let mut line_start = 0usize;
    for (piece, a, e) in scan(src) {
        let text = &src[a..e];
        if piece == Piece::Text {
            let mut i = a;
            while i < e {
                if b[i].is_ascii_whitespace() {
                    if b[i] == b'\n' {
                        breaks += 1;
                        line_start = i + 1;
                    }
                    i += 1;
                    continue;
                }
                let kind = match b[i] {
                    b'{' | b'[' => Kind::Open,
                    b'}' | b']' => Kind::Close,
                    b',' => Kind::Comma,
                    b':' => Kind::Colon,
                    _ => Kind::Value,
                };
                let start = i;
                if kind == Kind::Value {
                    // Runs to the next thing that is punctuation or space.
                    // Every stop byte is ASCII, so this can't split a
                    // multi-byte character.
                    while i < e
                        && !b[i].is_ascii_whitespace()
                        && !matches!(b[i], b'{' | b'[' | b'}' | b']' | b',' | b':')
                    {
                        i += 1;
                    }
                } else {
                    i += 1;
                }
                out.push(Tok {
                    kind,
                    text: &src[start..i],
                    breaks,
                    col: start - line_start,
                });
                breaks = 0;
            }
            continue;
        }
        let kind = match piece {
            Piece::Comment if text.starts_with("//") => Kind::LineComment,
            Piece::Comment => Kind::BlockComment,
            _ => Kind::Value,
        };
        out.push(Tok {
            kind,
            text,
            breaks,
            col: a - line_start,
        });
        breaks = 0;
        // A block comment or a template may carry newlines of its own, and the
        // next token's column is measured from the last of them.
        if let Some(nl) = text.rfind('\n') {
            line_start = a + nl + 1;
        }
    }
    out
}

/// What goes between two tokens.
enum Gap {
    None,
    Space,
    Line { blank: bool },
}

/// Write the tokens out with fresh indentation.
fn lay_out(toks: &[Tok<'_>]) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for (i, t) in toks.iter().enumerate() {
        // A closer is indented with the line it closes, not with the contents,
        // so the level comes off before the indent is written.
        if t.kind == Kind::Close {
            depth = depth.saturating_sub(1);
        }
        let gap = if i == 0 {
            Gap::None
        } else {
            let prev = toks[i - 1].kind;
            if prev == Kind::LineComment {
                // Nothing may share a line with a `//` that precedes it — not
                // even a comma, which would otherwise end up commented out and
                // turn a valid body into a broken one.
                Gap::Line {
                    blank: t.breaks >= 2,
                }
            } else {
                match t.kind {
                    Kind::Comma | Kind::Colon => Gap::None,
                    // An empty object or array is one thing, not three lines.
                    Kind::Close if prev == Kind::Open => Gap::None,
                    // A comment the author put at the end of a line belongs to
                    // that line; moving it above would change which field it
                    // is read as annotating.
                    Kind::LineComment | Kind::BlockComment if t.breaks == 0 => Gap::Space,
                    _ if prev == Kind::Colon => Gap::Space,
                    _ => Gap::Line {
                        blank: t.breaks >= 2,
                    },
                }
            }
        };
        match gap {
            Gap::None => {}
            Gap::Space => out.push(' '),
            Gap::Line { blank } => {
                out.push('\n');
                if blank {
                    out.push('\n');
                }
                out.push_str(&INDENT.repeat(depth));
            }
        }
        write_token(&mut out, t, depth);
        if t.kind == Kind::Open {
            depth += 1;
        }
    }
    out
}

/// Copy one token, re-indenting the inside of a multi-line block comment.
///
/// A `/* … */` drawn as a column of `*`s is the one token whose *internal*
/// whitespace matters, because moving only its first line leaves the rest
/// hanging where the old indent used to be. Each continuation line keeps
/// however much further it was indented than the opening `/*`, so the column
/// of stars stays a column of stars at the new depth.
fn write_token(out: &mut String, t: &Tok<'_>, depth: usize) {
    if t.kind != Kind::BlockComment || !t.text.contains('\n') {
        out.push_str(t.text);
        return;
    }
    let pad = INDENT.repeat(depth);
    let mut lines = t.text.split('\n');
    if let Some(first) = lines.next() {
        out.push_str(first);
    }
    for line in lines {
        out.push('\n');
        let body = line.trim_start();
        if body.is_empty() {
            continue;
        }
        let extra = (line.len() - body.len()).saturating_sub(t.col);
        out.push_str(&pad);
        out.push_str(&" ".repeat(extra));
        out.push_str(body);
    }
}

/// Every comment in `src`, in order, with its own indentation normalised away.
///
/// A multi-line block comment is re-indented on the way out, so comparing the
/// text byte for byte would report a change that isn't one. Trimming each line
/// leaves exactly what the check is for: whether a comment was dropped,
/// reordered, or had its words altered.
fn comments(src: &str) -> Vec<String> {
    scan(src)
        .into_iter()
        .filter(|(k, _, _)| *k == Piece::Comment)
        .map(|(_, a, b)| {
            src[a..b]
                .lines()
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect()
}

/// What a prettify request came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prettified {
    /// Laid out afresh; this is the new body text.
    Changed(String),
    /// Already laid out this way. Worth telling apart from a keystroke that did
    /// nothing because it was misaimed — silence would look like a bug.
    Unchanged,
    /// Nothing safe to do: an empty body, a GraphQL/XML/plain-text one, or JSON
    /// that is half-typed. Refusing beats mangling.
    NotJson,
}

/// Re-indent a JSON request body, keeping its comments and templates.
///
/// The result is checked before it is offered: it must still say the same thing
/// as the input once both are reduced to what goes on the wire, and it must
/// still carry the same comments in the same order. Neither check can fail as
/// the code stands — every token is copied verbatim — which is exactly why they
/// are worth keeping: the day a layout rule is added that drops something, this
/// refuses to format rather than quietly rewriting the user's request.
pub fn prettify_json(src: &str) -> Prettified {
    if src.trim().is_empty() {
        return Prettified::NotJson;
    }
    // Comments and the commas they orphan are stripped before the parse test,
    // so a commented body is still recognised as the JSON it is.
    if !parses_as_json(&wire_body(src)) {
        return Prettified::NotJson;
    }
    let mut out = lay_out(&tokens(src));
    if src.ends_with('\n') {
        out.push('\n');
    }
    if !bodies_equivalent(&wire_body(src), &wire_body(&out)) || comments(&out) != comments(src) {
        return Prettified::NotJson;
    }
    if out == src {
        Prettified::Unchanged
    } else {
        Prettified::Changed(out)
    }
}

/// Where a cursor sitting at byte offset `at` in `src` belongs in the
/// reformatted `out`.
///
/// Only the whitespace *between* tokens is this module's to rewrite, so the
/// sequence of non-whitespace bytes is identical on both sides. Counting them
/// is therefore an exact map rather than a guess, and the cursor stays on the
/// character the user was looking at instead of being dumped at the top of a
/// body that may be hundreds of lines long.
pub fn map_cursor(src: &str, out: &str, at: usize) -> usize {
    let wanted = src.as_bytes()[..at.min(src.len())]
        .iter()
        .filter(|b| !b.is_ascii_whitespace())
        .count();
    if wanted == 0 {
        return 0;
    }
    let mut seen = 0usize;
    let mut landed = out.len();
    for (i, b) in out.bytes().enumerate() {
        if !b.is_ascii_whitespace() {
            seen += 1;
            if seen == wanted {
                landed = i + 1;
                break;
            }
        }
    }
    // A multi-byte character counts as several non-whitespace bytes, so the
    // landing place can fall inside one; nudge forward to where a slice is
    // legal.
    while landed < out.len() && !out.is_char_boundary(landed) {
        landed += 1;
    }
    landed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pretty(src: &str) -> String {
        match prettify_json(src) {
            Prettified::Changed(s) => s,
            other => panic!("expected a reformat, got {other:?}"),
        }
    }

    #[test]
    fn a_minified_body_gains_indentation() {
        assert_eq!(
            pretty(r#"{"a":1,"b":[1,2],"c":{"d":true}}"#),
            "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2\n  ],\n  \"c\": {\n    \"d\": true\n  }\n}"
        );
    }

    #[test]
    fn an_empty_object_or_array_stays_on_one_line() {
        assert_eq!(
            pretty(r#"{"a":{},"b":[]}"#),
            "{\n  \"a\": {},\n  \"b\": []\n}"
        );
    }

    /// The reason this isn't a `serde_json` round-trip. Every one of these is
    /// quietly rewritten by a parse-and-print, and every one of them changes
    /// what the server is sent.
    #[test]
    fn numbers_are_copied_not_re_encoded() {
        let src = r#"{"a":1.50,"b":1e3,"c":12345678901234567890,"d":-0.0}"#;
        let out = pretty(src);
        for atom in ["1.50", "1e3", "12345678901234567890", "-0.0"] {
            assert!(out.contains(atom), "{atom} was rewritten: {out}");
        }
    }

    #[test]
    fn duplicate_keys_and_key_order_are_left_alone() {
        let out = pretty(r#"{"z":1,"a":2,"z":3}"#);
        assert_eq!(out, "{\n  \"z\": 1,\n  \"a\": 2,\n  \"z\": 3\n}");
    }

    /// A bare template where a value belongs is ordinary in a Hurl body and is
    /// not valid JSON, so anything that parses first would refuse this outright.
    #[test]
    fn a_bare_template_survives_and_does_not_block_formatting() {
        assert_eq!(
            pretty("{\"n\":{{ COUNT }},\"u\":\"https://{{host}}/x\"}"),
            "{\n  \"n\": {{ COUNT }},\n  \"u\": \"https://{{host}}/x\"\n}"
        );
    }

    /// A `{` inside a template must not open a level of indentation, and a `,`
    /// or `}` inside a string must not lay anything out.
    #[test]
    fn punctuation_inside_strings_and_templates_is_data() {
        assert_eq!(
            pretty(r#"{"s":"a,b{c}d","t":"[]"}"#),
            "{\n  \"s\": \"a,b{c}d\",\n  \"t\": \"[]\"\n}"
        );
    }

    #[test]
    fn a_comment_on_its_own_line_keeps_its_own_line() {
        assert_eq!(
            pretty("{\n// who is asking\n\"id\":1}"),
            "{\n  // who is asking\n  \"id\": 1\n}"
        );
    }

    /// Which field a trailing note annotates is the position it was written
    /// in; a formatter that lifted it onto its own line would re-point it at
    /// the field below.
    #[test]
    fn a_trailing_comment_stays_at_the_end_of_its_line() {
        assert_eq!(
            pretty("{\"id\":1, // the caller\n\"n\":2}"),
            "{\n  \"id\": 1, // the caller\n  \"n\": 2\n}"
        );
    }

    /// The failure this guards against writes a body that cannot be read back:
    /// gluing the comma to the end of a `//` comment puts it inside the
    /// comment, and the object loses its separator.
    #[test]
    fn a_comma_is_never_swallowed_by_the_comment_before_it() {
        let out = pretty("{\"a\":1 // note\n,\"b\":2}");
        assert!(
            !out.contains("// note,"),
            "the comma was commented out: {out}"
        );
        assert!(parses_as_json(&wire_body(&out)), "unreadable result: {out}");
    }

    /// Commenting out the last field of an object is one of the main reasons to
    /// want comments, and it leaves a comma with nothing after it. The authored
    /// text keeps both; only the wire form drops the comma.
    #[test]
    fn a_commented_out_last_field_still_formats() {
        let out = pretty("{\"a\":1,\n// \"b\": 2\n}");
        assert_eq!(out, "{\n  \"a\": 1,\n  // \"b\": 2\n}");
    }

    #[test]
    fn a_blank_line_between_fields_is_kept_as_grouping() {
        assert_eq!(
            pretty("{\"a\":1,\n\n\n\"b\":2}"),
            "{\n  \"a\": 1,\n\n  \"b\": 2\n}",
            "several blank lines collapse to one, but the grouping survives"
        );
    }

    #[test]
    fn a_multi_line_block_comment_keeps_its_column_of_stars() {
        let out = pretty("{\n/* one\n * two\n */\n\"a\":1}");
        assert_eq!(out, "{\n  /* one\n   * two\n   */\n  \"a\": 1\n}");
    }

    #[test]
    fn a_body_that_is_already_laid_out_this_way_reports_no_change() {
        let src = "{\n  \"a\": 1\n}";
        assert_eq!(prettify_json(src), Prettified::Unchanged);
    }

    #[test]
    fn a_trailing_newline_is_kept_and_one_that_was_absent_is_not_added() {
        assert!(pretty("{\"a\":1}\n").ends_with("}\n"));
        assert!(!pretty("{\"a\":1}").ends_with('\n'));
    }

    /// GraphQL is the case that makes "strip the slashes anyway" unacceptable:
    /// `//` in a query is data, and a half-typed body is the normal state of an
    /// editor, so neither may be touched.
    #[test]
    fn anything_that_is_not_json_is_refused_rather_than_mangled() {
        for src in [
            "",
            "   \n ",
            "query { user { name } }",
            "<order><id>1</id></order>",
            "{\"a\": ",
            "plain text // not a comment",
        ] {
            assert_eq!(
                prettify_json(src),
                Prettified::NotJson,
                "should have refused: {src:?}"
            );
        }
    }

    /// Formatting twice must give what formatting once gave, or the keystroke
    /// would keep reporting a change and the body would keep drifting.
    #[test]
    fn formatting_is_idempotent() {
        for src in [
            r#"{"a":1,"b":[1,{"c":2}],"d":{}}"#,
            "{\n// lead\n\"a\":1, // trail\n\n\"b\":[]}",
            "{\"n\":{{ COUNT }}}",
            "[1,2,3]",
            "{\n/* one\n * two\n */\n\"a\":1}",
        ] {
            let once = pretty(src);
            assert_eq!(
                prettify_json(&once),
                Prettified::Unchanged,
                "second pass moved it: {once}"
            );
        }
    }

    /// The cursor has to survive the reflow, or prettifying a long body means
    /// finding your place again by hand.
    #[test]
    fn the_cursor_lands_on_the_same_character_it_was_on() {
        let src = r#"{"a":1,"bee":2}"#;
        let out = pretty(src);
        // Just after the `b` of "bee".
        let at = src.find("bee").unwrap() + 1;
        let mapped = map_cursor(src, &out, at);
        assert_eq!(&out[mapped - 1..mapped], "b");
        assert_eq!(map_cursor(src, &out, 0), 0, "the start stays the start");
        assert_eq!(
            map_cursor(src, &out, src.len()),
            out.len(),
            "and the end stays the end"
        );
    }

    #[test]
    fn mapping_a_cursor_never_lands_inside_a_character() {
        let src = "{\"n\":\"café ☕\",\"b\":2}";
        let out = pretty(src);
        for at in 0..=src.len() {
            let mapped = map_cursor(src, &out, at);
            assert!(out.is_char_boundary(mapped), "split a character at {at}");
        }
    }

    /// The safety net doing its job in the one shape that reaches it.
    #[test]
    fn the_result_always_says_what_the_input_said() {
        for src in [
            r#"{"a":1,"b":[1,2]}"#,
            "{\n// c\n\"a\":1}",
            "{\"u\":\"https://x/a//b\"}",
        ] {
            let out = pretty(src);
            assert!(bodies_equivalent(&wire_body(src), &wire_body(&out)));
        }
    }
}
