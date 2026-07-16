//! A small, configurable RGB colour picker for [ratatui] apps.
//!
//! [`ColorPicker`] holds the picker's state — the working `[r, g, b]` value,
//! which channel is focused, and any digits typed so far — and exposes both
//! low-level mutators (so you can bind your own keys) and an optional
//! batteries-included [`ColorPicker::handle_key`]. Rendering is a separate,
//! fully-styleable ratatui [`Widget`]: a colour swatch with its hex value on
//! top, then one horizontal bar slider per channel, and an optional hint line.
//!
//! The picker is intentionally unopinionated about its frame: it renders its
//! *contents* into whatever [`Rect`] you give it, so you keep control of the
//! surrounding block, title, centering and popup behaviour.
//!
//! # Example
//!
//! ```
//! use tui_rgb_picker::{ColorPicker, ColorPickerAction};
//! use ratatui::crossterm::event::{KeyCode, KeyEvent};
//!
//! let mut picker = ColorPicker::new([120, 200, 90]);
//!
//! // Low-level: bind your own keys.
//! picker.focus_next_channel();   // move to Green
//! picker.adjust(16);             // +16 on the focused channel
//! assert_eq!(picker.rgb(), [120, 216, 90]);
//!
//! // Or the built-in default key map:
//! match picker.handle_key(KeyEvent::from(KeyCode::Enter)) {
//!     ColorPickerAction::Accept => { /* commit picker.rgb() */ }
//!     ColorPickerAction::Cancel => { /* discard / revert() */ }
//!     ColorPickerAction::Changed => { /* live-preview picker.rgb() */ }
//!     ColorPickerAction::Ignored => {}
//! }
//! ```
//!
//! Rendering (you supply the frame/block):
//!
//! ```no_run
//! # use tui_rgb_picker::{ColorPicker, ColorPickerStyle, ColorPickerLabels};
//! # use ratatui::layout::Rect;
//! # use ratatui::buffer::Buffer;
//! # let picker = ColorPicker::new([0, 0, 0]);
//! # let area = Rect::new(0, 0, 30, 6);
//! # let mut buf = Buffer::empty(area);
//! let style = ColorPickerStyle::default();
//! let labels = ColorPickerLabels::default();
//! picker.widget(&style, &labels).render_into(area, &mut buf);
//! ```
//!
//! [ratatui]: https://docs.rs/ratatui
//! [`Widget`]: ratatui::widgets::Widget

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};

/// Number of channels (red, green, blue).
const CHANNELS: usize = 3;

/// What a [`ColorPicker::handle_key`] call did, so the host can react
/// (commit, cancel, live-preview) without the picker itself knowing about
/// your application's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorPickerAction {
    /// The working colour changed (adjust, typed digit, backspace).
    Changed,
    /// The user asked to accept the current colour (Enter by default).
    Accept,
    /// The user asked to cancel (Esc by default). The picker does *not*
    /// revert automatically — call [`ColorPicker::revert`] or read
    /// [`ColorPicker::original`] if you want the pre-edit value.
    Cancel,
    /// The key wasn't one the default map handles; nothing changed.
    Ignored,
}

/// The state of an RGB colour picker: the working colour, the focused
/// channel, and the pre-edit colour (for cancel/revert).
#[derive(Clone, Debug)]
pub struct ColorPicker {
    channel: usize,
    rgb: [u8; CHANNELS],
    orig: [u8; CHANNELS],
    /// Digits typed for the current channel since it was focused; cleared as
    /// soon as an arrow adjustment or a channel change happens.
    buf: String,
}

impl ColorPicker {
    /// A picker opened on `rgb`, with the red channel focused. `rgb` is also
    /// remembered as the [`original`](Self::original) value for cancel/revert.
    pub fn new(rgb: [u8; 3]) -> Self {
        Self {
            channel: 0,
            rgb,
            orig: rgb,
            buf: String::new(),
        }
    }

    /// The current working colour.
    pub fn rgb(&self) -> [u8; 3] {
        self.rgb
    }

    /// The colour the picker was opened with (for cancel/revert).
    pub fn original(&self) -> [u8; 3] {
        self.orig
    }

    /// The focused channel: `0` = red, `1` = green, `2` = blue.
    pub fn channel(&self) -> usize {
        self.channel
    }

