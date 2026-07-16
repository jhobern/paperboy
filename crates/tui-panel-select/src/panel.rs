//! A ready-to-use, batteries-included wrapper: [`SelectablePanel`].
//!
//! The primitives in [`crate::wrapcache`] and [`crate::selection`] are
//! deliberately stateless (easy to embed in an existing app that already
//! owns its own selection state). `SelectablePanel` bundles them into the
//! smallest useful stateful object for the common case: one scrollable text
//! panel whose content can be mouse-selected and copied, with the selection
//! confined to the panel and surviving resizes/rewraps.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use ratatui::layout::Rect;
//! use tui_panel_select::SelectablePanel;
//!
//! let mut panel = SelectablePanel::new();
//! // Each frame, before drawing, tell the panel its text and inner width.
//! panel.set_content(Arc::from("hello world\nsecond line"), 40);
//!
//! // The panel's inner text area on screen, and its current scroll offset.
//! let area = Rect::new(1, 1, 40, 10);
//! let scroll = 0;
//!
//! // Mouse down starts a selection; drag extends it; up copies it.
//! panel.begin_selection(area, scroll, (1, 1));      // click at "h"
//! panel.extend_selection(area, scroll, (5, 1));     // drag to "o"
//! assert_eq!(panel.selected_text().as_deref(), Some("hello"));
//!
//! // On mouse-up, copy to the system clipboard (best-effort).
//! panel.copy_selection();
//! ```
//!
//! Rendering each frame:
//!
//! ```no_run
//! # use tui_panel_select::SelectablePanel;
//! # use ratatui::layout::Rect;
//! # let panel = SelectablePanel::new();
//! # let area = Rect::new(0, 0, 40, 10);
//! # let scroll = 0u16;
//! // 1. Draw the visible wrapped rows:
//! let rows = panel.visible_rows(scroll, area.height);
//! // ...render `rows` into `area`...
//!
//! // 2. Paint the highlight over the selected cells:
//! for (row, col_from, col_to) in panel.highlight_cells(area, scroll) {
//!     // ...invert/style cells [col_from, col_to) on terminal row `row`...
//!     let _ = (row, col_from, col_to);
//! }
//! ```

use std::sync::Arc;

use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::clipboard::copy_to_clipboard;
use crate::selection;
use crate::wrapcache::{PanelWrap, TextPos};

/// One scrollable, mouse-selectable text panel.
///
/// Holds the panel's wrapped-line cache and its current selection. Cheap to
/// keep around across frames: [`set_content`](Self::set_content) only
/// rebuilds the cache when the text or width actually changed, so calling it
/// unconditionally every frame is fine.
#[derive(Default)]
pub struct SelectablePanel {
    wrap: Option<PanelWrap>,
    /// `(anchor, cursor)` in logical positions. The anchor is where the
    /// selection started (mouse-down); the cursor is its live end (drag).
    selection: Option<(TextPos, TextPos)>,
}

impl SelectablePanel {
    /// A panel with no content and no selection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or update) the panel's text and the inner width it wraps to, in
    /// columns. A no-op when neither the text (by `Arc` identity) nor the
    /// width changed, so it's safe — and intended — to call every frame.
    ///
    /// Pass a fresh `Arc<str>` whenever the underlying text changes; identity
    /// (not byte comparison) is what signals "content changed".
    pub fn set_content(&mut self, text: Arc<str>, width: usize) {
        PanelWrap::rebuild_if_needed(&mut self.wrap, &text, width);
    }

    /// Whether any content has been set yet.
    pub fn has_content(&self) -> bool {
        self.wrap.is_some()
    }

    /// The total number of wrapped rows the current content occupies — the
    /// scrollable extent, for sizing a scrollbar or clamping a scroll offset.
    pub fn total_rows(&self) -> u32 {
        self.wrap.as_ref().map_or(0, PanelWrap::total_rows)
    }

    /// The exact, unmodified text the panel was built from (every line, not
    /// just what's scrolled into view) — for a "copy the whole panel"
    /// action that needs no selection.
    pub fn whole_text(&self) -> Option<&str> {
        self.wrap.as_ref().map(PanelWrap::source)
    }

    /// Start a selection at terminal point `(col, row)`, given the panel's
    /// inner `area` and current `scroll` (in wrapped rows). Points outside
    /// `area` clamp to its nearest edge. No-op if there's no content.
    pub fn begin_selection(&mut self, area: Rect, scroll: u16, point: (u16, u16)) {
        let Some(wrap) = self.wrap.as_ref() else {
            return;
        };
        let pos = selection::point_to_textpos(point, area, scroll, wrap);
        self.selection = Some((pos, pos));
    }

    /// Extend the in-progress selection's live end to terminal point
    /// `(col, row)`. No-op if no selection was started or there's no content.
    pub fn extend_selection(&mut self, area: Rect, scroll: u16, point: (u16, u16)) {
        let Some(wrap) = self.wrap.as_ref() else {
            return;
        };
        if let Some((_, cursor)) = self.selection.as_mut() {
            *cursor = selection::point_to_textpos(point, area, scroll, wrap);
        }
    }

