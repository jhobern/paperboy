# tui-line-editor

A small single- or multi-line text editor primitive (with selection and
masking) for [ratatui](https://docs.rs/ratatui) apps.

- **Cursor & lines you own** — text is a `Vec<String>` of logical lines with a
  `(row, col)` cursor; granular mutators (`insert`, `backspace`,
  `left`/`right`/`up`/`down`, `home`/`end`, `newline`, `insert_str`) let you
  wire your own key handling and interleave editing with app-level logic.
- **Selection** — anchor/extend a selection (`begin_selection_if_needed`,
  `set_selecting`, `selection_range`, `selected_text`) with ordinary stream
  semantics across multiple lines.
- **Masking** — render every character as `•` for secrets.
- **Two renderers** — `render_editor` draws a scrolling multi-line view that
  follows the cursor and highlights the selection; `render_line_field` draws a
  compact single-line field. Both take an `EditorTheme` so you supply your own
  colours.
- **Mouse mapping** — `point_to_row_col` maps a terminal point back to a
  `(row, col)` text position for click/drag selection.
- **Batteries-included single-line key handler** — `apply_edit_key` covers the
  common form-field case (typing, backspace, arrows, `Ctrl`+`←`/`→` for
  home/end).

## Example

```rust
use tui_line_editor::Editor;

let mut ed = Editor::new("hello", false);
ed.home();
ed.insert('>');
ed.insert(' ');
assert_eq!(ed.text(), "> hello");
```

## Rendering (you supply the frame/area/theme)

```rust,no_run
use ratatui::style::Color;
use tui_line_editor::{Editor, EditorTheme, render_line_field};

# fn demo(f: &mut ratatui::Frame, area: ratatui::layout::Rect) {
let ed = Editor::new("token", false);
let theme = EditorTheme {
    text: Color::White,
    panel: Color::Black,
    dim: Color::DarkGray,
    select_fg: Color::Black,
    select_bg: Color::Cyan,
};
render_line_field(f, area, &ed, &theme, /* focused */ true, /* mask */ true);
# }
```

## License

MIT
