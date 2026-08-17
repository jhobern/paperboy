//! A small single- or multi-line text editor primitive for [ratatui] apps.
//!
//! [`Editor`] holds the edited text as a `Vec<String>` of logical lines plus a
//! `(row, col)` cursor and an optional selection anchor. It exposes granular
//! mutators (`insert`, `backspace`, `left`/`right`/`up`/`down`, `home`/`end`,
//! `newline`, …) so a host application can wire its own key handling and
//! interleave editing with app-level logic, rather than delegating a whole
//! event stream to the widget. Two batteries-included key handlers cover the
//! common cases: [`apply_edit_key`] for a single-line field, and
//! [`apply_edit_key_full`] for a selection-aware multi-line pane (returning an
//! [`EditResponse`] describing what it changed / copied).
//!
//! Rendering is separate and fully styleable via [`EditorTheme`]:
//! [`render_editor`] draws a scrolling (multi-line) view that follows the
//! cursor and highlights any selection, [`render_editor_highlighted`] does the
//! same from caller-supplied styled spans (for live syntax highlighting), and
//! [`render_line_field`] draws a compact single-line field. Both plain renders
//! can mask every character (for secrets).
//! [`render_clipped_line`] renders a read-only, host-coloured line and shows a
//! [`TruncationMarker`] (a dim `…` by default) when the text is cut off.
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
//! use crate::tui::line_editor::Editor;
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
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
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

/// Optional glyph drawn in the last column of a truncated single line
/// ([`render_clipped_line`]) to indicate the text is wider than the area and
/// has been cut off. Mirrors the wrap-marker concept: start from
/// [`TruncationMarker::default`] (a dim ellipsis `…`) and override the
/// [`glyph`](Self::glyph) / [`style`](Self::style) to taste.
#[derive(Clone, Copy, Debug)]
pub struct TruncationMarker {
    /// The glyph drawn in the reserved last column (e.g. an ellipsis `…`).
    /// Must be a single terminal cell wide.
    pub glyph: char,
    /// The style the glyph is drawn with — typically dim so it reads as an
    /// annotation rather than content.
    pub style: Style,
}

impl Default for TruncationMarker {
    /// A dim ellipsis (`…`) — the conventional "there is more text" indicator.
    fn default() -> Self {
        Self {
            glyph: '\u{2026}',
            style: Style::default().add_modifier(Modifier::DIM),
        }
    }
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
    /// Past buffer states, most-recent last — popped by [`undo`](Self::undo).
    /// A checkpoint is pushed *before* a mutation, so undoing restores the
    /// state as it was just before that edit.
    undo_stack: Vec<Snapshot>,
    /// States that were undone, so [`redo`](Self::redo) can replay them.
    /// Cleared whenever a fresh edit is recorded.
    redo_stack: Vec<Snapshot>,
    /// The kind of the edit currently being coalesced into a single undo step
    /// (so typing a run of characters — or holding Backspace — is one undo,
    /// not one per keystroke); `None` between runs.
    coalesce: Option<EditKind>,
}

/// A snapshot of the editable state used for undo/redo. Selection is not
/// captured (an undo/redo always lands with no active selection).
#[derive(Clone)]
struct Snapshot {
    lines: Vec<String>,
    row: usize,
    col: usize,
}

/// The category of a text-mutating edit, used to coalesce consecutive edits of
/// the same kind into one undo step.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    /// Character insertion (typing).
    Insert,
    /// Single-character deletion (Backspace/Delete).
    Delete,
}