    /// Drop the current selection.
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Whether there is a selection (even a zero-width one from a bare click).
    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    /// The currently selected text (lines joined with `\n`), or `None` when
    /// there's no selection or it covers nothing but whitespace.
    pub fn selected_text(&self) -> Option<String> {
        let wrap = self.wrap.as_ref()?;
        let (anchor, cursor) = self.selection?;
        selection::extract_text(anchor, cursor, wrap, None)
    }

    /// Copy the current selection to the system clipboard (best-effort:
    /// local clipboard tool, else an OSC 52 escape sequence). Returns `true`
    /// if there was text to copy.
    pub fn copy_selection(&self) -> bool {
        match self.selected_text() {
            Some(text) => {
                copy_to_clipboard(&text);
                true
            }
            None => false,
        }
    }

    /// The visible wrapped rows for a `height`-row window starting at
    /// wrapped-row `scroll` — ready to render. Only the rows actually on
    /// screen are wrapped, regardless of total content size.
    pub fn visible_rows(&self, scroll: u16, height: u16) -> Vec<Line<'static>> {
        self.wrap
            .as_ref()
            .map(|w| w.visible_window(scroll, height))
            .unwrap_or_default()
    }

    /// The selection's on-screen cells to highlight, as `(row, col_from,
    /// col_to_exclusive)` in absolute terminal coordinates, bounded to the
    /// visible window. Empty when there's no selection or it's off-screen.
    pub fn highlight_cells(&self, area: Rect, scroll: u16) -> Vec<(u16, u16, u16)> {
        let Some(wrap) = self.wrap.as_ref() else {
            return Vec::new();
        };
        let Some((anchor, cursor)) = self.selection else {
            return Vec::new();
        };
        selection::highlight_cells(anchor, cursor, wrap, area, scroll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(text: &str, width: usize) -> SelectablePanel {
        let mut p = SelectablePanel::new();
        p.set_content(Arc::from(text), width);
        p
    }

    #[test]
    fn a_fresh_panel_has_no_content_or_selection() {
        let p = SelectablePanel::new();
        assert!(!p.has_content());
        assert!(!p.has_selection());
        assert_eq!(p.selected_text(), None);
        assert!(p.highlight_cells(Rect::new(0, 0, 10, 5), 0).is_empty());
    }

    #[test]
    fn begin_and_extend_select_the_covered_text() {
        // "hello world" on one line; width wide enough not to wrap.
        let mut p = panel("hello world", 40);
        let area = Rect::new(2, 1, 40, 5);
        p.begin_selection(area, 0, (2, 1)); // col 0 -> 'h'
        p.extend_selection(area, 0, (6, 1)); // col 4 -> 'o'
        assert!(p.has_selection());
        assert_eq!(p.selected_text().as_deref(), Some("hello"));
    }

    #[test]
    fn a_multi_line_drag_joins_lines_with_newlines() {
        let mut p = panel("first\nsecond", 40);
        let area = Rect::new(0, 0, 40, 5);
        p.begin_selection(area, 0, (2, 0)); // 'r' in first (col 2)
        p.extend_selection(area, 0, (2, 1)); // 'c' in second (col 2)
        assert_eq!(p.selected_text().as_deref(), Some("rst\nsec"));
    }

    #[test]
    fn clearing_removes_the_selection_and_its_highlight() {
        let mut p = panel("hello", 40);
        let area = Rect::new(0, 0, 40, 5);
        p.begin_selection(area, 0, (0, 0));
        p.extend_selection(area, 0, (4, 0));
        assert!(!p.highlight_cells(area, 0).is_empty());
        p.clear_selection();
        assert!(!p.has_selection());
        assert!(p.highlight_cells(area, 0).is_empty());
    }

    #[test]
    fn whole_text_and_total_rows_reflect_the_content() {
        let p = panel("0123456789ABCDE\n", 10); // 15-char line wraps to 2 rows
        assert_eq!(p.whole_text(), Some("0123456789ABCDE\n"));
        assert_eq!(p.total_rows(), 2);
    }

    #[test]
    fn selection_survives_a_width_change_by_staying_on_the_same_characters() {
        // Selection is stored logically, so re-wrapping at a new width keeps
        // the same characters selected.
        let mut p = panel("hello world", 40);
        let area = Rect::new(0, 0, 40, 5);
        p.begin_selection(area, 0, (0, 0));
        p.extend_selection(area, 0, (4, 0)); // "hello"
        assert_eq!(p.selected_text().as_deref(), Some("hello"));
        // Same text, narrower width (forces a rewrap); selection unchanged.
        p.set_content(Arc::from("hello world"), 5);
        assert_eq!(p.selected_text().as_deref(), Some("hello"));
    }
}
