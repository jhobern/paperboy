//! Front-end-agnostic **source indentation** rules for PaperTrail text.
//!
//! A `.trail` file nests: `FOR … END` and `REPORT REQUEST … WITH … END` open
//! blocks whose bodies are indented one level. Both editors (the terminal UI's
//! own [`Editor`](crate::tui::editor) and the GUI's `egui` text area) want the
//! same two behaviours while typing — inherit the current line's indentation on
//! a new line, and snap a finished `END` back to its opener — so the rules live
//! here rather than being reimplemented (and drifting) per front-end.
//!
//! These are deliberately *textual*: they work on lines, not on the AST, because
//! the buffer is usually mid-edit and does not parse.

use crate::report::parser::opens_block;

/// One level of source indentation (matches the flow serializer's four spaces,
/// so hand-typed text and `ReportFlow::to_text` output agree).
pub const INDENT_UNIT: &str = "    ";

/// The leading whitespace of `line`, as an owned string (used to inherit the
/// current line's indentation onto a freshly-inserted newline).
pub fn leading_ws(line: &str) -> String {
    line.chars().take_while(|c| c.is_whitespace()).collect()
}

/// The indentation a newly-inserted line should start with, given the line the
/// cursor was on: the current line's own indentation, plus one level when that
/// line opens a block.
pub fn indent_for_new_line(current_line: &str) -> String {
    let mut indent = leading_ws(current_line);
    if opens_block(current_line) {
        indent.push_str(INDENT_UNIT);
    }
    indent
}

/// Whether `line` is exactly a block-closing `END` (ignoring case and
/// surrounding whitespace).
pub fn is_end_line(line: &str) -> bool {
    line.trim().eq_ignore_ascii_case("END")
}

/// The indentation an `END` should take, given every line *above* it in source
/// order: that of the block opener it closes.
///
/// Walks upward tracking opener/`END` balance so nested blocks resolve to the
/// right opener. Returns `None` when the nesting above is unbalanced (a stray
/// `END`, or a block that was never opened), in which case the caller should
/// leave the line alone rather than guess.
pub fn matching_opener_indent<S: AsRef<str>>(lines_above: &[S]) -> Option<String> {
    let mut depth = 0i32;
    for prev in lines_above.iter().rev() {
        let prev = prev.as_ref();
        if is_end_line(prev) {
            depth += 1;
        } else if opens_block(prev) {
            if depth == 0 {
                return Some(leading_ws(prev));
            }
            depth -= 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_ws_takes_only_the_prefix() {
        assert_eq!(leading_ws("    REQUEST a"), "    ");
        assert_eq!(leading_ws("REQUEST a"), "");
        assert_eq!(leading_ws("\t  x"), "\t  ");
        // A blank line is all whitespace, and inherits all of it.
        assert_eq!(leading_ws("   "), "   ");
    }

    #[test]
    fn a_new_line_inherits_the_current_indent() {
        assert_eq!(indent_for_new_line("    REQUEST a"), "    ");
        assert_eq!(indent_for_new_line("REQUEST a"), "");
    }

    #[test]
    fn a_new_line_after_an_opener_gains_one_level() {
        assert_eq!(indent_for_new_line("FOR F IN FILES \"docs\""), INDENT_UNIT);
        assert_eq!(
            indent_for_new_line("    FOR F IN FILES \"docs\""),
            format!("    {INDENT_UNIT}")
        );
        assert_eq!(
            indent_for_new_line("REPORT REQUEST process WITH"),
            INDENT_UNIT
        );
        // A REPORT with no trailing WITH is a single-line statement.
        assert_eq!(indent_for_new_line("REPORT REQUEST process"), "");
    }

    #[test]
    fn is_end_line_ignores_case_and_padding() {
        assert!(is_end_line("END"));
        assert!(is_end_line("   end  "));
        assert!(!is_end_line("ENDS"));
        assert!(!is_end_line("REQUEST END"));
    }

    #[test]
    fn end_snaps_to_its_own_opener_through_nesting() {
        let above = [
            "FOR A IN [\"x\"]",
            "    FOR B IN [\"y\"]",
            "        REQUEST r",
            "    END",
            "    REQUEST s",
        ];
        // The inner block is already closed, so this END belongs to the outer FOR.
        assert_eq!(matching_opener_indent(&above), Some(String::new()));

        let above = [
            "FOR A IN [\"x\"]",
            "    FOR B IN [\"y\"]",
            "        REQUEST r",
        ];
        assert_eq!(matching_opener_indent(&above), Some("    ".to_string()));
    }

    #[test]
    fn an_unbalanced_end_has_no_opener() {
        assert_eq!(matching_opener_indent::<&str>(&[]), None);
        assert_eq!(matching_opener_indent(&["REQUEST a", "END"]), None);
    }
}
