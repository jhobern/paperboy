//! A small single- or multi-line text editor primitive for [ratatui] apps.
//!
//! [`Editor`] holds the edited text as a `Vec<String>` of logical lines plus a
//! `(row, col)` cursor and an optional selection anchor. It exposes granular
//! mutators (`insert`, `backspace`, `left`/`right`/`up`/`down`, `home`/`end`,
//! `newline`, …) so a host application can wire its own key handling and
//! interleave editing with app-level logic, rather than delegating a whole
//! event stream to the widget. An optional batteries-included
//! [`apply_edit_key`] covers the common single-line case.
//!
//! Rendering is separate and fully styleable via [`EditorTheme`]:
//! [`render_editor`] draws a scrolling (multi-line) view that follows the
//! cursor and highlights any selection, while [`render_line_field`] draws a
//! compact single-line field. Both can mask every character (for secrets).
//!
//! The editor is deliberately unopinionated about its frame: it renders its
//! *contents* into whatever [`Rect`] you give it, so the host keeps control of
//! the surrounding block, title and layout.
//!
//! [ratatui]: https://docs.rs/ratatui
//!
//! # Example
//!
//! ```
//! use tui_line_editor::Editor;
//!
//! let mut ed = Editor::new("hello", false);
//! ed.home();
//! ed.insert('>');
//! ed.insert(' ');
//! assert_eq!(ed.text(), "> hello");
//! ```

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

/// Colours used when rendering an [`Editor`]. Build one from your application's
/// own theme and pass it to [`render_editor`] / [`render_line_field`].
#[derive(Clone, Copy, Debug)]
pub struct EditorTheme {
    /// Foreground colour of the edited text.
    pub text: Color,
    /// Background colour of a focused single-line field ([`render_line_field`]).
    pub panel: Color,
    /// Foreground colour of an unfocused single-line field ([`render_line_field`]).
    pub dim: Color,
    /// Foreground colour of selected text ([`render_editor`]).
    pub select_fg: Color,
    /// Background colour of selected text ([`render_editor`]).
    pub select_bg: Color,
}

/// A single- or multi-line text buffer with a cursor and optional selection.
pub struct Editor {
    /// The logical lines of text (never empty — always at least one line).
    pub lines: Vec<String>,
    /// Cursor row (index into [`lines`](Self::lines)).
    pub row: usize,
    /// Cursor column, measured in characters (not bytes) within the row.
    pub col: usize,
    /// Whether newlines are accepted (multi-line) or ignored (single-line).
    pub multiline: bool,
    /// The *other* end of an active text selection (row, char-col),
    /// anchored the moment the user first holds Shift while moving the
    /// cursor; `None` means no selection. The current `(row, col)` is
    /// always the selection's live end. Only ever set by Shift+Arrow
    /// handling in a multi-line editor — plain movement clears it.
    pub sel_anchor: Option<(usize, usize)>,
}

