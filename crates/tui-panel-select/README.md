# tui-panel-select

Panel-scoped mouse text selection and clipboard copy for
[ratatui](https://docs.rs/ratatui) apps.

A terminal's own click-drag selection can't be confined to one panel — it spans
the full terminal row, sweeping up borders and neighbouring panels. This crate
lets your app capture the mouse itself and implement a selection that is:

- **Confined to a single panel's rectangle** — never spills into other panels
  or the border.
- **Natural "stream" selection** — first line from the click column to its end,
  full lines in between, last line up to the release column (never a
  rectangular block).
- **Stable across resize / rewrap / scroll** — selections are stored as logical
  `(line, column)` positions, not stale screen cells, so the same *characters*
  stay selected when the panel is resized or rewrapped.
- **Cheap on huge content** — only the rows actually on screen are ever wrapped
  or painted, so a multi-megabyte body (or one enormous unbroken line) stays
  responsive.
- **Copy that works locally and remotely** — on mouse-up the text is copied via
  a local clipboard tool (`wl-copy`/`xclip`/`xsel`/`pbcopy`/`clip.exe`) when
  available, falling back to an OSC 52 escape sequence for SSH/tmux sessions.

## Quick start — the batteries-included API

```rust
use std::sync::Arc;
use ratatui::layout::Rect;
use tui_panel_select::SelectablePanel;

let mut panel = SelectablePanel::new();

// Each frame, before drawing, give the panel its text and inner width.
panel.set_content(Arc::from("hello world\nsecond line"), 40);

// The panel's inner text area on screen, and its scroll offset (wrapped rows).
let area = Rect::new(1, 1, 40, 10);
let scroll = 0;

// Mouse down starts a selection; drag extends it; up copies it.
panel.begin_selection(area, scroll, (1, 1));   // click at 'h'
panel.extend_selection(area, scroll, (5, 1));  // drag to 'o'
assert_eq!(panel.selected_text().as_deref(), Some("hello"));
panel.copy_selection();                         // -> system clipboard
```

Rendering each frame:

```rust,no_run
# use tui_panel_select::SelectablePanel;
# use ratatui::layout::Rect;
# let panel = SelectablePanel::new();
# let area = Rect::new(0, 0, 40, 10);
# let scroll = 0u16;
// 1. Draw the visible wrapped rows into your panel:
let rows = panel.visible_rows(scroll, area.height);

// 2. Paint the highlight over the selected cells:
for (row, col_from, col_to) in panel.highlight_cells(area, scroll) {
    // invert/style cells [col_from, col_to) on terminal row `row`
}
```

Wire these into your event loop: capture the mouse (`EnableMouseCapture`), then
call `begin_selection` on `MouseEventKind::Down`, `extend_selection` on `Drag`,
and `copy_selection` on `Up`. Call `set_content` every frame — it only rebuilds
its cache when the text (by `Arc` identity) or width actually changed.

## Low-level primitives

If your app already owns its selection state (e.g. multiple simultaneous
selections, keyboard-extended selection, excluding decorative characters from
the copied text), skip `SelectablePanel` and use the stateless building blocks
directly:

- [`wrapcache::PanelWrap`] / [`wrapcache::TextPos`] — the line/wrap cache and
  logical positions, with conversions between screen and logical space.
- [`selection`] — pure functions: `point_to_textpos`, `extract_text`,
  `highlight_cells`, `strip_positions`.
- [`clipboard::copy_to_clipboard`] — local tool + OSC 52 fallback.
- [`wrap`] — the underlying character-exact line-wrapping helpers.

## License

MIT