    /// The working colour as an uppercase `#RRGGBB` string.
    pub fn hex(&self) -> String {
        let [r, g, b] = self.rgb;
        format!("#{r:02X}{g:02X}{b:02X}")
    }

    /// Replace the working colour outright (does not change the remembered
    /// original). Clears any half-typed digits.
    pub fn set_rgb(&mut self, rgb: [u8; 3]) {
        self.rgb = rgb;
        self.buf.clear();
    }

    /// Restore the working colour to the [`original`](Self::original).
    pub fn revert(&mut self) {
        self.rgb = self.orig;
        self.buf.clear();
    }

    /// Add `delta` (may be negative) to the focused channel, clamped to
    /// `0..=255`. Clears any half-typed digits.
    pub fn adjust(&mut self, delta: i16) {
        let v = self.rgb[self.channel] as i16 + delta;
        self.rgb[self.channel] = v.clamp(0, 255) as u8;
        self.buf.clear();
    }

    /// Focus channel `channel` (clamped to `0..=2`). Clears any half-typed
    /// digits.
    pub fn focus_channel(&mut self, channel: usize) {
        self.channel = channel.min(CHANNELS - 1);
        self.buf.clear();
    }

    /// Focus the previous channel (saturating at red).
    pub fn focus_prev_channel(&mut self) {
        self.focus_channel(self.channel.saturating_sub(1));
    }

    /// Focus the next channel (saturating at blue).
    pub fn focus_next_channel(&mut self) {
        self.focus_channel(self.channel + 1);
    }

    /// Type a decimal digit into the focused channel. Digits accumulate
    /// (e.g. `2`,`5`,`5` → 255); a fourth digit starts a fresh number.
    /// Values above 255 are clamped. Non-digits are ignored.
    pub fn type_digit(&mut self, d: char) {
        if !d.is_ascii_digit() {
            return;
        }
        if self.buf.len() >= 3 {
            self.buf.clear();
        }
        self.buf.push(d);
        let n = self.buf.parse::<u16>().unwrap_or(0).min(255);
        self.rgb[self.channel] = n as u8;
    }

    /// Delete the last typed digit from the focused channel.
    pub fn backspace(&mut self) {
        self.buf.pop();
        let n = self.buf.parse::<u16>().unwrap_or(0).min(255);
        self.rgb[self.channel] = n as u8;
    }

    /// A convenient default key map, for hosts that don't want to bind keys
    /// themselves:
    ///
    /// - `Enter` → [`Accept`](ColorPickerAction::Accept)
    /// - `Esc` → [`Cancel`](ColorPickerAction::Cancel)
    /// - `Up`/`k`, `Down`/`j` → focus previous/next channel
    /// - `Left`/`Right` → ∓1 on the focused channel
    /// - `Ctrl`+`Left`/`Right`, `PageDown`/`PageUp` → ∓16
    /// - digits → type into the focused channel; `Backspace` deletes
    ///
    /// Returns what happened so you can commit/cancel/preview.
    pub fn handle_key(&mut self, key: KeyEvent) -> ColorPickerAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => ColorPickerAction::Accept,
            KeyCode::Esc => ColorPickerAction::Cancel,
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_prev_channel();
                ColorPickerAction::Changed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus_next_channel();
                ColorPickerAction::Changed
            }
            KeyCode::Left if ctrl => {
                self.adjust(-16);
                ColorPickerAction::Changed
            }
            KeyCode::Right if ctrl => {
                self.adjust(16);
                ColorPickerAction::Changed
            }
            KeyCode::Left => {
                self.adjust(-1);
                ColorPickerAction::Changed
            }
            KeyCode::Right => {
                self.adjust(1);
                ColorPickerAction::Changed
            }
            KeyCode::PageDown => {
                self.adjust(-16);
                ColorPickerAction::Changed
            }
            KeyCode::PageUp => {
                self.adjust(16);
                ColorPickerAction::Changed
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                self.type_digit(c);
                ColorPickerAction::Changed
            }
            KeyCode::Backspace => {
                self.backspace();
                ColorPickerAction::Changed
            }
            _ => ColorPickerAction::Ignored,
        }
    }

    /// Build a renderable [`Widget`] borrowing this picker, `style`, and
    /// `labels`. Render it into whatever inner [`Rect`] you like — you own
    /// the surrounding block/popup.
    pub fn widget<'a>(
        &'a self,
        style: &'a ColorPickerStyle,
        labels: &'a ColorPickerLabels<'a>,
    ) -> ColorPickerWidget<'a> {
        ColorPickerWidget {
            picker: self,
            style,
            labels,
        }
    }
}

