//! Lightweight syntax highlighting for PaperTrail report source.
//!
//! This is a *view-only* concern (it produces `ratatui` styled spans from the
//! theme), so it lives here rather than in the front-end-agnostic `report/`
//! core. The same per-line highlighter feeds both the read-only source panel
//! (`MultiSelectPanel::set_styled_content`) and the live editor
//! (`render_editor_highlighted`), so the source looks identical whether or not
//! it currently has edit focus.
//!
//! The highlighting is deliberately simple — enough to tell a well-formed flow
//! from a malformed one at a glance:
//! - PaperTrail keywords are drawn in the theme accent (bold);
//! - `{{ … }}` substitution placeholders reuse the app's substitution colour,
//!   matching how they read everywhere else;
//! - whole-line `#` comments/directives are dimmed;
//! - the single line the parser rejects (when the source doesn't parse) is
//!   recoloured to the error colour and underlined, so a broken script is
//!   obvious without reading the validation panel.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashSet;

use super::theme::Theme;

/// Context the highlighter needs to colour *references* by whether they
/// currently resolve: the parser's rejected line, whether the report's
/// `# collection:` directive binds to a loaded collection, and the set of
/// loaded global-environment names (so `# environment:` and `ENVS`/`BASELINE`/
/// `COMPARISON` env names light up green when loaded, amber when not). Built
/// fresh each draw from `TuiApp`; `Default` (nothing resolves) is used by the
/// unit tests.
#[derive(Default)]
pub(crate) struct HlCtx {
    /// 1-based line the parser rejected, recoloured to the error style.
    pub error_line: Option<usize>,
    /// Whether the report's `# collection:` reference resolves to a loaded tab.
    pub collection_resolves: bool,
    /// Names of every currently-loaded global environment.
    pub loaded_envs: HashSet<String>,
    /// Titles of every request in the bound collection, so a `REQUEST`
    /// (or `REPORT REQUEST`) name lights up green when it resolves to a real
    /// request and amber when it doesn't (mirroring env-name colouring).
    pub request_names: HashSet<String>,
}

/// Every PaperTrail keyword (matched case-insensitively), including the two
/// reserved-but-unimplemented ones (`JOIN`/`ON`) so a flow that reaches for
/// them still reads as "these are keywords".
const KEYWORDS: &[&str] = &[
    "REQUEST",
    "REPORT",
    "FOR",
    "IN",
    "FILES",
    "FOLDERS",
    "ENVS",
    "TUPLES",
    "FROM",
    "ZIP",
    "CONCAT",
    "MATCH",
    "WITH",
    "LIST",
    "BASELINE",
    "COMPARISON",
    "END",
    "PARALLEL",
    "AS",
    "RESPONSE",
    "RAW",
    "PRETTY",
    "JOIN",
    "ON",
];

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_keyword(word: &str) -> bool {
    let upper = word.to_ascii_uppercase();
    KEYWORDS.contains(&upper.as_str())
}

