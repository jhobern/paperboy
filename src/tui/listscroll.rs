//! A remembered scroll position for a ratatui `List`.
//!
//! ratatui's `ListState` owns both the selection *and* the viewport offset, and
//! a fresh `ListState::default()` starts at offset zero. Drawing a list that way
//! means ratatui scrolls the *minimum* needed to reveal the selection on every
//! frame — which lands the selection on the last visible row whenever it is
//! below the fold. The list then follows the cursor instead of the cursor
//! moving through the list: walking up a long list keeps the selected row
//! pinned to the bottom edge while everything else slides past it.
//!
//! Carrying the offset from frame to frame leaves ratatui with nothing to do
//! until the selection actually reaches an edge, which is what every list in
//! every editor does. This type is that carried offset: a `Cell`, because the
//! draw functions all take `&` (they are views, not mutations), and the same
//! reason `TuiApp` holds its measured widths in `Cell`s.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{List, ListState};

/// The scroll position of one list, remembered between frames.
///
/// `ctx` tags the offset with *which* list it belongs to for widgets that are
/// reused for more than one list (the collection tree is drawn once per tab,
/// for instance). A different `ctx` starts at the top, because another list's
/// scroll position means nothing in this one. Widgets that only ever show one
/// list can leave it at the default of zero.
#[derive(Debug, Default)]
pub(crate) struct ListScroll {
    at: std::cell::Cell<(u64, usize)>,
}

impl ListScroll {
    /// Render `list` into `area`, carrying the viewport across frames.
    ///
    /// `len` is the number of rows in the list; the carried offset is clamped
    /// to it so a list that shrank (a filter was typed, a directory was left)
    /// can't leave the viewport scrolled past its own end and showing nothing.
    /// Returns the offset actually used, which is what mouse hit-testing needs
    /// to turn a screen row back into a list index.
    pub(crate) fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        list: List<'_>,
        sel: Option<usize>,
        len: usize,
    ) -> usize {
        self.render_ctx(f, area, list, sel, len, 0)
    }

    /// [`Self::render`], for a widget that is reused across several lists.
    pub(crate) fn render_ctx(
        &self,
        f: &mut Frame,
        area: Rect,
        list: List<'_>,
        sel: Option<usize>,
        len: usize,
        ctx: u64,
    ) -> usize {
        let (last_ctx, carried) = self.at.get();
        let carried = if last_ctx == ctx {
            carried.min(len.saturating_sub(1))
        } else {
            0
        };
        let mut st = ListState::default().with_offset(carried);
        st.select(sel);
        f.render_stateful_widget(list, area, &mut st);
        let offset = st.offset();
        self.at.set((ctx, offset));
        offset
    }

    /// The offset as of the last frame — for hit-testing done outside the draw
    /// call that rendered the list, and for tests that assert on where a list
    /// ended up scrolled to.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn offset(&self) -> usize {
        self.at.get().1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::widgets::ListItem;

    /// Render `sel` into a `height`-row viewport, twice over, and report where
    /// the viewport ended up.
    fn scroll_through(rows: usize, height: u16, selections: &[usize]) -> Vec<usize> {
        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, height)).unwrap();
        let scroll = ListScroll::default();
        let mut seen = Vec::new();
        for &sel in selections {
            term.draw(|f| {
                let items: Vec<ListItem> =
                    (0..rows).map(|i| ListItem::new(format!("{i}"))).collect();
                let area = Rect::new(0, 0, 20, height);
                seen.push(scroll.render(f, area, List::new(items), Some(sel), rows));
            })
            .unwrap();
        }
        seen
    }

    /// The whole point: the cursor moves through the visible rows, and the list
    /// only scrolls once the cursor has reached an edge.
    #[test]
    fn the_list_holds_still_until_the_cursor_reaches_an_edge() {
        // Ten rows in a four-row viewport. Walking down to row 6 drags the
        // viewport with it (rows 3..6 visible), and walking back up to row 4
        // must *not* move it again — row 4 is already on screen.
        let seen = scroll_through(10, 4, &[0, 6, 4]);
        assert_eq!(seen[0], 0, "starts at the top");
        assert_eq!(seen[1], 3, "walking past the bottom edge scrolls");
        assert_eq!(
            seen[2], 3,
            "coming back to a row that is already visible must not scroll"
        );
    }

    /// A list that shrank under a carried offset must not leave the viewport
    /// parked past its own end, showing an empty panel.
    #[test]
    fn a_shrinking_list_pulls_the_viewport_back() {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 4)).unwrap();
        let scroll = ListScroll::default();
        term.draw(|f| {
            let items: Vec<ListItem> = (0..40).map(|i| ListItem::new(format!("{i}"))).collect();
            scroll.render(f, Rect::new(0, 0, 20, 4), List::new(items), Some(30), 40);
        })
        .unwrap();
        assert!(scroll.offset() > 0, "scrolled a long way down");

        // Now the filter matches two rows.
        term.draw(|f| {
            let items: Vec<ListItem> = (0..2).map(|i| ListItem::new(format!("{i}"))).collect();
            scroll.render(f, Rect::new(0, 0, 20, 4), List::new(items), Some(0), 2);
        })
        .unwrap();
        assert_eq!(scroll.offset(), 0, "the short list is shown from the top");
    }

    /// One widget, two lists: the second must not inherit the first's viewport.
    #[test]
    fn another_lists_scroll_position_does_not_carry_over() {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 4)).unwrap();
        let scroll = ListScroll::default();
        let draw = |term: &mut ratatui::Terminal<_>, sel: usize, ctx: u64| {
            term.draw(|f| {
                let items: Vec<ListItem> = (0..40).map(|i| ListItem::new(format!("{i}"))).collect();
                scroll.render_ctx(
                    f,
                    Rect::new(0, 0, 20, 4),
                    List::new(items),
                    Some(sel),
                    40,
                    ctx,
                );
            })
            .unwrap();
        };
        draw(&mut term, 30, 1);
        assert!(scroll.offset() > 0);
        draw(&mut term, 0, 2);
        assert_eq!(scroll.offset(), 0, "a different list starts at the top");
    }
}
