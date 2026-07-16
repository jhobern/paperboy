# tui-rgb-picker

A small, configurable RGB colour picker widget (channel sliders) for
[ratatui](https://docs.rs/ratatui) apps.

- **Three channel sliders** — a colour swatch with its `#RRGGBB` hex value on
  top, then one horizontal bar per R/G/B channel showing the current value.
- **Bring your own keys** — low-level mutators (`adjust`, `focus_channel`,
  `type_digit`, `backspace`, `revert`) let you bind whatever shortcuts you
  like; or use the built-in `handle_key` default map.
- **Fully styleable & localizable** — pass a `ColorPickerStyle` (colours from
  your own theme) and `ColorPickerLabels` (channel names + an optional hint).
- **Unopinionated about its frame** — it renders its *contents* into whatever
  `Rect` you give it, so you keep control of the block, title, centering and
  popup behaviour.

## Example

```rust
use tui_rgb_picker::{ColorPicker, ColorPickerAction};
use ratatui::crossterm::event::{KeyCode, KeyEvent};

let mut picker = ColorPicker::new([120, 200, 90]);

// Low-level: bind your own keys.
picker.focus_next_channel();   // move to Green
picker.adjust(16);             // +16 on the focused channel
assert_eq!(picker.rgb(), [120, 216, 90]);

// ...or use the built-in default key map, which reports what happened:
match picker.handle_key(KeyEvent::from(KeyCode::Enter)) {
    ColorPickerAction::Accept  => { /* commit picker.rgb() */ }
    ColorPickerAction::Cancel  => { /* discard, or picker.revert() */ }
    ColorPickerAction::Changed => { /* live-preview picker.rgb() */ }
    ColorPickerAction::Ignored => {}
}
```

## Rendering (you supply the frame/block)

```rust,no_run
use tui_rgb_picker::{ColorPicker, ColorPickerStyle, ColorPickerLabels};
# use ratatui::layout::Rect;
# let picker = ColorPicker::new([0, 0, 0]);
# let inner = Rect::new(0, 0, 30, 6);

let style = ColorPickerStyle::default();      // override colours to match your theme
let labels = ColorPickerLabels {
    channels: ["Red", "Green", "Blue"],
    hint: Some("←/→ ±1, ^←/^→ ±16, Enter accepts"),
};

// With a ratatui Frame, draw it into your popup's inner area:
// frame.render_widget(picker.widget(&style, &labels), inner);
```

The default key map (`handle_key`):

| Key | Action |
|---|---|
| `Enter` | Accept |
| `Esc` | Cancel (does not auto-revert) |
| `Up`/`k`, `Down`/`j` | Focus previous / next channel |
| `Left` / `Right` | ∓1 on the focused channel |
| `Ctrl`+`Left`/`Right`, `PageDown`/`PageUp` | ∓16 |
| digits, `Backspace` | Type / delete a value on the focused channel |

## License

MIT