/// Tokenise one source line into styled spans that tile the whole line (their
/// character lengths sum to the line's length), so it can be clipped by the
/// editor's horizontal-scroll window without misaligning.
pub(crate) fn highlight_line(line: &str, ctx: &HlCtx, th: &Theme) -> Vec<Span<'static>> {
    // A whole-line comment/directive. `# collection:` / `# environment:` get
    // their value recoloured by whether it resolves; every other comment is
    // dimmed wholesale so it recedes behind the statements.
    if line.trim_start().starts_with('#') {
        return highlight_comment(line, ctx, th);
    }

    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut spans: Vec<Span<'static>> = Vec::new();
    // Set once an `ENVS`/`BASELINE`/`COMPARISON` keyword has appeared on the
    // line: subsequent string literals name environments, so they are coloured
    // by whether that environment is loaded rather than left plain.
    let mut env_names = false;
    // Set the moment a `REQUEST` keyword is emitted: the very next name token
    // (bare word or quoted string) is a request name, coloured by whether it
    // resolves against the bound collection. Cleared once that token is seen
    // (or another keyword intervenes).
    let mut expect_request_name = false;
    let mut i = 0;
    while i < n {
        let c = chars[i];
        // `{{ … }}` substitution placeholder.
        if c == '{' && i + 1 < n && chars[i + 1] == '{' {
            let start = i;
            i += 2;
            while i < n && !(chars[i] == '}' && i + 1 < n && chars[i + 1] == '}') {
                i += 1;
            }
            i = (i + 2).min(n); // consume the closing `}}` if present
            let text: String = chars[start..i].iter().collect();
            spans.push(Span::styled(text, Style::default().fg(th.subst)));
            continue;
        }
        // A `"…"` string literal: never keyword-highlight its contents (a
        // request name like `"Upload for document"` must not light up `for`),
        // but still surface any `{{ … }}` substitution inside it. When the line
        // is an environment clause, the literal names an env and is coloured by
        // whether that env is loaded.
        if c == '"' {
            let start = i;
            i += 1;
            while i < n {
                // Honour `\"` / `\\` escapes so an embedded quote doesn't end
                // the string early.
                if chars[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if chars[i] == '"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            if env_names {
                let name = unquote_literal(&chars[start..i]);
                let colour = if ctx.loaded_envs.contains(&name) {
                    th.ok
                } else {
                    th.pending
                };
                spans.push(Span::styled(
                    chars[start..i].iter().collect::<String>(),
                    Style::default().fg(colour),
                ));
            } else if expect_request_name {
                expect_request_name = false;
                let name = unquote_literal(&chars[start..i]);
                let colour = if ctx.request_names.contains(&name) {
                    th.ok
                } else {
                    th.pending
                };
                spans.push(Span::styled(
                    chars[start..i].iter().collect::<String>(),
                    Style::default().fg(colour),
                ));
            } else {
                push_string_spans(&mut spans, &chars[start..i], th);
            }
            continue;
        }
        if is_word_char(c) {
            let start = i;
            while i < n && is_word_char(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let style = if is_keyword(&word) {
                let upper = word.to_ascii_uppercase();
                if matches!(upper.as_str(), "ENVS" | "BASELINE" | "COMPARISON") {
                    env_names = true;
                }
                // The token after a `REQUEST` keyword is a request name; any
                // other keyword ends that expectation.
                expect_request_name = upper == "REQUEST";
                Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
            } else if expect_request_name {
                expect_request_name = false;
                let colour = if ctx.request_names.contains(&word) {
                    th.ok
                } else {
                    th.pending
                };
                Style::default().fg(colour)
            } else {
                Style::default().fg(th.text)
            };
            spans.push(Span::styled(word, style));
            continue;
        }
        // A run of punctuation/whitespace, up to the next word, placeholder or
        // string literal.
        let start = i;
        while i < n {
            let ch = chars[i];
            if is_word_char(ch) || ch == '"' || (ch == '{' && i + 1 < n && chars[i + 1] == '{') {
                break;
            }
            i += 1;
        }
        let text: String = chars[start..i].iter().collect();
        spans.push(Span::styled(text, Style::default().fg(th.text)));
    }
    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }
    spans
}

/// Push the spans for a `"…"` string literal `s` (given as its chars): plain
/// text throughout — so keywords inside it are *not* highlighted — except for
/// any `{{ … }}` substitution, which keeps its substitution colour. The spans
/// tile `s` exactly.
fn push_string_spans(spans: &mut Vec<Span<'static>>, s: &[char], th: &Theme) {
    let n = s.len();
    let mut i = 0;
    let mut plain_start = 0;
    while i < n {
        if s[i] == '{' && i + 1 < n && s[i + 1] == '{' {
            if i > plain_start {
                spans.push(Span::styled(
                    s[plain_start..i].iter().collect::<String>(),
                    Style::default().fg(th.text),
                ));
            }
            let start = i;
            i += 2;
            while i < n && !(s[i] == '}' && i + 1 < n && s[i + 1] == '}') {
                i += 1;
            }
            i = (i + 2).min(n);
            spans.push(Span::styled(
                s[start..i].iter().collect::<String>(),
                Style::default().fg(th.subst),
            ));
            plain_start = i;
            continue;
        }
        i += 1;
    }
    if plain_start < n {
        spans.push(Span::styled(
            s[plain_start..n].iter().collect::<String>(),
            Style::default().fg(th.text),
        ));
    }
}

