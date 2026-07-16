//! Mouse (and keyboard-extended) text selection scoped to a single panel
//! (Request JSON / Response).
//!
//! The terminal's own click-drag selection can't be confined to one panel —
//! it always spans the full terminal row, sweeping up whatever's to the left
//! (other panels, borders, etc.). To let users copy a long response body or
//! URL cleanly, the app captures the mouse itself and implements its own
//! selection: dragging inside a panel selects text using ordinary "stream"
//! semantics (first line from the start column to its own end, full lines in
//! between, last line from its own start to the end column) — never a
//! rectangular block — and never anything outside that panel's own Rect.
//!
//! Selections are stored as [`TextPos`] (logical line/char-offset)
//! positions, not terminal (row, col) cells — see `wrapcache` — so the exact
//! same characters stay selected across a rewrap/rescroll/resize instead of
//! silently re-interpreting stale screen coordinates against new content.

use ratatui::layout::Rect;

use crate::wrapcache::{PanelWrap, TextPos};

/// Order two positions so the first is not after the second (a selection
/// dragged "backwards" — up or left — still resolves correctly).
pub fn ordered(a: TextPos, b: TextPos) -> (TextPos, TextPos) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Map a raw terminal (column, row) point onto the [`TextPos`] it
/// corresponds to, given the panel's Rect, its current scroll offset (in
/// wrapped rows), and its line/wrap cache. Points outside the area clamp to
/// its nearest edge, exactly as the on-screen content does.
pub fn point_to_textpos(point: (u16, u16), area: Rect, scroll: u16, wrap: &PanelWrap) -> TextPos {
    let (col, row) = point;
    let local_row = if area.height == 0 || row < area.y {
        0
    } else {
        ((row - area.y) as u32).min(area.height as u32 - 1)
    };
    let local_col = if area.width == 0 || col < area.x {
        0
    } else {
        (col - area.x) as usize
    };
    wrap.row_col_to_textpos(scroll as u32 + local_row, local_col)
}

/// The selected char range `(from, to_exclusive)` on `line`, given the
/// selection's ordered endpoints — "stream" semantics: the first line runs
/// from its start column to its own end, the last line from column 0 to its
/// end column, every line strictly between is selected in full.
fn range_for_line(line: usize, start: TextPos, end: TextPos, wrap: &PanelWrap) -> (usize, usize) {
    let len = wrap.line_char_len(line);
    if start.line == end.line {
        (start.col.min(len), (end.col + 1).min(len))
    } else if line == start.line {
        (start.col.min(len), len)
    } else if line == end.line {
        (0, (end.col + 1).min(len))
    } else {
        (0, len)
    }
}

/// Per-line selected character ranges `(line, char_from, char_to_exclusive)`
/// across the *entire* selection (which may span far more lines than are
/// currently visible on screen — e.g. after a drag-to-autoscroll). Used only
/// for extraction (`extract_text`), where touching every selected line is
/// unavoidable; never for painting the on-screen highlight (see
/// `highlight_cells`, which bounds itself to the visible window instead).
fn selection_ranges(start: TextPos, end: TextPos, wrap: &PanelWrap) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    for line in start.line..=end.line {
        if line >= wrap.line_count() {
            break;
        }
        let (from, to) = range_for_line(line, start, end, wrap);
        out.push((line, from, to));
    }
    out
}