/// Colours/styling for the picker. Every field has a sensible default so you
/// only override what you care about; set them from your own theme to match
/// the rest of your UI.
#[derive(Clone, Debug)]
pub struct ColorPickerStyle {
    /// An unfocused channel's label + marker.
    pub label: Style,
    /// The focused channel's label + marker.
    pub label_focused: Style,
    /// The filled/unfilled slider bar.
    pub bar: Style,
    /// The hex readout and the numeric channel value.
    pub value: Style,
    /// The optional hint line.
    pub hint: Style,
    /// Width, in cells, of each channel's slider bar.
    pub bar_width: usize,
}

impl Default for ColorPickerStyle {
    fn default() -> Self {
        Self {
            label: Style::default().add_modifier(Modifier::DIM),
            label_focused: Style::default().add_modifier(Modifier::BOLD),
            bar: Style::default(),
            value: Style::default(),
            hint: Style::default().add_modifier(Modifier::DIM),
            bar_width: 16,
        }
    }
}

/// Text labels for the picker (channel names and an optional hint), so it can
/// be localized. Defaults to `"R"`/`"G"`/`"B"` and no hint.
#[derive(Clone, Debug)]
pub struct ColorPickerLabels<'a> {
    /// The three channel labels, in `[red, green, blue]` order.
    pub channels: [&'a str; 3],
    /// An optional hint line rendered below the channels (e.g. describing
    /// your keybindings). Wrapped to fit; omit to save a row.
    pub hint: Option<&'a str>,
}

impl Default for ColorPickerLabels<'static> {
    fn default() -> Self {
        Self {
            channels: ["R", "G", "B"],
            hint: None,
        }
    }
}

/// A borrowing, renderable view of a [`ColorPicker`]. Create it with
/// [`ColorPicker::widget`].
pub struct ColorPickerWidget<'a> {
    picker: &'a ColorPicker,
    style: &'a ColorPickerStyle,
    labels: &'a ColorPickerLabels<'a>,
}

impl ColorPickerWidget<'_> {
    /// Render the picker's contents into `area` of `buf`. Equivalent to
    /// [`Widget::render`], but takes `&self` so the widget value can be
    /// reused. Handy with a raw [`Buffer`]; with a `Frame`, prefer
    /// `frame.render_widget(picker.widget(&style, &labels), area)`.
    pub fn render_into(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let p = self.picker;
        let st = self.style;

        // swatch + hex, spacer, one row per channel, then the rest for the hint.
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);

        let [r, g, b] = p.rgb;
        let color = Color::Rgb(r, g, b);
        Paragraph::new(Line::from(vec![
            Span::styled("      ", Style::default().bg(color)),
            Span::raw("  "),
            Span::styled(p.hex(), st.value),
        ]))
        .render(rows[0], buf);

        let bar_w = st.bar_width.max(1);
        for ch in 0..CHANNELS {
            let value = p.rgb[ch] as usize;
            let filled = value * bar_w / 255;
            let bar: String = "█".repeat(filled) + &"░".repeat(bar_w - filled);
            let focused = p.channel == ch;
            let marker = if focused { "›" } else { " " };
            let label_style = if focused { st.label_focused } else { st.label };
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("{marker} {} ", self.labels.channels[ch]),
                    label_style,
                ),
                Span::styled(bar, st.bar),
                Span::styled(format!(" {value:>3}"), st.value),
            ]))
            .render(rows[2 + ch], buf);
        }

        if let Some(hint) = self.labels.hint
            && rows[5].height > 0
        {
            Paragraph::new(Line::from(Span::styled(hint, st.hint)))
                .wrap(Wrap { trim: true })
                .render(rows[5], buf);
        }
    }
}

