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

use super::theme::Theme;

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
pub(crate) fn highlight_line(line: &str, th: &Theme) -> Vec<Span<'static>> {
    // A whole-line comment/directive: dim the entire line (leading indentation
    // included) so it recedes behind the statements.
    if line.trim_start().starts_with('#') {
        return vec![Span::styled(line.to_string(), Style::default().fg(th.dim))];
    }

    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut spans: Vec<Span<'static>> = Vec::new();
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
        if is_word_char(c) {
            let start = i;
            while i < n && is_word_char(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let style = if is_keyword(&word) {
                Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(th.text)
            };
            spans.push(Span::styled(word, style));
            continue;
        }
        // A run of punctuation/whitespace, up to the next word or placeholder.
        let start = i;
        while i < n {
            let ch = chars[i];
            if is_word_char(ch) || (ch == '{' && i + 1 < n && chars[i + 1] == '{') {
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

/// Apply the error highlight (error colour + underline) to a line's spans in
/// place — used for the one line the parser rejected.
fn mark_error(spans: &mut [Span<'static>], th: &Theme) {
    for sp in spans {
        sp.style = sp.style.fg(th.err).add_modifier(Modifier::UNDERLINED);
    }
}

/// Highlight one source row and apply the error style when it is the parser's
/// rejected line (`error_line` is 1-based). Shared by the read view and the
/// live editor so both look identical.
pub(crate) fn highlight_row(
    row: usize,
    line: &str,
    error_line: Option<usize>,
    th: &Theme,
) -> Vec<Span<'static>> {
    let mut spans = highlight_line(line, th);
    if error_line == Some(row + 1) {
        mark_error(&mut spans, th);
    }
    spans
}

/// Highlight a whole report source into styled [`Line`]s. When `error_line` is
/// `Some(n)` (a 1-based parser error line), that line is recoloured to the
/// error style so a malformed script stands out.
pub(crate) fn highlight_source(
    text: &str,
    error_line: Option<usize>,
    th: &Theme,
) -> Vec<Line<'static>> {
    text.split('\n')
        .enumerate()
        .map(|(i, line)| Line::from(highlight_row(i, line, error_line, th)))
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
        let spans = highlight_line(line, &th);
        assert_tiles(line, &spans);
        let report = spans.iter().find(|s| s.content == "REPORT").unwrap();
        assert_eq!(report.style.fg, Some(th.accent));
        assert!(report.style.add_modifier.contains(Modifier::BOLD));
        // A non-keyword identifier is plain text, not accented.
        let ident = spans.iter().find(|s| s.content == "upload").unwrap();
        assert_eq!(ident.style.fg, Some(th.text));
        assert!(!ident.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn keywords_match_case_insensitively() {
        let th = th();
        let spans = highlight_line("report request x", &th);
        let report = spans.iter().find(|s| s.content == "report").unwrap();
        assert_eq!(report.style.fg, Some(th.accent));
    }

    #[test]
    fn substitution_placeholders_use_the_subst_colour() {
        let th = th();
        let line = "URL={{ base }}/api";
        let spans = highlight_line(line, &th);
        assert_tiles(line, &spans);
        let var = spans.iter().find(|s| s.content == "{{ base }}").unwrap();
        assert_eq!(var.style.fg, Some(th.subst));
    }

    #[test]
    fn comment_lines_are_dimmed_wholesale() {
        let th = th();
        let line = "  # collection: ./x.hurl";
        let spans = highlight_line(line, &th);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, line);
        assert_eq!(spans[0].style.fg, Some(th.dim));
    }

    #[test]
    fn the_error_row_is_recoloured_and_underlined() {
        let th = th();
        // error_line is 1-based; row 0 is line 1.
        let spans = highlight_row(0, "REQUEST bad", Some(1), &th);
        for sp in &spans {
            assert_eq!(sp.style.fg, Some(th.err));
            assert!(sp.style.add_modifier.contains(Modifier::UNDERLINED));
        }
        // A different row keeps its normal highlighting.
        let ok = highlight_row(1, "REQUEST good", Some(1), &th);
        let kw = ok.iter().find(|s| s.content == "REQUEST").unwrap();
        assert_eq!(kw.style.fg, Some(th.accent));
    }

    #[test]
    fn highlight_source_yields_one_line_per_row() {
        let th = th();
        let lines = highlight_source("REQUEST a\nEND", None, &th);
        assert_eq!(lines.len(), 2);
    }
}
