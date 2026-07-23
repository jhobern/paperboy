//! PaperBoy's line/multi-line editor is now the reusable [`tui_line_editor`]
//! crate. This module re-exports the editor type and key handler, and keeps
//! the two theming-aware render wrappers so call sites can keep passing a
//! [`Theme`] instead of an [`EditorTheme`].

use ratatui::Frame;
use ratatui::layout::Rect;

use super::theme::*;
use ratatui::style::{Color, Style};
use tui_line_editor::{EditorTheme, TruncationMarker};

pub(crate) use tui_line_editor::{Editor, apply_edit_key};

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