/// Highlight a whole-line comment. A `# collection: <ref>` or
/// `# environment: <name>` directive keeps its label dim but recolours the
/// value: green when it resolves (the collection is loaded / the env is
/// loaded), amber (`pending`) when it doesn't — so a mistyped or unloaded
/// reference is obvious at a glance. Any other comment is dimmed wholesale. The
/// returned spans tile the line exactly.
fn highlight_comment(line: &str, ctx: &HlCtx, th: &Theme) -> Vec<Span<'static>> {
    let dim = Style::default().fg(th.dim);
    let whole = || vec![Span::styled(line.to_string(), dim)];
    let Some(hash) = line.find('#') else {
        return whole();
    };
    let Some(rel_colon) = line[hash + 1..].find(':') else {
        return whole();
    };
    let colon = hash + 1 + rel_colon;
    let key = line[hash + 1..colon].trim().to_ascii_lowercase();
    let value_full = &line[colon + 1..];
    let value = value_full.trim();
    let colour = match key.as_str() {
        "collection" if ctx.collection_resolves => th.ok,
        "collection" => th.pending,
        "environment" if ctx.loaded_envs.contains(value) => th.ok,
        "environment" => th.pending,
        _ => return whole(),
    };
    if value.is_empty() {
        return whole();
    }
    // Split the line into: dim prefix (`# collection:`), dim leading space,
    // coloured value, dim trailing — all byte-contiguous slices of `line`.
    let ws_len = value_full.len() - value_full.trim_start().len();
    let value_start = colon + 1 + ws_len;
    let value_end = value_start + value.len();
    let mut spans = vec![Span::styled(line[..value_start].to_string(), dim)];
    spans.push(Span::styled(
        value.to_string(),
        Style::default().fg(colour).add_modifier(Modifier::BOLD),
    ));
    if value_end < line.len() {
        spans.push(Span::styled(line[value_end..].to_string(), dim));
    }
    spans
}