impl Widget for ColorPickerWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.render_into(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjust_clamps_and_targets_the_focused_channel() {
        let mut p = ColorPicker::new([10, 20, 30]);
        p.adjust(5);
        assert_eq!(p.rgb(), [15, 20, 30], "red is focused by default");
        p.adjust(-100);
        assert_eq!(p.rgb(), [0, 20, 30], "clamped at 0");
        p.focus_next_channel();
        p.adjust(300);
        assert_eq!(p.rgb(), [0, 255, 30], "green clamped at 255");
    }

    #[test]
    fn channel_focus_saturates_at_both_ends() {
        let mut p = ColorPicker::new([0, 0, 0]);
        p.focus_prev_channel();
        assert_eq!(p.channel(), 0, "can't go before red");
        p.focus_next_channel();
        p.focus_next_channel();
        p.focus_next_channel();
        assert_eq!(p.channel(), 2, "can't go past blue");
    }

    #[test]
    fn typing_digits_accumulates_then_restarts_after_three() {
        let mut p = ColorPicker::new([0, 0, 0]);
        p.type_digit('2');
        p.type_digit('5');
        p.type_digit('5');
        assert_eq!(p.rgb()[0], 255);
        // A fourth digit starts a new number.
        p.type_digit('7');
        assert_eq!(p.rgb()[0], 7);
        // Over-255 clamps.
        p.type_digit('9');
        p.type_digit('9');
        assert_eq!(p.rgb()[0], 255, "79->799 clamps to 255");
    }

    #[test]
    fn backspace_walks_the_typed_number_back_down() {
        let mut p = ColorPicker::new([0, 0, 0]);
        p.type_digit('1');
        p.type_digit('2');
        p.type_digit('8'); // 128
        p.backspace(); // "12"
        assert_eq!(p.rgb()[0], 12);
        p.backspace(); // "1"
        assert_eq!(p.rgb()[0], 1);
    }

    #[test]
    fn adjusting_clears_any_half_typed_digits() {
        let mut p = ColorPicker::new([0, 0, 0]);
        p.type_digit('1'); // buf = "1"
        p.adjust(1); // 1 -> 2, buf cleared
        assert_eq!(p.rgb()[0], 2);
        p.type_digit('9'); // fresh number, not "19"
        assert_eq!(p.rgb()[0], 9);
    }

    #[test]
    fn hex_is_uppercase_and_zero_padded() {
        assert_eq!(ColorPicker::new([0, 15, 255]).hex(), "#000FFF");
    }

    #[test]
    fn revert_restores_the_opening_colour() {
        let mut p = ColorPicker::new([100, 100, 100]);
        p.adjust(50);
        assert_eq!(p.rgb(), [150, 100, 100]);
        p.revert();
        assert_eq!(p.rgb(), [100, 100, 100]);
        assert_eq!(p.original(), [100, 100, 100]);
    }

    #[test]
    fn default_key_map_reports_accept_cancel_and_changes() {
        let mut p = ColorPicker::new([0, 0, 0]);
        assert_eq!(
            p.handle_key(KeyEvent::from(KeyCode::Enter)),
            ColorPickerAction::Accept
        );
        assert_eq!(
            p.handle_key(KeyEvent::from(KeyCode::Esc)),
            ColorPickerAction::Cancel
        );
        assert_eq!(
            p.handle_key(KeyEvent::from(KeyCode::Right)),
            ColorPickerAction::Changed
        );
        assert_eq!(p.rgb()[0], 1);
        // Ctrl+Right steps by 16.
        let ctrl_right = KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL);
        assert_eq!(p.handle_key(ctrl_right), ColorPickerAction::Changed);
        assert_eq!(p.rgb()[0], 17);
        assert_eq!(
            p.handle_key(KeyEvent::from(KeyCode::Char('x'))),
            ColorPickerAction::Ignored
        );
    }

    #[test]
    fn renders_a_swatch_hex_and_three_bars_without_panicking() {
        let picker = ColorPicker::new([255, 0, 128]);
        let style = ColorPickerStyle::default();
        let labels = ColorPickerLabels {
            channels: ["Red", "Green", "Blue"],
            hint: Some("arrows adjust, Enter accepts"),
        };
        let area = Rect::new(0, 0, 30, 7);
        let mut buf = Buffer::empty(area);
        picker.widget(&style, &labels).render_into(area, &mut buf);
        let rendered = buffer_text(&buf);
        assert!(rendered.contains("#FF0080"), "shows the hex value");
        assert!(rendered.contains("Red"), "shows a channel label");
        assert!(rendered.contains('›'), "marks the focused channel");
    }

    fn buffer_text(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }
}