impl Editor {
    /// Create an editor holding `text`, with the cursor at the end. `multiline`
    /// controls whether [`newline`](Self::newline) inserts line breaks.
    pub fn new(text: &str, multiline: bool) -> Self {
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(|s| s.to_string()).collect()
        };
        let row = lines.len() - 1;
        let col = lines[row].chars().count();
        Self {
            lines,
            row,
            col,
            multiline,
            sel_anchor: None,
        }
    }

    /// An empty single-line editor — the common case for form cells.
    pub fn blank() -> Self {
        Self::new("", false)
    }

    /// The full text, with logical lines joined by `\n`.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// The byte index within `line` of the character at `char_col` (or the
    /// line's byte length if `char_col` is past the end).
    pub fn byte_idx(line: &str, char_col: usize) -> usize {
        line.char_indices()
            .nth(char_col)
            .map(|(i, _)| i)
            .unwrap_or(line.len())
    }

    /// The number of characters on `row`.
    pub fn line_len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    /// Insert `ch` at the cursor and advance one column.
    pub fn insert(&mut self, ch: char) {
        let idx = Self::byte_idx(&self.lines[self.row], self.col);
        self.lines[self.row].insert(idx, ch);
        self.col += 1;
    }

    /// Insert every character of `s` at the cursor, in order (used to
    /// autocomplete a ghost suffix). `s` is expected to be single-line.
    pub fn insert_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.insert(ch);
        }
    }

    /// Split the current line at the cursor (no-op in single-line mode).
    pub fn newline(&mut self) {
        if !self.multiline {
            return;
        }
        let idx = Self::byte_idx(&self.lines[self.row], self.col);
        let tail = self.lines[self.row].split_off(idx);
        self.lines.insert(self.row + 1, tail);
        self.row += 1;
        self.col = 0;
    }

    /// Delete the character before the cursor, joining with the previous line
    /// when at column 0.
    pub fn backspace(&mut self) {
        if self.col > 0 {
            let start = Self::byte_idx(&self.lines[self.row], self.col - 1);
            let end = Self::byte_idx(&self.lines[self.row], self.col);
            self.lines[self.row].replace_range(start..end, "");
            self.col -= 1;
        } else if self.row > 0 {
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.line_len(self.row);
            self.lines[self.row].push_str(&cur);
        }
    }

    /// Move the cursor one character left (wrapping to the previous line end).
    pub fn left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.line_len(self.row);
        }
    }

    /// Move the cursor one character right (wrapping to the next line start).
    pub fn right(&mut self) {
        if self.col < self.line_len(self.row) {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    /// Move the cursor up one line, clamping the column to the new line length.
    pub fn up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.col = self.col.min(self.line_len(self.row));
        }
    }

    /// Move the cursor down one line, clamping the column to the new line length.
    pub fn down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.col.min(self.line_len(self.row));
        }
    }

    /// Move the cursor to column 0 of the current line.
    pub fn home(&mut self) {
        self.col = 0;
    }

    /// Move the cursor to the end of the current line.
    pub fn end(&mut self) {
        self.col = self.line_len(self.row);
    }

    /// Anchor a selection at the current cursor position, if one isn't
    /// already active — called once, right before the first Shift+Arrow
    /// move extends it.
    pub fn begin_selection_if_needed(&mut self) {
        if self.sel_anchor.is_none() {
            self.sel_anchor = Some((self.row, self.col));
        }
    }

    /// Prepare for a cursor move: when `extend` (Shift held) start/keep a
    /// selection, otherwise drop any existing one. Call right before the move.
    pub fn set_selecting(&mut self, extend: bool) {
        if extend {
            self.begin_selection_if_needed();
        } else {
            self.clear_selection();
        }
    }

    /// Drop any active selection.
    pub fn clear_selection(&mut self) {
        self.sel_anchor = None;
    }

    /// The selection's two endpoints in text order (`(row, col)`), or
    /// `None` if there's no active selection or it's collapsed to a single
    /// point (anchor == cursor).
    pub fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.sel_anchor?;
        let cursor = (self.row, self.col);
        if anchor == cursor {
            return None;
        }
        Some(if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        })
    }

    /// The selected text, using ordinary "stream" semantics: the first line
    /// runs from its start column to its own end, the last from column 0 to
    /// its end column, and every line strictly in between is taken in full.
    pub fn selected_text(&self) -> Option<String> {
        let ((sr, sc), (er, ec)) = self.selection_range()?;
        if sr == er {
            return Some(self.lines[sr].chars().skip(sc).take(ec - sc).collect());
        }
        let mut out = String::new();
        for row in sr..=er {
            if row > sr {
                out.push('\n');
            }
            let len = self.line_len(row);
            let (from, to) = if row == sr {
                (sc, len)
            } else if row == er {
                (0, ec)
            } else {
                (0, len)
            };
            out.extend(
                self.lines[row]
                    .chars()
                    .skip(from)
                    .take(to.saturating_sub(from)),
            );
        }
        Some(out)
    }

    /// Map a mouse point (terminal screen space) to the (row, col) text
    /// position it corresponds to, given the exact `area` [`render_editor`]
    /// last drew this editor into. Deliberately *not* clamped to the
    /// visible window's rows/columns: [`render_editor`]'s viewport always
    /// follows the cursor (`row_off`/`col_off` are recomputed from `row`/
    /// `col` every frame), so mapping a drag past the edge to a row/col
    /// outside the current window still naturally scrolls the editor to
    /// reveal it on the very next frame — no separate auto-scroll tick
    /// needed here.
    pub fn point_to_row_col(&self, point: (u16, u16), area: Rect) -> (usize, usize) {
        let h = area.height as usize;
        let w = (area.width as usize).max(1);
        let row_off = self.row.saturating_sub(h.saturating_sub(1));
        let col_off = self.col.saturating_sub(w.saturating_sub(1));
        let dy = point.1 as i64 - area.y as i64;
        let last_row = self.lines.len().saturating_sub(1);
        let row = (row_off as i64 + dy).clamp(0, last_row as i64) as usize;
        let dx = point.0 as i64 - area.x as i64;
        let col = (col_off as i64 + dx).max(0) as usize;
        (row, col.min(self.line_len(row)))
    }
}

