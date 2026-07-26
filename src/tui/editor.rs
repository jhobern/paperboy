//! PaperBoy's line/multi-line editor is now the reusable [`tui_line_editor`]
//! crate. This module re-exports the editor type and key handler, and keeps
//! the two theming-aware render wrappers so call sites can keep passing a
//! [`Theme`] instead of an [`EditorTheme`].

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;

use super::theme::*;
use ratatui::style::{Color, Style};
use tui_line_editor::{EditorTheme, TruncationMarker};

pub(crate) use tui_line_editor::{Editor, apply_edit_key, apply_edit_key_full};

/// Move the editor cursor left to the start of the previous word: skip any
/// whitespace immediately to the left, then the run of non-whitespace. At
/// column 0 this falls back to a plain left move (wrapping to the previous
/// line), matching a normal cursor. Shared by every editing surface so
/// `Ctrl+Left` means the same thing in the report editor, Raw Mode, and the
/// request wizard.
pub(crate) fn word_left(ed: &mut Editor) {
    if ed.col == 0 {
        ed.left();
        return;
    }
    let chars: Vec<char> = ed.lines[ed.row].chars().collect();
    let mut c = ed.col;
    while c > 0 && chars[c - 1].is_whitespace() {
        c -= 1;
    }
    while c > 0 && !chars[c - 1].is_whitespace() {
        c -= 1;
    }
    ed.col = c;
}

/// Move the editor cursor right past the current/next word: skip any whitespace
/// under the cursor, then the run of non-whitespace. At the line end this falls
/// back to a plain right move (wrapping to the next line). The `Ctrl+Right`
/// counterpart of [`word_left`].
pub(crate) fn word_right(ed: &mut Editor) {
    let len = ed.line_len(ed.row);
    if ed.col >= len {
        ed.right();
        return;
    }
    let chars: Vec<char> = ed.lines[ed.row].chars().collect();
    let mut c = ed.col;
    while c < len && chars[c].is_whitespace() {
        c += 1;
    }
    while c < len && !chars[c].is_whitespace() {
        c += 1;
    }
    ed.col = c;
}

/// Map PaperBoy's [`Theme`] to the crate's [`EditorTheme`].
fn editor_theme(th: &Theme) -> EditorTheme {
    EditorTheme {
        text: th.text,
        panel: th.panel,
        dim: th.dim,
        select_fg: th.select_fg,
        select_bg: th.select_bg,
    }
}

pub(crate) fn render_editor(f: &mut Frame, area: Rect, ed: &Editor, masked: bool, th: &Theme) {
    tui_line_editor::render_editor(f, area, ed, &editor_theme(th), masked);
}

/// Like [`render_editor`], but each visible logical line is drawn from the
/// styled spans `highlight(row, line)` returns — used to keep syntax
/// highlighting live while a report's source panel has edit focus. The cursor,
/// selection overlay and horizontal scrolling behave exactly as in
/// [`render_editor`].
pub(crate) fn render_editor_highlighted(
    f: &mut Frame,
    area: Rect,
    ed: &Editor,
    th: &Theme,
    highlight: impl Fn(usize, &str) -> Vec<Span<'static>>,
) {
    tui_line_editor::render_editor_highlighted(f, area, ed, &editor_theme(th), highlight);
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
    tui_line_editor::render_line_field(f, area, ed, &editor_theme(th), focused, mask);
}

/// Render a read-only single line of `text` into `area` in `color`, drawing a
/// dim ellipsis (`…`) in the last column when the text is too wide to fit — so
/// truncated, unfocused cells still read as "there is more here" rather than
/// looking complete. Used for the wizard's unfocused Header/Cookie/Query/Form
/// cells, where the colour is chosen by the caller (e.g. a file-validity
/// highlight) rather than by focus state.
pub(crate) fn render_clipped_line(f: &mut Frame, area: Rect, text: &str, color: Color, th: &Theme) {
    let marker = TruncationMarker {
        glyph: '\u{2026}',
        style: Style::default().fg(th.dim),
    };
    tui_line_editor::render_clipped_line(f, area, text, color, Some(marker));
}