/// Extract the selected text (lines joined with `\n`) between two logical
/// positions. Cost is proportional only to the selected lines themselves
/// (via `PanelWrap::line_text`'s O(1) slicing), never the panel's total
/// content size. `None` when there's nothing to select (no content, or a
/// purely blank selection). `exclude`, when given, drops any character
/// whose position is in the set — used to keep purely-visual annotations
/// (like the Request panel's shadow-warning icon; see
/// `TuiApp::main_shadow_icon_positions`) out of copied text even though
/// they're part of what's shown on screen.
pub fn extract_text(
    anchor: TextPos,
    cursor: TextPos,
    wrap: &PanelWrap,
    exclude: Option<&std::collections::HashSet<TextPos>>,
) -> Option<String> {
    if wrap.line_count() == 0 {
        return None;
    }
    let (start, end) = ordered(anchor, cursor);
    let ranges = selection_ranges(start, end, wrap);
    let mut out = String::new();
    for (i, (line, from, to)) in ranges.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let text = wrap.line_text(*line);
        let piece: String = text
            .chars()
            .enumerate()
            .skip(*from)
            .take(to.saturating_sub(*from))
            .filter(|(col, _)| !exclude.is_some_and(|ex| ex.contains(&TextPos::new(*line, *col))))
            .map(|(_, c)| c)
            .collect();
        out.push_str(&piece);
    }
    if out.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Strip every character at a position in `exclude` from `text` (used for
/// "copy the whole panel", which reads straight from `PanelWrap::source`
/// rather than going through `extract_text`'s per-line ranges). Rebuilds
/// line-by-line the same way `text`'s own trailing newline was originally
/// appended (see `draw::draw_collection_main`), so a whole-panel copy still
/// matches the underlying buffer exactly but for the excluded positions.
pub fn strip_positions(text: &str, exclude: &std::collections::HashSet<TextPos>) -> String {
    if exclude.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for (line_idx, line) in text.lines().enumerate() {
        if line_idx > 0 {
            out.push('\n');
        }
        for (col, ch) in line.chars().enumerate() {
            if !exclude.contains(&TextPos::new(line_idx, col)) {
                out.push(ch);
            }
        }
    }
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Convert the selection into absolute terminal cell ranges (row, col_from,
/// col_to_exclusive) suitable for painting a highlight, for whichever
/// (raw) lines the selection intersects the *current visible window*
/// (`scroll`..`scroll + area.height`). Bounded to that window — never the
/// whole selection — so highlighting stays cheap even when the selection
/// itself spans an enormous, mostly off-screen range.
pub fn highlight_cells(
    anchor: TextPos,
    cursor: TextPos,
    wrap: &PanelWrap,
    area: Rect,
    scroll: u16,
) -> Vec<(u16, u16, u16)> {
    if area.width == 0 || area.height == 0 || wrap.line_count() == 0 {
        return Vec::new();
    }
    let (start, end) = ordered(anchor, cursor);
    let first_visible = wrap.row_col_to_textpos(scroll as u32, 0).line;
    let last_visible_row = (scroll as u32 + area.height as u32).saturating_sub(1);
    let last_visible = wrap.row_col_to_textpos(last_visible_row, 0).line;
    let lo = start.line.max(first_visible);
    let hi = end.line.min(last_visible);
    if lo > hi {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in lo..=hi {
        if line >= wrap.line_count() {
            break;
        }
        let (from, to) = range_for_line(line, start, end, wrap);
        if from >= to {
            continue;
        }
        let len = wrap.line_char_len(line);
        let width = area.width as usize;
        let (base_row, _) = wrap.textpos_to_row_col(TextPos::new(line, 0));
        let rows_in_line = if width == 0 {
            1
        } else {
            len.div_ceil(width).max(1)
        };
        // Bound the row scan to this line's overlap with the *visible*
        // scroll window — never `0..rows_in_line` — so one enormous raw
        // line (thousands+ of wrapped rows) still costs only what's on
        // screen, exactly like `PanelWrap::visible_window` does.
        let window_lo = (scroll as u32).saturating_sub(base_row);
        let window_hi_excl =
            ((scroll as u32).saturating_add(area.height as u32)).saturating_sub(base_row);
        let r_lo = window_lo.min(rows_in_line as u32) as usize;
        let r_hi = window_hi_excl.min(rows_in_line as u32) as usize;
        for r in r_lo..r_hi {
            let row_start = r * width.max(1);
            let row_end = ((r + 1) * width.max(1)).min(len);
            let seg_from = from.max(row_start);
            let seg_to = to.min(row_end);
            if seg_from >= seg_to {
                continue;
            }
            let abs_row = base_row + r as u32;
            if abs_row < scroll as u32 {
                continue;
            }
            let local_row = abs_row - scroll as u32;
            if local_row >= area.height as u32 {
                continue;
            }
            out.push((
                area.y + local_row as u16,
                area.x + (seg_from - row_start) as u16,
                area.x + (seg_to - row_start) as u16,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn rect() -> Rect {
        Rect::new(2, 1, 20, 5) // x=2, y=1, width=20, height=5
    }

    fn wrap() -> PanelWrap {
        PanelWrap::build(
            Arc::from("first line here\nsecond\n\nfourth line of text\nfifth"),
            20,
        )
    }

    #[test]
    fn point_to_textpos_maps_terminal_coords_into_logical_positions() {
        let area = rect();
        let w = wrap();
        assert_eq!(
            point_to_textpos((2, 1), area, 0, &w),
            TextPos::new(0, 0),
            "top-left of the area"
        );
        // row 2 (local row 1) is "second"; col 5 (local col 3) lands inside it.
        assert_eq!(
            point_to_textpos((5, 2), area, 0, &w),
            TextPos::new(1, 3),
            "interior point offsets by area origin"
        );
        // Above/left of the area clamps to the nearest edge, not negative.
        assert_eq!(point_to_textpos((0, 0), area, 0, &w), TextPos::new(0, 0));
    }

    #[test]
    fn single_row_selection_takes_only_the_selected_columns() {
        let w = wrap();
        let text = extract_text(TextPos::new(0, 2), TextPos::new(0, 5), &w, None).unwrap();
        assert_eq!(text, "rst "); // chars 2..6 of "first line here"
    }

    #[test]
    fn multi_row_selection_takes_the_rest_of_the_first_line_full_middle_lines_and_the_start_of_the_last()
     {
        let w = wrap();
        let text = extract_text(TextPos::new(0, 6), TextPos::new(3, 5), &w, None).unwrap();
        assert_eq!(text, "line here\nsecond\n\nfourth");
    }

    #[test]
    fn dragging_backwards_still_resolves_to_the_same_selection() {
        let w = wrap();
        let forward = extract_text(TextPos::new(0, 2), TextPos::new(1, 4), &w, None).unwrap();
        let backward = extract_text(TextPos::new(1, 4), TextPos::new(0, 2), &w, None).unwrap();
        assert_eq!(forward, backward);
    }

    #[test]
    fn a_blank_or_empty_selection_extracts_to_none() {
        let w = wrap();
        // A click with no drag (anchor == cursor) on a single character still
        // yields that one character, but a selection entirely inside the
        // blank line yields None.
        assert_eq!(
            extract_text(TextPos::new(2, 0), TextPos::new(2, 0), &w, None),
            None
        );
        let empty = PanelWrap::build(Arc::from(""), 20);
        assert_eq!(
            extract_text(TextPos::new(0, 0), TextPos::new(0, 0), &empty, None),
            None,
            "no content"
        );
    }

    #[test]
    fn extract_text_excludes_only_the_positions_given() {
        let w = wrap();
        let mut exclude = std::collections::HashSet::new();
        // "first line here" — drop the 'f' (col 0) but keep everything else.
        exclude.insert(TextPos::new(0, 0));
        let text =
            extract_text(TextPos::new(0, 0), TextPos::new(0, 5), &w, Some(&exclude)).unwrap();
        assert_eq!(
            text, "irst ",
            "the excluded column is dropped, all others are kept"
        );
    }

    #[test]
    fn strip_positions_removes_only_excluded_characters() {
        let mut exclude = std::collections::HashSet::new();
        exclude.insert(TextPos::new(0, 5)); // the '!' in "hello!world"
        let out = strip_positions("hello!world\nsecond!line", &exclude);
        assert_eq!(
            out, "helloworld\nsecond!line",
            "only the recorded position is stripped, other lines untouched"
        );
    }

    #[test]
    fn strip_positions_is_a_no_op_with_an_empty_exclude_set() {
        let exclude = std::collections::HashSet::new();
        let out = strip_positions("unchanged!text\n", &exclude);
        assert_eq!(out, "unchanged!text\n");
    }

    #[test]
    fn highlight_cells_skip_empty_rows_and_report_absolute_terminal_columns() {
        let w = wrap();
        let area = rect();
        let cells = highlight_cells(TextPos::new(0, 6), TextPos::new(3, 5), &w, area, 0);
        // The blank middle row (row 2) contributes nothing to highlight.
        assert_eq!(
            cells,
            vec![
                (area.y, area.x + 6, area.x + 15),
                (area.y + 1, area.x, area.x + 6),
                (area.y + 3, area.x, area.x + 6),
            ]
        );
    }

    #[test]
    fn highlight_cells_only_scans_lines_intersecting_the_visible_window() {
        // A huge body with a selection spanning nearly all of it: the
        // highlight scan must still return promptly and only report rows
        // actually within the visible scroll window.
        let body: String = (0..100_000).map(|i| format!("line {i}\n")).collect();
        let w = PanelWrap::build(Arc::from(body), 20);
        let area = Rect::new(0, 0, 20, 5);
        let cells = highlight_cells(
            TextPos::new(0, 0),
            TextPos::new(99_999, 3),
            &w,
            area,
            50_000,
        );
        assert_eq!(
            cells.len(),
            5,
            "exactly the 5 visible rows, not the whole selected range"
        );
        assert_eq!(cells[0].0, 0);
        assert_eq!(cells[4].0, 4);
    }

    #[test]
    fn highlight_cells_handles_a_selection_wholly_off_screen() {
        let w = wrap();
        let area = rect();
        // Selection entirely above the current scroll window.
        let cells = highlight_cells(TextPos::new(0, 0), TextPos::new(0, 3), &w, area, 10);
        assert!(cells.is_empty());
    }

    /// Regression test: a *single* raw line that itself wraps into thousands
    /// of rows (e.g. one enormous unbroken line in a huge response) used to
    /// make `highlight_cells` iterate `0..rows_in_line` for that line —
    /// tens of thousands of iterations every redraw regardless of how much
    /// of it was actually on screen. The scan must be bounded to the rows
    /// that intersect the visible window, exactly like `visible_window`.
    #[test]
    fn highlight_cells_bounds_the_scan_even_when_one_line_has_thousands_of_wrapped_rows() {
        let body: String = "x".repeat(500_000); // one line, 500_000 / 20 = 25_000 wrapped rows
        let w = PanelWrap::build(Arc::from(body), 20);
        let area = Rect::new(0, 0, 20, 5);
        // Select the whole line, but scroll deep into its middle.
        let cells = highlight_cells(
            TextPos::new(0, 0),
            TextPos::new(0, 499_999),
            &w,
            area,
            12_000,
        );
        assert_eq!(
            cells.len(),
            5,
            "exactly the 5 visible rows of this one giant line, not all 25,000"
        );
        assert_eq!(cells[0].0, area.y);
        assert_eq!(cells[4].0, area.y + 4);
        // Every reported row should be a full-width row (the whole line is selected).
        for &(_, from, to) in &cells {
            assert_eq!(
                to - from,
                area.width,
                "each visible row of a fully-selected giant line is fully highlighted"
            );
        }
    }
}