/// Apply a single-line editing key to `ed` (Ctrl+←/→ jump to start/end).
pub fn apply_edit_key(ed: &mut Editor, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(c) => ed.insert(c),
        KeyCode::Backspace => ed.backspace(),
        KeyCode::Left if ctrl => ed.home(),
        KeyCode::Right if ctrl => ed.end(),
        KeyCode::Left => ed.left(),
        KeyCode::Right => ed.right(),
        KeyCode::Home => ed.home(),
        KeyCode::End => ed.end(),
        _ => {}
    }
}

/// Render a (possibly multi-line) editor into `area`, scrolling so the cursor
/// stays visible, highlighting any selection, and placing the terminal cursor.
/// When `masked`, every character is drawn as `•` (for secrets).
pub fn render_editor(f: &mut Frame, area: Rect, ed: &Editor, style: &EditorTheme, masked: bool) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let h = area.height as usize;
    let w = area.width as usize;
    let row_off = ed.row.saturating_sub(h - 1);
    let col_off = ed.col.saturating_sub(w - 1);
    let lines: Vec<Line> = ed
        .lines
        .iter()
        .skip(row_off)
        .take(h)
        .map(|l| {
            let visible = l.chars().skip(col_off).take(w);
            let text: String = if masked {
                // Mask each character so a secret is never shown while editing.
                visible.map(|_| '\u{2022}').collect()
            } else {
                visible.collect()
            };
            Line::from(text)
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).style(Style::default().fg(style.text)),
        area,
    );
    if let Some(((sr, sc), (er, ec))) = ed.selection_range() {
        let buf = f.buffer_mut();
        for screen_row in 0..h {
            let line_idx = row_off + screen_row;
            if line_idx >= ed.lines.len() || line_idx < sr || line_idx > er {
                continue;
            }
            let len = ed.line_len(line_idx);
            let (from, to) = if sr == er {
                (sc, ec)
            } else if line_idx == sr {
                (sc, len)
            } else if line_idx == er {
                (0, ec)
            } else {
                (0, len)
            };
            for col in from.max(col_off)..to.min(col_off + w) {
                let screen_col = col - col_off;
                if let Some(cell) =
                    buf.cell_mut((area.x + screen_col as u16, area.y + screen_row as u16))
                {
                    cell.set_style(Style::default().bg(style.select_bg).fg(style.select_fg));
                }
            }
        }
    }
    let cx = area.x + (ed.col - col_off) as u16;
    let cy = area.y + (ed.row - row_off) as u16;
    f.set_cursor_position(Position::new(cx, cy));
}