/// The inner text of a `"…"` string literal (given as its chars, quotes
/// included), with `\"`/`\\` escapes resolved — used to test an env name
/// against the set of loaded environments.
fn unquote_literal(s: &[char]) -> String {
    let inner = s
        .strip_prefix(&['"'])
        .unwrap_or(s)
        .strip_suffix(&['"'])
        .unwrap_or(s);
    let mut out = String::with_capacity(inner.len());
    let mut it = inner.iter();
    while let Some(&c) = it.next() {
        if c == '\\' {
            if let Some(&next) = it.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Apply the error highlight (error colour + underline) to a line's spans in
/// place — used for the one line the parser rejected.
fn mark_error(spans: &mut [Span<'static>], th: &Theme) {
    for sp in spans {
        sp.style = sp.style.fg(th.err).add_modifier(Modifier::UNDERLINED);
    }
}

/// Highlight one source row and apply the error style when it is the parser's
/// rejected line (`ctx.error_line` is 1-based). Shared by the read view and the
/// live editor so both look identical.
pub(crate) fn highlight_row(row: usize, line: &str, ctx: &HlCtx, th: &Theme) -> Vec<Span<'static>> {
    let mut spans = highlight_line(line, ctx, th);
    if ctx.error_line == Some(row + 1) {
        mark_error(&mut spans, th);
    }
    spans
}

/// Highlight a whole report source into styled [`Line`]s, recolouring the
/// parser's rejected line (when `ctx.error_line` is set) to the error style.
pub(crate) fn highlight_source(text: &str, ctx: &HlCtx, th: &Theme) -> Vec<Line<'static>> {
    text.split('\n')
        .enumerate()
        .map(|(i, line)| Line::from(highlight_row(i, line, ctx, th)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;
    use crate::tui::theme::theme;

    fn th() -> Theme {
        theme(&Language::English)
    }

    fn ctx() -> HlCtx {
        HlCtx::default()
    }

    /// The spans a line is broken into must tile it exactly (their character
    /// lengths sum to the line length), or the editor's horizontal-scroll
    /// clipping would misalign the highlighting against the cursor.
    fn assert_tiles(line: &str, spans: &[Span<'static>]) {
        let total: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(total, line.chars().count(), "spans must tile {line:?}");
    }

    #[test]
    fn keywords_are_accented_and_bold() {
        let th = th();
        let line = "REPORT REQUEST upload";
        let spans = highlight_line(line, &ctx(), &th);
        assert_tiles(line, &spans);
        let report = spans.iter().find(|s| s.content == "REPORT").unwrap();
        assert_eq!(report.style.fg, Some(th.accent));
        assert!(report.style.add_modifier.contains(Modifier::BOLD));
        // A non-keyword identifier is not keyword-accented (here it's a
        // request name, coloured by resolution — never bold like a keyword).
        let ident = spans.iter().find(|s| s.content == "upload").unwrap();
        assert_ne!(ident.style.fg, Some(th.accent));
        assert!(!ident.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn keywords_match_case_insensitively() {
        let th = th();
        let spans = highlight_line("report request x", &ctx(), &th);
        let report = spans.iter().find(|s| s.content == "report").unwrap();
        assert_eq!(report.style.fg, Some(th.accent));
    }

    #[test]
    fn substitution_placeholders_use_the_subst_colour() {
        let th = th();
        let line = "URL={{ base }}/api";
        let spans = highlight_line(line, &ctx(), &th);
        assert_tiles(line, &spans);
        let var = spans.iter().find(|s| s.content == "{{ base }}").unwrap();
        assert_eq!(var.style.fg, Some(th.subst));
    }

    #[test]
    fn plain_comment_lines_are_dimmed_wholesale() {
        let th = th();
        let line = "  # just a note";
        let spans = highlight_line(line, &ctx(), &th);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, line);
        assert_eq!(spans[0].style.fg, Some(th.dim));
    }

    #[test]
    fn collection_directive_value_is_green_when_bound_and_amber_when_not() {
        let th = th();
        let line = "# collection: ./x.hurl";
        // Unbound: the value is amber (`pending`), the label stays dim.
        let spans = highlight_line(line, &ctx(), &th);
        assert_tiles(line, &spans);
        let val = spans.iter().find(|s| s.content == "./x.hurl").unwrap();
        assert_eq!(val.style.fg, Some(th.pending));
        // Bound: the value is green.
        let bound = HlCtx {
            collection_resolves: true,
            ..Default::default()
        };
        let spans = highlight_line(line, &bound, &th);
        let val = spans.iter().find(|s| s.content == "./x.hurl").unwrap();
        assert_eq!(val.style.fg, Some(th.ok));
    }

    #[test]
    fn environment_directive_value_tracks_whether_the_env_is_loaded() {
        let th = th();
        let line = "# environment: staging";
        let spans = highlight_line(line, &ctx(), &th);
        let val = spans.iter().find(|s| s.content == "staging").unwrap();
        assert_eq!(val.style.fg, Some(th.pending));
        let loaded = HlCtx {
            loaded_envs: HashSet::from(["staging".to_string()]),
            ..Default::default()
        };
        let spans = highlight_line(line, &loaded, &th);
        let val = spans.iter().find(|s| s.content == "staging").unwrap();
        assert_eq!(val.style.fg, Some(th.ok));
    }

    #[test]
    fn envs_clause_names_are_coloured_by_loaded_state() {
        let th = th();
        let line = "FOR TARGET IN ENVS \"prod\", \"staging\"";
        let loaded = HlCtx {
            loaded_envs: HashSet::from(["prod".to_string()]),
            ..Default::default()
        };
        let spans = highlight_line(line, &loaded, &th);
        assert_tiles(line, &spans);
        let prod = spans.iter().find(|s| s.content == "\"prod\"").unwrap();
        assert_eq!(prod.style.fg, Some(th.ok), "loaded env is green");
        let staging = spans.iter().find(|s| s.content == "\"staging\"").unwrap();
        assert_eq!(staging.style.fg, Some(th.pending), "unloaded env is amber");
    }

    #[test]
    fn keywords_inside_a_string_literal_are_not_highlighted() {
        let th = th();
        // The request name contains the keyword-looking word "for".
        let line = "REQUEST \"Upload for document\"";
        let spans = highlight_line(line, &ctx(), &th);
        assert_tiles(line, &spans);
        // The leading REQUEST keyword is still accented…
        let kw = spans.iter().find(|s| s.content == "REQUEST").unwrap();
        assert_eq!(kw.style.fg, Some(th.accent));
        // …but nothing inside the quoted string is accent-coloured or bold (so
        // "for" reads as part of the name, not a keyword). The whole literal is
        // coloured as one request-name span, not tokenised into keywords.
        for sp in &spans {
            if sp.content.contains("for") || sp.content.contains("Upload") {
                assert_ne!(
                    sp.style.fg,
                    Some(th.accent),
                    "string-literal content is not keyword-accented"
                );
                assert!(!sp.style.add_modifier.contains(Modifier::BOLD));
            }
        }
    }

    #[test]
    fn request_names_are_coloured_by_whether_they_resolve() {
        let th = th();
        let line = "REQUEST Oauth";
        // Unbound / unknown request → amber (pending).
        let spans = highlight_line(line, &ctx(), &th);
        assert_tiles(line, &spans);
        let name = spans.iter().find(|s| s.content == "Oauth").unwrap();
        assert_eq!(name.style.fg, Some(th.pending), "unknown request is amber");
        // Known request → green.
        let bound = HlCtx {
            request_names: HashSet::from(["Oauth".to_string()]),
            ..Default::default()
        };
        let spans = highlight_line(line, &bound, &th);
        let name = spans.iter().find(|s| s.content == "Oauth").unwrap();
        assert_eq!(name.style.fg, Some(th.ok), "resolved request is green");
    }

    #[test]
    fn quoted_request_name_after_report_request_is_coloured() {
        let th = th();
        let bound = HlCtx {
            request_names: HashSet::from(["Upload document".to_string()]),
            ..Default::default()
        };
        let line = "REPORT REQUEST \"Upload document\"";
        let spans = highlight_line(line, &bound, &th);
        assert_tiles(line, &spans);
        let name = spans
            .iter()
            .find(|s| s.content == "\"Upload document\"")
            .unwrap();
        assert_eq!(name.style.fg, Some(th.ok));
    }

    #[test]
    fn only_the_first_token_after_request_is_a_name() {
        // In `REQUEST proc AS result`, only `proc` is the request name; the
        // `AS` alias is a plain identifier, not coloured as a request.
        let th = th();
        let bound = HlCtx {
            request_names: HashSet::from(["proc".to_string()]),
            ..Default::default()
        };
        let line = "REQUEST proc AS result";
        let spans = highlight_line(line, &bound, &th);
        let name = spans.iter().find(|s| s.content == "proc").unwrap();
        assert_eq!(name.style.fg, Some(th.ok));
        let alias = spans.iter().find(|s| s.content == "result").unwrap();
        assert_eq!(alias.style.fg, Some(th.text), "the alias stays plain text");
    }

    #[test]
    fn substitutions_inside_a_string_literal_still_highlight() {
        let th = th();
        let line = "FILES \"dir/{{NAME}}\"";
        let spans = highlight_line(line, &ctx(), &th);
        assert_tiles(line, &spans);
        let var = spans.iter().find(|s| s.content == "{{NAME}}").unwrap();
        assert_eq!(var.style.fg, Some(th.subst));
    }

    #[test]
    fn the_error_row_is_recoloured_and_underlined() {
        let th = th();
        // error_line is 1-based; row 0 is line 1.
        let err = HlCtx {
            error_line: Some(1),
            ..Default::default()
        };
        let spans = highlight_row(0, "REQUEST bad", &err, &th);
        for sp in &spans {
            assert_eq!(sp.style.fg, Some(th.err));
            assert!(sp.style.add_modifier.contains(Modifier::UNDERLINED));
        }
        // A different row keeps its normal highlighting.
        let ok = highlight_row(1, "REQUEST good", &err, &th);
        let kw = ok.iter().find(|s| s.content == "REQUEST").unwrap();
        assert_eq!(kw.style.fg, Some(th.accent));
    }

    #[test]
    fn highlight_source_yields_one_line_per_row() {
        let th = th();
        let lines = highlight_source("REQUEST a\nEND", &ctx(), &th);
        assert_eq!(lines.len(), 2);
    }
}
