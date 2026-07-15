use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::theme::*;

pub(crate) struct Editor {
    pub(crate) lines: Vec<String>,
    pub(crate) row: usize,
    pub(crate) col: usize,
    pub(crate) multiline: bool,
    /// The *other* end of an active text selection (row, char-col),
    /// anchored the moment the user first holds Shift while moving the
    /// cursor; `None` means no selection. The current `(row, col)` is
    /// always the selection's live end. Only ever set by Shift+Arrow
    /// handling in the Raw (Hurl Mode) editor — plain movement clears it.
    pub(crate) sel_anchor: Option<(usize, usize)>,
}

impl Editor {
    pub(crate) fn new(text: &str, multiline: bool) -> Self {
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

    pub(crate) fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub(crate) fn byte_idx(line: &str, char_col: usize) -> usize {
        line.char_indices()
            .nth(char_col)
            .map(|(i, _)| i)
            .unwrap_or(line.len())
    }

    pub(crate) fn line_len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    pub(crate) fn insert(&mut self, ch: char) {
        let idx = Self::byte_idx(&self.lines[self.row], self.col);
        self.lines[self.row].insert(idx, ch);
        self.col += 1;
    }

    /// Insert every character of `s` at the cursor, in order (used to
    /// autocomplete a ghost suffix). `s` is expected to be single-line.
    pub(crate) fn insert_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.insert(ch);
        }
    }

    pub(crate) fn newline(&mut self) {
        if !self.multiline {
            return;
        }
        let idx = Self::byte_idx(&self.lines[self.row], self.col);
        let tail = self.lines[self.row].split_off(idx);
        self.lines.insert(self.row + 1, tail);
        self.row += 1;
        self.col = 0;
    }

    pub(crate) fn backspace(&mut self) {
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

    pub(crate) fn left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.line_len(self.row);
        }
    }

    pub(crate) fn right(&mut self) {
        if self.col < self.line_len(self.row) {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    pub(crate) fn up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.col = self.col.min(self.line_len(self.row));
        }
    }

    pub(crate) fn down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.col.min(self.line_len(self.row));
        }
    }

    pub(crate) fn home(&mut self) {
        self.col = 0;
    }

    pub(crate) fn end(&mut self) {
        self.col = self.line_len(self.row);
    }

    /// Anchor a selection at the current cursor position, if one isn't
    /// already active — called once, right before the first Shift+Arrow
    /// move extends it.
    pub(crate) fn begin_selection_if_needed(&mut self) {
        if self.sel_anchor.is_none() {
            self.sel_anchor = Some((self.row, self.col));
        }
    }

    /// Prepare for a cursor move: when `extend` (Shift held) start/keep a
    /// selection, otherwise drop any existing one. Call right before the move.
    pub(crate) fn set_selecting(&mut self, extend: bool) {
        if extend {
            self.begin_selection_if_needed();
        } else {
            self.clear_selection();
        }
    }

    pub(crate) fn clear_selection(&mut self) {
        self.sel_anchor = None;
    }

    /// The selection's two endpoints in text order (`(row, col)`), or
    /// `None` if there's no active selection or it's collapsed to a single
    /// point (anchor == cursor).
    pub(crate) fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
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
    pub(crate) fn selected_text(&self) -> Option<String> {
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
    /// visible window's rows/columns: `render_editor`'s viewport always
    /// follows the cursor (`row_off`/`col_off` are recomputed from `row`/
    /// `col` every frame, unlike the Main/Response panels' independent
    /// scroll state), so mapping a drag past the edge to a row/col outside
    /// the current window still naturally scrolls the editor to reveal it
    /// on the very next frame — no separate auto-scroll tick needed here.
    pub(crate) fn point_to_row_col(&self, point: (u16, u16), area: Rect) -> (usize, usize) {
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
pub(crate) fn apply_edit_key(ed: &mut Editor, key: KeyEvent) {
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

pub(crate) fn render_editor(f: &mut Frame, area: Rect, ed: &Editor, masked: bool, th: &Theme) {
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
        Paragraph::new(lines).style(Style::default().fg(th.text)),
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
                    // Match the app's own selection highlight elsewhere
                    // (Request JSON / Response panels) instead of
                    // `Modifier::REVERSED`, which just swapped whatever
                    // fg/bg the terminal happened to be using — usually
                    // reading as plain (often white) reverse video, easy to
                    // mistake for the terminal's own native selection.
                    cell.set_style(Style::default().bg(th.select_bg).fg(th.select_fg));
                }
            }
        }
    }
    let cx = area.x + (ed.col - col_off) as u16;
    let cy = area.y + (ed.row - row_off) as u16;
    f.set_cursor_position(Position::new(cx, cy));
}

/// Render a single-line editor's text into `area`, masking every character with
/// `•` when `mask` is set (used for the access token). Places the cursor when
/// focused.
pub(crate) fn render_line_field(
    f: &mut Frame,
    area: Rect,
    ed: &Editor,
    focused: bool,
    mask: bool,
    th: &Theme,
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
    let style = if focused {
        Style::default().fg(th.text).bg(th.panel)
    } else {
        Style::default().fg(th.dim)
    };
    f.render_widget(Paragraph::new(vis).style(style), area);
    if focused {
        f.set_cursor_position(Position::new(area.x + (ed.col - col_off) as u16, area.y));
    }
}