/// Render a single-line editor's text into `area`, masking every character with
/// `•` when `mask` is set. Places the terminal cursor when `focused`.
pub fn render_line_field(
    f: &mut Frame,
    area: Rect,
    ed: &Editor,
    style: &EditorTheme,
    focused: bool,
    mask: bool,
) {
    if area.width == 0 {
        return;
    }
    let w = area.width as usize;
    let text = ed.text();
    let shown: String = if mask {
        "\u{2022}".repeat(text.chars().count())
    } else {
        text
    };
    let col_off = ed.col.saturating_sub(w.saturating_sub(1));
    let vis: String = shown.chars().skip(col_off).take(w).collect();
    let cell_style = if focused {
        Style::default().fg(style.text).bg(style.panel)
    } else {
        Style::default().fg(style.dim)
    };
    f.render_widget(Paragraph::new(vis).style(cell_style), area);
    if focused {
        f.set_cursor_position(Position::new(area.x + (ed.col - col_off) as u16, area.y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_places_cursor_at_end_and_splits_lines() {
        let ed = Editor::new("ab\ncd", true);
        assert_eq!(ed.lines, vec!["ab".to_string(), "cd".to_string()]);
        assert_eq!((ed.row, ed.col), (1, 2));
        assert_eq!(ed.text(), "ab\ncd");
    }

    #[test]
    fn blank_is_one_empty_single_line() {
        let ed = Editor::blank();
        assert_eq!(ed.lines, vec![String::new()]);
        assert!(!ed.multiline);
        assert_eq!(ed.text(), "");
    }

    #[test]
    fn insert_and_backspace_track_the_cursor() {
        let mut ed = Editor::blank();
        ed.insert('h');
        ed.insert('i');
        assert_eq!(ed.text(), "hi");
        ed.backspace();
        assert_eq!(ed.text(), "h");
        assert_eq!(ed.col, 1);
    }

    #[test]
    fn insert_str_types_each_character() {
        let mut ed = Editor::blank();
        ed.insert_str("hello");
        assert_eq!(ed.text(), "hello");
        assert_eq!(ed.col, 5);
    }

    #[test]
    fn newline_only_splits_in_multiline_mode() {
        let mut single = Editor::new("ab", false);
        single.home();
        single.newline();
        assert_eq!(single.text(), "ab");

        let mut multi = Editor::new("ab", true);
        multi.home();
        multi.right();
        multi.newline();
        assert_eq!(multi.text(), "a\nb");
        assert_eq!((multi.row, multi.col), (1, 0));
    }

    #[test]
    fn backspace_at_column_zero_joins_lines() {
        let mut ed = Editor::new("ab\ncd", true);
        ed.row = 1;
        ed.col = 0;
        ed.backspace();
        assert_eq!(ed.text(), "abcd");
        assert_eq!((ed.row, ed.col), (0, 2));
    }

    #[test]
    fn movement_wraps_across_lines() {
        let mut ed = Editor::new("ab\ncd", true);
        ed.row = 1;
        ed.col = 0;
        ed.left();
        assert_eq!((ed.row, ed.col), (0, 2));
        ed.right();
        assert_eq!((ed.row, ed.col), (1, 0));
    }

    #[test]
    fn home_and_end_jump_within_the_line() {
        let mut ed = Editor::new("hello", false);
        ed.home();
        assert_eq!(ed.col, 0);
        ed.end();
        assert_eq!(ed.col, 5);
    }

    #[test]
    fn single_line_selection_extracts_the_covered_run() {
        let mut ed = Editor::new("hello world", false);
        ed.row = 0;
        ed.col = 0;
        ed.begin_selection_if_needed();
        ed.col = 5;
        assert_eq!(ed.selected_text().as_deref(), Some("hello"));
    }

    #[test]
    fn multi_line_selection_joins_with_newlines() {
        let mut ed = Editor::new("first\nsecond\nthird", true);
        ed.row = 0;
        ed.col = 3;
        ed.begin_selection_if_needed();
        ed.row = 2;
        ed.col = 2;
        assert_eq!(ed.selected_text().as_deref(), Some("st\nsecond\nth"));
    }

    #[test]
    fn collapsed_selection_yields_none() {
        let mut ed = Editor::new("hello", false);
        ed.begin_selection_if_needed();
        assert_eq!(ed.selection_range(), None);
        assert_eq!(ed.selected_text(), None);
    }

    #[test]
    fn set_selecting_starts_and_clears() {
        let mut ed = Editor::new("hello", false);
        ed.set_selecting(true);
        assert!(ed.sel_anchor.is_some());
        ed.set_selecting(false);
        assert!(ed.sel_anchor.is_none());
    }

    #[test]
    fn apply_edit_key_handles_typing_and_navigation() {
        let mut ed = Editor::new("hi", false);
        apply_edit_key(
            &mut ed,
            KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
        );
        assert_eq!(ed.text(), "hi!");
        apply_edit_key(&mut ed, KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(ed.col, 0);
        apply_edit_key(
            &mut ed,
            KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL),
        );
        assert_eq!(ed.col, 3);
        apply_edit_key(
            &mut ed,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert_eq!(ed.text(), "hi");
    }

    #[test]
    fn point_to_row_col_maps_screen_space_back_to_text() {
        let ed = Editor::new("first\nsecond\nthird", true);
        // Small area anchored at (0, 0); cursor at end so viewport shows all.
        let area = Rect::new(0, 0, 40, 3);
        assert_eq!(ed.point_to_row_col((3, 0), area), (0, 3));
        assert_eq!(ed.point_to_row_col((100, 1), area), (1, 6)); // clamps to line end
    }
}
