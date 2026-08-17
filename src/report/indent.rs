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

/// Why a [`reformat`] didn't happen.
pub enum ReformatError {
    /// The source doesn't parse, so its block structure isn't known. Re-indenting
    /// guesswork is worse than leaving a broken file alone — fix the syntax first.
    Unparseable(String),
    /// Re-indenting changed what the script *means*. This should be impossible
    /// (only leading whitespace is touched), so it indicates a bug rather than
    /// anything the user did — but it is checked, because silently rewriting a
    /// script into a different one is the worst outcome this feature could have.
    WouldChangeMeaning,
}

/// Re-indent a whole PaperTrail script so every line sits at its true block
/// depth, four spaces per level.
///
/// **Only leading whitespace changes.** Comments, blank lines, spacing within a
/// statement and the order of everything are preserved exactly, which is what
/// makes this safe to run over a file you didn't write. That rules out the
/// obvious implementation — parse to a [`ReportFlow`](crate::report::flow::ReportFlow)
/// and call `to_text()` — because the AST has nowhere to put a body comment, so
/// round-tripping through it deletes them.
///
/// The result is verified by re-parsing it and comparing the AST to the
/// original's, so a reformat can never change what a script does.
///
/// Returns `Ok(None)` when the text is already correctly indented, so callers
/// can skip the undo entry and the dirty mark.
pub fn reformat(src: &str) -> Result<Option<String>, ReformatError> {
    let before = crate::report::parser::parse_flow(src)
        .map_err(|e| ReformatError::Unparseable(e.message))?;

    let mut out = String::with_capacity(src.len());
    let mut depth = 0usize;
    for line in src.lines() {
        let body = line.trim();
        if body.is_empty() {
            // A blank line stays blank rather than keeping stale indentation.
            out.push('\n');
            continue;
        }
        // `END` closes the block it sits in, so it dedents *before* it is placed.
        if is_end_line(body) {
            depth = depth.saturating_sub(1);
        }
        for _ in 0..depth {
            out.push_str(INDENT_UNIT);
        }
        out.push_str(body);
        out.push('\n');
        if opens_block(body) {
            depth += 1;
        }
    }
    // `str::lines` drops the final newline; put one back only if there was one.
    if !src.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }

    if out == src {
        return Ok(None);
    }
    let after =
        crate::report::parser::parse_flow(&out).map_err(|_| ReformatError::WouldChangeMeaning)?;
    if after != before {
        return Err(ReformatError::WouldChangeMeaning);
    }
    Ok(Some(out))
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

    /// The case that prompted the feature: wrapping an existing block in a new
    /// outer loop leaves every line of the old body one level short.
    #[test]
    fn reformat_reindents_a_newly_wrapped_block() {
        let src = "# collection: c\n\nFOR T IN FILES \"*.txt\"\nFOR F IN FILES \"*.png\"\nREQUEST a\nEND\nEND\n";
        let out = reformat(src).ok().flatten().expect("reindented");
        assert_eq!(
            out,
            "# collection: c\n\nFOR T IN FILES \"*.txt\"\n    FOR F IN FILES \"*.png\"\n        REQUEST a\n    END\nEND\n"
        );
    }

    /// Body comments and blank lines are the reason this doesn't round-trip
    /// through the AST: the flow has nowhere to keep them, so `to_text()` would
    /// silently delete every one.
    #[test]
    fn reformat_keeps_comments_and_blank_lines() {
        let src =
            "# collection: c\n\nFOR T IN FILES \"*.txt\"\n# why we do this\n\nREQUEST a\nEND\n";
        let out = reformat(src).ok().flatten().expect("reindented");
        assert!(
            out.contains("    # why we do this"),
            "comment kept: {out:?}"
        );
        assert!(out.contains("\n\n"), "blank line kept: {out:?}");
    }

    /// A `WITH` block nests like a loop does.
    #[test]
    fn reformat_indents_a_with_block() {
        let src =
            "# collection: c\n\nREPORT REQUEST proc AS p WITH\nframe: jsonpath \"$.a\"\nEND\n";
        let out = reformat(src).ok().flatten().expect("reindented");
        assert!(out.contains("    frame: jsonpath"), "{out:?}");
    }

    /// Already-tidy text reports "nothing to do" rather than a no-op edit, so it
    /// costs no undo entry and no dirty mark.
    #[test]
    fn reformat_of_tidy_text_changes_nothing() {
        let src = "# collection: c\n\nFOR T IN FILES \"*.txt\"\n    REQUEST a\nEND\n";
        assert!(matches!(reformat(src), Ok(None)));
    }

    /// Without a parse there is no block structure to indent to, and guessing at
    /// a broken file is worse than leaving it alone.
    #[test]
    fn reformat_refuses_source_that_does_not_parse() {
        assert!(matches!(
            reformat("# collection: c\nFOR X IN\n"),
            Err(ReformatError::Unparseable(_))
        ));
    }

    /// Only leading whitespace moves — nothing inside a statement is touched,
    /// including a quoted string that happens to look like indentation.
    #[test]
    fn reformat_does_not_touch_the_inside_of_a_statement() {
        let src = "# collection: c\n\nFOR T IN FILES \"a   b\"\nREQUEST \"x  y\"\nEND\n";
        let out = reformat(src).ok().flatten().expect("reindented");
        assert!(out.contains("FILES \"a   b\""), "{out:?}");
        assert!(out.contains("REQUEST \"x  y\""), "{out:?}");
    }

    /// A file with no trailing newline doesn't silently gain one.
    #[test]
    fn reformat_preserves_a_missing_final_newline() {
        let src = "# collection: c\n\nFOR T IN FILES \"*.txt\"\nREQUEST a\nEND";
        let out = reformat(src).ok().flatten().expect("reindented");
        assert!(!out.ends_with('\n'), "{out:?}");
    }
}