/// The most undo steps retained; older ones are dropped so a long session
/// can't grow the history without bound.
const UNDO_LIMIT: usize = 256;

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
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            coalesce: None,
        }
    }

    /// Move the cursor to `(row, col)`, clamped to the current text (`row` to
    /// the last line, `col` to that line's character length), dropping any
    /// selection. Used to restore a remembered caret when an editor is
    /// re-opened over the same (or edited) text.
    pub fn set_cursor(&mut self, row: usize, col: usize) {
        self.row = row.min(self.lines.len().saturating_sub(1));
        self.col = col.min(self.lines[self.row].chars().count());
        self.sel_anchor = None;
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

    /// Delete the word — or a whole `"…"` quoted token — immediately to the
    /// left of the cursor (the Ctrl+Backspace motion). At column 0 this joins
    /// with the previous line, exactly like [`backspace`](Self::backspace).
    ///
    /// When the character just left of the cursor is a closing `"` that has a
    /// matching opening `"` earlier on the same line, the entire quoted span
    /// (quotes included) is removed in one step — so a quoted name like
    /// `"Upload document"` deletes whole. Otherwise it removes any run of
    /// whitespace directly left of the cursor and then the preceding run of
    /// non-whitespace characters, mirroring a Ctrl+← word jump.
    pub fn delete_word_left(&mut self) {
        if self.col == 0 {
            self.backspace();
            return;
        }
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        // Quoted-token case: the cursor sits right after a closing `"`.
        if chars[self.col - 1] == '"'
            && let Some(open) = chars[..self.col - 1].iter().rposition(|&c| c == '"')
        {
            self.delete_char_range(open, self.col);
            return;
        }
        let mut c = self.col;
        while c > 0 && chars[c - 1].is_whitespace() {
            c -= 1;
        }
        while c > 0 && !chars[c - 1].is_whitespace() {
            c -= 1;
        }
        self.delete_char_range(c, self.col);
    }

    /// Remove the characters in the half-open char-column range `[from, to)` on
    /// the current row and leave the cursor at `from`.
    fn delete_char_range(&mut self, from: usize, to: usize) {
        let start = Self::byte_idx(&self.lines[self.row], from);
        let end = Self::byte_idx(&self.lines[self.row], to);
        self.lines[self.row].replace_range(start..end, "");
        self.col = from;
    }

    /// Record the current buffer state as a discrete undo checkpoint, ending
    /// any in-progress coalesced run. Call this *before* a programmatic edit
    /// (e.g. accepting an autocompletion or inserting an auto-indent) so that
    /// edit becomes its own, cleanly reversible undo step.
    pub fn checkpoint(&mut self) {
        self.push_undo();
        self.coalesce = None;
    }

    /// Undo the most recent edit (or coalesced run of edits), returning whether
    /// anything changed. The undone state is kept so [`redo`](Self::redo) can
    /// replay it.
    pub fn undo(&mut self) -> bool {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.snapshot());
            self.restore(prev);
            self.coalesce = None;
            true
        } else {
            false
        }
    }

    /// Redo the most recently undone edit, returning whether anything changed.
    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.snapshot());
            self.restore(next);
            self.coalesce = None;
            true
        } else {
            false
        }
    }

    /// Note that an edit of `kind` is about to happen, pushing an undo
    /// checkpoint only when it *starts* a new run (a different kind from the
    /// one currently coalescing), so a run of same-kind edits collapses into a
    /// single undo step.
    fn record_edit(&mut self, kind: EditKind) {
        if self.coalesce != Some(kind) {
            self.push_undo();
        }
        self.coalesce = Some(kind);
    }

    /// End any coalesced edit run without recording a checkpoint, so the next
    /// edit starts a fresh undo step. Called on cursor movement.
    fn break_run(&mut self) {
        self.coalesce = None;
    }

    /// Capture the current editable state.
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            row: self.row,
            col: self.col,
        }
    }

    /// Push the current state onto the undo stack (capping its length) and
    /// clear the redo stack — any fresh edit invalidates a pending redo.
    fn push_undo(&mut self) {
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > UNDO_LIMIT {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    /// Restore a captured state, clamping the cursor into the restored text and
    /// dropping any selection.
    fn restore(&mut self, s: Snapshot) {
        self.lines = s.lines;
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.row = s.row.min(self.lines.len() - 1);
        self.col = s.col.min(self.line_len(self.row));
        self.sel_anchor = None;
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

/// Apply a single-line editing key to `ed` (Ctrl+←/→ jump to start/end,
/// Ctrl+Backspace — or its legacy-terminal alias Ctrl+H — deletes the previous
/// word, Ctrl+Z undoes).
pub fn apply_edit_key(ed: &mut Editor, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('z') | KeyCode::Char('Z') if ctrl => {
            ed.undo();
        }
        // Ctrl+Backspace on terminals *without* the keyboard-enhancement
        // protocol never arrives as `Backspace`+CONTROL: the terminal sends a
        // bare BS byte (0x08), which crossterm decodes as Ctrl+H
        // (`Char('h')`+CONTROL). Treat that identically to Ctrl+Backspace so
        // word-delete works on legacy terminals too (on enhanced terminals a
        // real Ctrl+H is rare and word-delete is a sensible binding for it).
        KeyCode::Char('h') | KeyCode::Char('H') if ctrl => {
            ed.checkpoint();
            ed.delete_word_left();
        }
        KeyCode::Char(c) => {
            ed.record_edit(EditKind::Insert);
            ed.insert(c);
        }
        KeyCode::Backspace if ctrl => {
            ed.checkpoint();
            ed.delete_word_left();
        }
        KeyCode::Backspace => {
            ed.record_edit(EditKind::Delete);
            ed.backspace();
        }
        KeyCode::Left if ctrl => {
            ed.break_run();
            ed.home()
        }
        KeyCode::Right if ctrl => {
            ed.break_run();
            ed.end()
        }
        KeyCode::Left => {
            ed.break_run();
            ed.left()
        }
        KeyCode::Right => {
            ed.break_run();
            ed.right()
        }
        KeyCode::Home => {
            ed.break_run();
            ed.home()
        }
        KeyCode::End => {
            ed.break_run();
            ed.end()
        }
        _ => {}
    }
}

/// What an editing key handled by [`apply_edit_key_full`] did, so the host can
/// react without re-inspecting the key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditResponse {
    /// The key modified the editor's text (the host may want to re-validate,
    /// mark the buffer dirty, resize, …).
    pub changed: bool,
    /// Ctrl+Y copied the active selection: the editor keeps no clipboard of its
    /// own, so the host should place this text on *its* clipboard.
    pub copy: Option<String>,
}

/// Apply one key to a (possibly multi-line) `ed`, covering the full
/// selection-aware editing surface a plain text pane needs: typing, Backspace,
/// Ctrl+Backspace (delete the previous word / a whole `"…"` token — its legacy
/// alias Ctrl+H does the same), Enter (a
/// newline only in a multi-line editor), arrow movement with Shift-to-select
/// and Ctrl+←/→ jump-to-edge, Home/End, Ctrl+Y to copy the selection, and
/// Ctrl+Z / Ctrl+Shift+Z to undo / redo. Unlike the single-line
/// [`apply_edit_key`], it reports its effect via [`EditResponse`] so a host can
/// revalidate on change and own the clipboard.
///
/// Keys the editor doesn't own — Esc, a commit/submit key, application
/// shortcuts, and Ctrl-modified character keys other than the ones above — are
/// left untouched and yield a default (`changed == false`, `copy == None`)
/// [`EditResponse`], so the host can still handle them itself.
pub fn apply_edit_key_full(ed: &mut Editor, key: KeyEvent) -> EditResponse {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let mut resp = EditResponse::default();
    match key.code {
        // Copy first so a bare 'y' still types normally below.
        KeyCode::Char('y') if ctrl => resp.copy = ed.selected_text(),
        // Ctrl+Shift+Z redoes; Ctrl+Z undoes. Both report `changed` so the
        // host revalidates.
        KeyCode::Char('z') | KeyCode::Char('Z') if ctrl && shift => {
            resp.changed = ed.redo();
        }
        KeyCode::Char('z') | KeyCode::Char('Z') if ctrl => {
            resp.changed = ed.undo();
        }
        // Ctrl+Backspace on terminals *without* the keyboard-enhancement
        // protocol arrives as Ctrl+H (a bare BS byte, 0x08, which crossterm
        // decodes as `Char('h')`+CONTROL) rather than `Backspace`+CONTROL, so
        // treat it as the same word-delete for parity across terminals.
        KeyCode::Char('h') | KeyCode::Char('H') if ctrl => {
            ed.checkpoint();
            ed.clear_selection();
            ed.delete_word_left();
            resp.changed = true;
        }
        // Ignore other Ctrl+letter combos: they're app shortcuts, not text.
        KeyCode::Char(c) if !ctrl => {
            ed.record_edit(EditKind::Insert);
            ed.clear_selection();
            ed.insert(c);
            resp.changed = true;
        }
        KeyCode::Enter if ed.multiline && !ctrl => {
            ed.checkpoint();
            ed.clear_selection();
            ed.newline();
            resp.changed = true;
        }
        // Ctrl+Backspace deletes the previous word (or a whole quoted token);
        // a plain Backspace deletes one character.
        KeyCode::Backspace if ctrl => {
            ed.checkpoint();
            ed.clear_selection();
            ed.delete_word_left();
            resp.changed = true;
        }
        KeyCode::Backspace => {
            ed.record_edit(EditKind::Delete);
            ed.clear_selection();
            ed.backspace();
            resp.changed = true;
        }
        KeyCode::Left => {
            ed.break_run();
            ed.set_selecting(shift);
            if ctrl { ed.home() } else { ed.left() }
        }
        KeyCode::Right => {
            ed.break_run();
            ed.set_selecting(shift);
            if ctrl { ed.end() } else { ed.right() }
        }
        KeyCode::Up => {
            ed.break_run();
            ed.set_selecting(shift);
            ed.up();
        }
        KeyCode::Down => {
            ed.break_run();
            ed.set_selecting(shift);
            ed.down();
        }
        KeyCode::Home => {
            ed.break_run();
            ed.clear_selection();
            ed.home();
        }
        KeyCode::End => {
            ed.break_run();
            ed.clear_selection();
            ed.end();
        }
        _ => {}
    }
    resp
}

/// Render a (possibly multi-line) editor into `area`, scrolling so the cursor
/// stays visible, highlighting any selection, and placing the terminal cursor.
/// When `masked`, every character is drawn as `•` (for secrets).
pub fn render_editor(f: &mut Frame, area: Rect, ed: &Editor, style: &EditorTheme, masked: bool) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (row_off, col_off, h, w) = scroll_offsets(ed, area);
    let lines: Vec<Line> = ed
        .lines
        .iter()
        .skip(row_off)
        .take(h)
        .map(|l| {
            let visible = l.chars().skip(col_off).take(w + 1);

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
    paint_selection(f, area, ed, style);
    place_cursor(f, area, ed, row_off, col_off);
}

/// Like [`render_editor`], but each logical line is drawn from caller-supplied
/// styled [`Span`]s instead of one flat run — enabling live syntax highlighting
/// while editing. `highlight(row, line)` is called for every *visible* logical
/// line and must return spans that tile the whole line (their character lengths
/// should sum to the line's length); the same horizontal-scroll window, cursor
/// and selection overlay as [`render_editor`] are applied on top, so a
/// selection still visually wins over the highlight on the selected cells.
/// Masking is intentionally unsupported here (highlighted panes aren't secret).
pub fn render_editor_highlighted(
    f: &mut Frame,
    area: Rect,
    ed: &Editor,
    style: &EditorTheme,
    highlight: impl Fn(usize, &str) -> Vec<Span<'static>>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (row_off, col_off, h, w) = scroll_offsets(ed, area);
    let lines: Vec<Line> = ed
        .lines
        .iter()
        .enumerate()
        .skip(row_off)
        .take(h)
        .map(|(idx, l)| {
            let spans = highlight(idx, l);
            // Clip the styled spans to the same horizontal window a plain render
            // would show (skip `col_off` chars, keep `w + 1`).
            Line::from(slice_spans(&spans, col_off, col_off + w + 1))
        })
        .collect();

    f.render_widget(
        Paragraph::new(lines).style(Style::default().fg(style.text)),
        area,
    );
    paint_selection(f, area, ed, style);
    place_cursor(f, area, ed, row_off, col_off);
}

/// The scroll offsets and viewport size [`render_editor`] uses: the cursor is
/// kept on the last visible row/column, so the view follows it. Returns
/// `(row_off, col_off, height, width)`.
fn scroll_offsets(ed: &Editor, area: Rect) -> (usize, usize, usize, usize) {
    let h = area.height as usize;
    let w = area.width as usize;
    (
        ed.row.saturating_sub(h - 1),
        ed.col.saturating_sub(w - 1),
        h,
        w,
    )
}

/// Repaint the cells covered by the active selection with the theme's selection
/// colours, on top of whatever the paragraph rendered.
fn paint_selection(f: &mut Frame, area: Rect, ed: &Editor, style: &EditorTheme) {
    let Some(((sr, sc), (er, ec))) = ed.selection_range() else {
        return;
    };
    let (row_off, col_off, h, w) = scroll_offsets(ed, area);
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

/// Place the terminal cursor at the editor's cursor within `area`.
fn place_cursor(f: &mut Frame, area: Rect, ed: &Editor, row_off: usize, col_off: usize) {
    let cx = area.x + (ed.col - col_off) as u16;
    let cy = area.y + (ed.row - row_off) as u16;
    f.set_cursor_position(Position::new(cx, cy));
}

/// Clip a line's styled spans to the character window `[start, end)`, splitting
/// any span that straddles a boundary and preserving each piece's style. Used
/// by [`render_editor_highlighted`] to apply horizontal scrolling to styled
/// content the same way [`render_editor`] does to plain text.
fn slice_spans(spans: &[Span], start: usize, end: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut pos = 0usize; // char offset of the current span's start
    for sp in spans {
        let content = sp.content.as_ref();
        let len = content.chars().count();
        let sp_start = pos;
        let sp_end = pos + len;
        pos = sp_end;
        if sp_end <= start {
            continue;
        }
        if sp_start >= end {
            break;
        }
        let from = start.max(sp_start) - sp_start;
        let to = end.min(sp_end) - sp_start;
        let text: String = content.chars().skip(from).take(to - from).collect();
        out.push(Span::styled(text, sp.style));
    }
    out
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

/// [`render_line_field`], but an empty field shows `placeholder` dimmed —
/// an example of what belongs there, in the space where the answer will go.
///
/// A form whose every field carries a sentence of explanation underneath is a
/// wall of prose; the same example inside the field is read at the moment it
/// is needed and costs no rows. The cursor is still placed at the start when
/// focused, so an empty focused field reads as "type here, like this".
pub fn render_line_field_placeholder(
    f: &mut Frame,
    area: Rect,
    ed: &Editor,
    style: &EditorTheme,
    focused: bool,
    mask: bool,
    placeholder: &str,
) {
    if !ed.text().is_empty() || placeholder.is_empty() {
        render_line_field(f, area, ed, style, focused, mask);
        return;
    }
    if area.width == 0 {
        return;
    }
    let shown: String = placeholder.chars().take(area.width as usize).collect();
    f.render_widget(
        Paragraph::new(shown).style(Style::default().fg(style.dim)),
        area,
    );
    if focused {
        f.set_cursor_position(Position::new(area.x, area.y));
    }
}

/// Render a single, start-anchored line of read-only `text` into `area` in the
/// given `color`, drawing `marker` in the last column when the text is wider
/// than the area (i.e. it has been truncated). The text is clipped to the
/// area's width by ratatui; only the fact that content was cut off is signalled
/// by the marker.
///
/// Unlike [`render_line_field`], this takes a plain string and an explicit
/// colour rather than an [`Editor`] and focus state, so the host controls the
/// colour (e.g. a validity highlight) — it is intended for non-focused,
/// read-only cells such as table columns. Width is measured in characters, so
/// multi-byte text is handled correctly. Pass `marker: None` to render without
/// any truncation indicator.
pub fn render_clipped_line(
    f: &mut Frame,
    area: Rect,
    text: &str,
    color: Color,
    marker: Option<TruncationMarker>,
) {
    if area.width == 0 {
        return;
    }
    let w = area.width as usize;
    f.render_widget(
        Paragraph::new(text.to_string()).style(Style::default().fg(color)),
        area,
    );
    if let Some(marker) = marker
        && text.chars().count() > w
    {
        let last = Rect {
            x: area.x + w as u16 - 1,
            y: area.y,
            width: 1,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(marker.glyph.to_string()).style(marker.style),
            last,
        );
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

    fn render_to_string<F: FnOnce(&mut Frame)>(w: u16, h: u16, f: F) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|frame| f(frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn clipped_line_draws_marker_only_when_truncated() {
        let area = Rect::new(0, 0, 5, 1);
        let marker = Some(TruncationMarker::default());

        // Fits: no marker, text shown verbatim.
        let short = render_to_string(5, 1, |f| {
            render_clipped_line(f, area, "abc", Color::White, marker);
        });
        assert_eq!(short, "abc  ");

        // Too long: last visible column becomes the ellipsis marker.
        let long = render_to_string(5, 1, |f| {
            render_clipped_line(f, area, "abcdefgh", Color::White, marker);
        });
        assert_eq!(long, "abcd\u{2026}");

        // Too long but no marker requested: plain clip, no ellipsis.
        let plain = render_to_string(5, 1, |f| {
            render_clipped_line(f, area, "abcdefgh", Color::White, None);
        });
        assert_eq!(plain, "abcde");
    }

    #[test]
    fn apply_edit_key_full_types_navigates_and_reports_change() {
        let mut ed = Editor::new("hi", true);
        let r = apply_edit_key_full(
            &mut ed,
            KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
        );
        assert!(r.changed);
        assert_eq!(r.copy, None);
        assert_eq!(ed.text(), "hi!");

        // Pure movement doesn't count as a text change.
        let r = apply_edit_key_full(&mut ed, KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert!(!r.changed);
        assert_eq!(ed.col, 0);
    }

    #[test]
    fn apply_edit_key_full_newline_only_in_multiline_mode() {
        let mut multi = Editor::new("ab", true);
        multi.home();
        multi.right();
        let r = apply_edit_key_full(
            &mut multi,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(r.changed);
        assert_eq!(multi.text(), "a\nb");

        let mut single = Editor::new("ab", false);
        single.home();
        let r = apply_edit_key_full(
            &mut single,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(!r.changed);
        assert_eq!(single.text(), "ab");
    }

    #[test]
    fn apply_edit_key_full_shift_arrow_selects_and_ctrl_y_copies() {
        let mut ed = Editor::new("hello", true);
        // Shift+Left twice selects the last two chars.
        apply_edit_key_full(&mut ed, KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        apply_edit_key_full(&mut ed, KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        assert!(ed.sel_anchor.is_some());
        let r = apply_edit_key_full(
            &mut ed,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
        );
        assert_eq!(r.copy.as_deref(), Some("lo"));
        assert!(!r.changed);
        assert_eq!(ed.text(), "hello"); // copy never mutates
    }

    #[test]
    fn apply_edit_key_full_ignores_foreign_ctrl_combos() {
        let mut ed = Editor::new("hi", true);
        let r = apply_edit_key_full(
            &mut ed,
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        );
        assert_eq!(r, EditResponse::default());
        assert_eq!(ed.text(), "hi"); // Ctrl+letter left for the host, not typed
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn delete_word_left_removes_the_previous_word_and_its_whitespace() {
        let mut ed = Editor::new("hello   world", false);
        ed.delete_word_left();
        assert_eq!(ed.text(), "hello   ");
        // A second call eats the trailing spaces and the first word too.
        ed.delete_word_left();
        assert_eq!(ed.text(), "");
        assert_eq!(ed.col, 0);
    }

    #[test]
    fn delete_word_left_removes_a_whole_quoted_token() {
        let mut ed = Editor::new("REQUEST \"Upload document\"", false);
        ed.delete_word_left();
        assert_eq!(ed.text(), "REQUEST ");
        assert_eq!(ed.col, "REQUEST ".chars().count());
    }

    #[test]
    fn delete_word_left_at_column_zero_joins_lines() {
        let mut ed = Editor::new("ab\ncd", true);
        ed.row = 1;
        ed.col = 0;
        ed.delete_word_left();
        assert_eq!(ed.text(), "abcd");
        assert_eq!((ed.row, ed.col), (0, 2));
    }

    #[test]
    fn ctrl_backspace_deletes_a_word_via_the_key_handler() {
        let mut ed = Editor::new("one two", true);
        let r = apply_edit_key_full(&mut ed, ctrl(KeyCode::Backspace));
        assert!(r.changed);
        assert_eq!(ed.text(), "one ");
    }

    #[test]
    fn ctrl_h_is_a_word_delete_alias_in_the_multi_line_handler() {
        // Legacy terminals (no keyboard-enhancement protocol) send Ctrl+H for
        // Ctrl+Backspace, so it must delete a word too.
        let mut ed = Editor::new("alpha beta", true);
        let r = apply_edit_key_full(&mut ed, ctrl(KeyCode::Char('h')));
        assert!(r.changed);
        assert_eq!(ed.text(), "alpha ");
    }

    #[test]
    fn ctrl_h_is_a_word_delete_alias_in_the_single_line_handler() {
        let mut ed = Editor::new("foo bar", false);
        apply_edit_key(&mut ed, ctrl(KeyCode::Char('h')));
        assert_eq!(ed.text(), "foo ");
        // ...and it stays undoable.
        apply_edit_key(&mut ed, ctrl(KeyCode::Char('z')));
        assert_eq!(ed.text(), "foo bar");
    }

    #[test]
    fn ctrl_h_deletes_a_whole_quoted_token() {
        // A quoted request name (spaces and all) goes in one Ctrl+H, matching
        // the Ctrl+Backspace behaviour.
        let mut ed = Editor::new("REQUEST \"Upload for document\"", true);
        let r = apply_edit_key_full(&mut ed, ctrl(KeyCode::Char('h')));
        assert!(r.changed);
        assert_eq!(ed.text(), "REQUEST ");
    }

    #[test]
    fn undo_reverts_a_typed_run_in_one_step_and_redo_replays_it() {
        let mut ed = Editor::new("", true);
        for c in "abc".chars() {
            apply_edit_key_full(&mut ed, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(ed.text(), "abc");
        // A single undo reverts the whole coalesced typing run.
        assert!(ed.undo());
        assert_eq!(ed.text(), "");
        // Redo replays it.
        assert!(ed.redo());
        assert_eq!(ed.text(), "abc");
    }

    #[test]
    fn cursor_movement_splits_typing_into_separate_undo_steps() {
        let mut ed = Editor::new("", true);
        apply_edit_key_full(
            &mut ed,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        // Moving the cursor ends the run, so the next character is its own step.
        apply_edit_key_full(&mut ed, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        apply_edit_key_full(
            &mut ed,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
        );
        assert_eq!(ed.text(), "ba");
        ed.undo();
        assert_eq!(ed.text(), "a"); // only the 'b' is undone
    }

    #[test]
    fn ctrl_z_undoes_through_the_key_handler() {
        let mut ed = Editor::new("seed", true);
        apply_edit_key_full(
            &mut ed,
            KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
        );
        assert_eq!(ed.text(), "seed!");
        let r = apply_edit_key_full(&mut ed, ctrl(KeyCode::Char('z')));
        assert!(r.changed);
        assert_eq!(ed.text(), "seed");
    }

    #[test]
    fn checkpoint_makes_a_programmatic_edit_undoable() {
        let mut ed = Editor::new("REQUEST ", true);
        ed.checkpoint();
        ed.insert_str("Oauth");
        assert_eq!(ed.text(), "REQUEST Oauth");
        assert!(ed.undo());
        assert_eq!(ed.text(), "REQUEST ");
    }

    #[test]
    fn undo_on_empty_history_is_a_no_op() {
        let mut ed = Editor::new("x", false);
        assert!(!ed.undo());
        assert_eq!(ed.text(), "x");
    }

    #[test]
    fn apply_edit_key_ctrl_backspace_deletes_a_word_in_a_single_line_field() {
        let mut ed = Editor::new("foo bar", false);
        apply_edit_key(&mut ed, ctrl(KeyCode::Backspace));
        assert_eq!(ed.text(), "foo ");
        apply_edit_key(&mut ed, ctrl(KeyCode::Char('z')));
        assert_eq!(ed.text(), "foo bar"); // undo restores it
    }

    #[test]
    fn slice_spans_clips_to_a_char_window_keeping_styles() {
        let green = Style::default().fg(Color::Green);
        let spans = vec![Span::styled("keyword", green), Span::raw(" rest")];
        // Window [3, 9) spans the "word" tail of the styled run and " r".
        let out = slice_spans(&spans, 3, 9);
        let text: String = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "word r");
        assert_eq!(out[0].content.as_ref(), "word");
        assert_eq!(out[0].style.fg, Some(Color::Green));
        assert_eq!(out[1].content.as_ref(), " r");
        assert_eq!(out[1].style.fg, None);
    }

    #[test]
    fn render_editor_highlighted_paints_per_span_colours() {
        let ed = Editor::new("REQUEST x", false);
        let green = Style::default().fg(Color::Green);
        let style = EditorTheme {
            text: Color::White,
            panel: Color::Black,
            dim: Color::Gray,
            select_fg: Color::Black,
            select_bg: Color::Cyan,
        };
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(20, 1)).unwrap();
        terminal
            .draw(|f| {
                render_editor_highlighted(f, Rect::new(0, 0, 20, 1), &ed, &style, |_, line| {
                    // Colour a leading "REQUEST" keyword green, rest default.
                    if let Some(rest) = line.strip_prefix("REQUEST") {
                        vec![Span::styled("REQUEST", green), Span::raw(rest.to_string())]
                    } else {
                        vec![Span::raw(line.to_string())]
                    }
                });
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        // The keyword cells are green; a cell past it inherits the text colour.
        assert_eq!(buffer.cell((0, 0)).unwrap().fg, Color::Green);
        assert_eq!(buffer.cell((6, 0)).unwrap().fg, Color::Green);
        assert_eq!(buffer.cell((8, 0)).unwrap().fg, Color::White);
    }
}
