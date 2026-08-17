//! The Theme editor overlay (Settings → Theme): pick a built-in preset or a
//! saved custom theme, then tweak a custom theme's colours in place (edits
//! auto-save). New custom themes are created through a small popup that names
//! the theme and picks an existing one to copy its colours from; individual
//! colours are edited in a dedicated R/G/B picker popup. Built-in presets and
//! "Automatic" are read-only. A live whole-app preview is driven by
//! [`ThemeEditorState::draft`].

use super::listscroll::ListScroll;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, Paragraph};

use super::app::{MouseHitTarget, MouseLayer, MouseScrollTarget, TuiApp};
use super::draw::{centered_rect, panel};
use super::editor::{Editor, render_line_field};
use super::theme::{THEME_COLOR_COUNT, Theme, ThemeSpec};
use crate::i18n::Strings;
use tui_rgb_picker::{ColorPicker, ColorPickerLabels, ColorPickerStyle};

/// Which pane of the Theme editor has focus.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ThemePane {
    /// The list of selectable themes (Automatic + presets + customs).
    List,
    /// The colour rows of the selected (custom) theme.
    Fields,
}

/// Which field of the "New theme" popup has focus.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum NewThemeFocus {
    /// The name text input.
    Name,
    /// The "base it on" theme list.
    Base,
}

/// The "New theme" popup shown over the editor: name the theme and choose an
/// existing one to copy colours from.
pub(crate) struct NewThemeState {
    pub(crate) name: Editor,
    /// Index into [`TuiApp::all_themes`] of the theme to copy colours from.
    pub(crate) base_idx: usize,
    pub(crate) focus: NewThemeFocus,
    /// Where the base-theme list is scrolled to (a user with many custom
    /// themes can overflow the popup's few rows).
    pub(crate) base_scroll: ListScroll,
}

/// The R/G/B picker popup for editing a single theme colour. Wraps the
/// reusable [`ColorPicker`] (from the `tui-rgb-picker` crate) with the one
/// piece of context the crate stays agnostic about: *which* theme colour
/// (`0..THEME_COLOR_COUNT`) is being edited.
pub(crate) struct ColorPopup {
    /// Which theme colour (`0..THEME_COLOR_COUNT`) is being edited.
    pub(crate) color_idx: usize,
    /// The reusable picker state (working colour, focused channel, original).
    pub(crate) picker: ColorPicker,
}

pub(crate) struct ThemeEditorState {
    pub(crate) pane: ThemePane,
    /// Index into the picker list: `0` = "Automatic (match language)", then one
    /// row per [`TuiApp::all_themes`] entry (presets first, then customs).
    pub(crate) list_idx: usize,
    /// Focused colour row in the Fields pane (`0..THEME_COLOR_COUNT`).
    pub(crate) field: usize,
    /// When true, the Fields-pane cursor is on the name row above the colours
    /// (rather than on colour `field`).
    pub(crate) name_focused: bool,
    /// Editable name of the selected custom theme; renames auto-apply as you
    /// type (see [`TuiApp::theme_commit_name`]).
    pub(crate) name: Editor,
    /// The always-valid working colours, used for the live preview and written
    /// back to the custom theme on each committed colour edit.
    pub(crate) draft: ThemeSpec,
    /// When `Some`, the "New theme" popup is open and takes all key input.
    /// Where the theme list is scrolled to, carried between frames (see
    /// [`ListScroll`]).
    pub(crate) list_scroll: ListScroll,
    pub(crate) new_popup: Option<NewThemeState>,
    /// When `Some`, the colour picker popup is open and takes all key input.
    pub(crate) color_popup: Option<ColorPopup>,
}

impl ThemeEditorState {
    pub(crate) fn new(spec: &ThemeSpec) -> Self {
        let mut st = Self {
            pane: ThemePane::List,
            list_idx: 0,
            field: 0,
            name_focused: false,
            name: Editor::blank(),
            draft: spec.clone(),
            list_scroll: ListScroll::default(),
            new_popup: None,
            color_popup: None,
        };
        st.load_spec(spec);
        st
    }

    /// Load a theme into the working draft (preview value).
    pub(crate) fn load_spec(&mut self, spec: &ThemeSpec) {
        self.draft = spec.clone();
        self.field = self.field.min(THEME_COLOR_COUNT - 1);
        self.sync_name();
    }

    /// Reset the name editor to the draft's stored name, discarding any
    /// uncommitted (invalid) text left in the field.
    pub(crate) fn sync_name(&mut self) {
        self.name = Editor::new(&self.draft.name, false);
    }
}

/// The localised label for the `i`th theme colour, matching [`ThemeSpec::color`]
/// order.
pub(crate) fn color_label(s: &Strings, i: usize) -> &'static str {
    match i {
        0 => s.theme_c_bg,
        1 => s.theme_c_panel,
        2 => s.theme_c_text,
        3 => s.theme_c_dim,
        4 => s.theme_c_accent,
        5 => s.theme_c_ok,
        6 => s.theme_c_err,
        7 => s.theme_c_subst,
        8 => s.theme_c_pending,
        9 => s.theme_c_select_bg,
        _ => s.theme_c_select_fg,
    }
}

/// `#RRGGBB` (uppercase) for display.
fn hex_display([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// Draw the Theme editor overlay. `entries` are the picker rows (index 0 is the
/// "Automatic" label), computed by the caller from the current app state.
pub(crate) fn draw_theme_editor(
    f: &mut Frame,
    st: &ThemeEditorState,
    entries: &[String],
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    let width = 64u16;
    let inner_h = THEME_COLOR_COUNT as u16 + 1; // a name row above one row per colour
    let height = inner_h + 2 + 2; // borders + a hint line + its spacer
    let area = centered_rect(width, height.min(f.area().height.max(6)), f.area());
    f.render_widget(Clear, area);

    let block = panel(s.theme_editor_title.to_string(), true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(inner);
    let cols = Layout::horizontal([Constraint::Length(24), Constraint::Min(20)]).split(rows[0]);

    draw_picker(f, cols[0], st, entries, th);
    draw_fields(f, cols[1], st, s, th);
    if let Some(app) = app {
        app.set_mouse_layer(MouseLayer::Overlay);
        let list_inner = Rect {
            x: cols[0].x.saturating_add(1),
            y: cols[0].y.saturating_add(1),
            width: cols[0].width.saturating_sub(2),
            height: cols[0].height.saturating_sub(2),
        };
        app.push_mouse_hit(
            MouseLayer::Overlay,
            list_inner,
            MouseHitTarget::Scroll(MouseScrollTarget::ThemeEditor),
        );
        let visible = list_inner.height as usize;
        let selected = st.list_idx.min(entries.len().saturating_sub(1));
        let first = if selected >= visible {
            selected + 1 - visible
        } else {
            0
        };
        for row in first..entries.len().min(first + visible) {
            app.push_mouse_hit(
                MouseLayer::Overlay,
                Rect::new(
                    list_inner.x,
                    list_inner.y + (row - first) as u16,
                    list_inner.width,
                    1,
                ),
                MouseHitTarget::ThemeEditorRow(row),
            );
        }
        let fields_inner = Rect {
            x: cols[1].x.saturating_add(1),
            y: cols[1].y.saturating_add(1),
            width: cols[1].width.saturating_sub(2),
            height: cols[1].height.saturating_sub(2),
        };
        app.push_mouse_hit(
            MouseLayer::Overlay,
            Rect::new(fields_inner.x, fields_inner.y, fields_inner.width, 1),
            MouseHitTarget::ThemeEditorColor(THEME_COLOR_COUNT),
        );
        for row in 0..THEME_COLOR_COUNT.min(fields_inner.height.saturating_sub(1) as usize) {
            app.push_mouse_hit(
                MouseLayer::Overlay,
                Rect::new(
                    fields_inner.x,
                    fields_inner.y + 1 + row as u16,
                    fields_inner.width,
                    1,
                ),
                MouseHitTarget::ThemeEditorColor(row),
            );
        }
    }

    let hint = Paragraph::new(Line::from(Span::styled(
        if st.pane == ThemePane::Fields {
            s.theme_fields_hint
        } else {
            s.theme_editor_hint
        },
        Style::default().fg(th.dim),
    )));
    f.render_widget(hint, rows[1]);

    // A popup (only ever one at a time) floats over everything else.
    if let Some(popup) = &st.new_popup {
        let base_names = entries.get(1..).unwrap_or(&[]);
        draw_new_theme_popup(f, popup, base_names, s, th);
    } else if let Some(cp) = &st.color_popup {
        let inner = draw_color_popup(f, cp, s, th);
        if let Some(app) = app {
            app.set_mouse_layer(MouseLayer::Popup);
            app.push_mouse_hit(
                MouseLayer::Popup,
                inner,
                MouseHitTarget::ThemeEditorColorChoice(0),
            );
        }
    }
}

fn draw_picker(f: &mut Frame, area: Rect, st: &ThemeEditorState, entries: &[String], th: &Theme) {
    let focused = st.pane == ThemePane::List;
    let items: Vec<ListItem> = entries
        .iter()
        .map(|e| {
            ListItem::new(Line::from(Span::styled(
                e.clone(),
                Style::default().fg(th.text),
            )))
        })
        .collect();
    let highlight = if focused {
        Style::default()
            .bg(th.accent)
            .fg(th.bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(th.text)
            .add_modifier(Modifier::REVERSED)
    };
    let list = List::new(items)
        .block(panel("".to_string(), focused, th))
        .highlight_style(highlight)
        .highlight_symbol("› ");
    st.list_scroll
        .render(f, area, list, Some(st.list_idx), entries.len());
}

fn draw_fields(f: &mut Frame, area: Rect, st: &ThemeEditorState, s: &Strings, th: &Theme) {
    let focused = st.pane == ThemePane::Fields;
    let block = panel("".to_string(), focused, th);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1), // editable name
        Constraint::Min(1),    // colour rows
    ])
    .split(inner);

    // Editable theme name, with a blinking cursor when it holds focus.
    let name_focused = focused && st.name_focused;
    let name_label = format!("{}: ", s.theme_name_label);
    let lw = name_label.chars().count() as u16;
    let name_cols = Layout::horizontal([Constraint::Length(lw), Constraint::Min(1)]).split(rows[0]);
    f.render_widget(
        Paragraph::new(Span::styled(name_label, Style::default().fg(th.dim))),
        name_cols[0],
    );
    render_line_field(f, name_cols[1], &st.name, name_focused, false, th);

    let mut lines: Vec<Line> = Vec::with_capacity(THEME_COLOR_COUNT);

    let row_style = |on: bool| {
        if on {
            Style::default()
                .bg(th.accent)
                .fg(th.bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.text)
        }
    };

    // Pad every label to the widest one so the swatches (and hex values) line
    // up in a column regardless of how long individual colour names are.
    let label_w = (0..THEME_COLOR_COUNT)
        .map(|i| color_label(s, i).chars().count())
        .max()
        .unwrap_or(0);

    for i in 0..THEME_COLOR_COUNT {
        let rgb = st.draft.color(i);
        let swatch = Span::styled(
            "  ",
            Style::default().bg(Color::Rgb(rgb[0], rgb[1], rgb[2])),
        );
        let on = focused && !st.name_focused && st.field == i;
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<label_w$} ", color_label(s, i)),
                Style::default().fg(th.dim),
            ),
            swatch,
            Span::raw(" "),
            Span::styled(hex_display(rgb), row_style(on)),
        ]));
    }

    f.render_widget(Paragraph::new(lines), rows[1]);
}

fn draw_new_theme_popup(
    f: &mut Frame,
    popup: &NewThemeState,
    base_names: &[String],
    s: &Strings,
    th: &Theme,
) {
    let width = 46u16;
    let list_h = (base_names.len() as u16).clamp(1, 8);
    let height = (list_h + 6).min(f.area().height.max(6));
    let area = centered_rect(width, height, f.area());
    f.render_widget(Clear, area);

    let block = panel(s.theme_new_title.to_string(), true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1), // name
        Constraint::Length(1), // "Base on:" label
        Constraint::Min(1),    // base list
        Constraint::Length(1), // hint
    ])
    .split(inner);

    // Name label + a real line editor (so the terminal cursor blinks in it).
    let name_focused = popup.focus == NewThemeFocus::Name;
    let name_label = format!("{}: ", s.theme_name_label);
    let lw = name_label.chars().count() as u16;
    let name_cols = Layout::horizontal([Constraint::Length(lw), Constraint::Min(1)]).split(rows[0]);
    f.render_widget(
        Paragraph::new(Span::styled(name_label, Style::default().fg(th.dim))),
        name_cols[0],
    );
    render_line_field(f, name_cols[1], &popup.name, name_focused, false, th);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{}:", s.theme_new_base),
            Style::default().fg(th.dim),
        ))),
        rows[1],
    );

    let base_focused = popup.focus == NewThemeFocus::Base;
    let items: Vec<ListItem> = base_names
        .iter()
        .map(|b| {
            ListItem::new(Line::from(Span::styled(
                b.clone(),
                Style::default().fg(th.text),
            )))
        })
        .collect();
    let hl = if base_focused {
        Style::default()
            .bg(th.accent)
            .fg(th.bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(th.text)
            .add_modifier(Modifier::REVERSED)
    };
    let list = List::new(items).highlight_style(hl).highlight_symbol("› ");
    popup
        .base_scroll
        .render(f, rows[2], list, Some(popup.base_idx), base_names.len());

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            s.theme_new_popup_hint,
            Style::default().fg(th.dim),
        ))),
        rows[3],
    );
}

fn draw_color_popup(f: &mut Frame, cp: &ColorPopup, s: &Strings, th: &Theme) -> Rect {
    let width = 46u16;
    let height = 9u16.min(f.area().height.max(6));
    let area = centered_rect(width, height, f.area());
    f.render_widget(Clear, area);

    let block = panel(color_label(s, cp.color_idx).to_string(), true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // The swatch/hex/slider layout and input live in the reusable
    // `tui-rgb-picker` crate; here we only supply PaperBoy's theme colours,
    // localized channel labels, and its keybinding hint.
    let style = ColorPickerStyle {
        label: Style::default().fg(th.dim),
        label_focused: Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        bar: Style::default().fg(th.accent),
        value: Style::default().fg(th.text),
        hint: Style::default().fg(th.dim),
        bar_width: 16,
    };
    let labels = ColorPickerLabels {
        channels: [s.theme_ch_red, s.theme_ch_green, s.theme_ch_blue],
        hint: Some(s.theme_color_popup_hint),
    };
    f.render_widget(cp.picker.widget(&style, &labels), inner);
    inner
}

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::Overlay;
use super::theme::{is_builtin, preset_for_language};
use crate::i18n::Status;

impl TuiApp {
    /// The picker rows shown in the Theme editor: the "Automatic" row first,
    /// then every theme (presets, then customs).
    pub(crate) fn theme_picker_entries(&self, s: &Strings) -> Vec<String> {
        let mut entries = vec![s.theme_auto.to_string()];
        entries.extend(self.all_themes().into_iter().map(|t| t.name));
        entries
    }

    pub(crate) fn theme_picker_len(&self) -> usize {
        1 + self.all_themes().len()
    }

    /// The picker index of a theme by name (`0` is Automatic, so themes start
    /// at `1`).
    fn theme_index_of(&self, name: &str) -> Option<usize> {
        self.all_themes()
            .iter()
            .position(|t| t.name == name)
            .map(|p| p + 1)
    }

    /// Whether the currently-selected picker row is an editable custom theme
    /// (Automatic and the built-in presets are read-only).
    fn theme_selection_editable(&self, st: &ThemeEditorState) -> bool {
        st.list_idx > 0
            && self
                .all_themes()
                .get(st.list_idx - 1)
                .is_some_and(|t| !is_builtin(&t.name))
    }

    /// Open the Theme editor, selecting whatever theme is currently in effect.
    pub(crate) fn open_theme_editor(&mut self) {
        let spec = self.active_theme_spec();
        let mut st = ThemeEditorState::new(&spec);
        st.list_idx = match &self.active_theme {
            None => 0,
            Some(name) => self.theme_index_of(name).unwrap_or(0),
        };
        self.overlay = Some(Overlay::ThemeEditor(st));
    }

    /// Apply the picker's current selection: `0` follows the language
    /// (Automatic), any other row activates that theme. Loads the choice into
    /// the draft and persists.
    fn theme_apply_selection(&mut self, st: &mut ThemeEditorState) {
        let spec = if st.list_idx == 0 {
            self.active_theme = None;
            preset_for_language(&self.language)
        } else {
            let themes = self.all_themes();
            match themes.get(st.list_idx - 1) {
                Some(spec) => {
                    self.active_theme = Some(spec.name.clone());
                    spec.clone()
                }
                None => return,
            }
        };
        st.load_spec(&spec);
        self.save_state();
    }

    /// Try to move focus into the colour rows. Only custom themes are editable;
    /// Automatic and presets show a read-only hint and stay put.
    fn theme_enter_fields(&mut self, st: &mut ThemeEditorState) {
        if self.theme_selection_editable(st) {
            st.pane = ThemePane::Fields;
            st.field = 0;
            st.name_focused = false;
            st.sync_name();
        } else {
            self.status = Some(Status::ThemePresetReadonly);
        }
    }

    /// Apply the name typed in the Fields-pane name row to the selected custom
    /// theme, renaming it in place. Called when focus leaves the name row. The
    /// rename only sticks if the new name is non-empty, isn't a reserved preset
    /// name, and doesn't collide with another theme; otherwise the field is
    /// reverted to the theme's current name.
    fn theme_commit_name(&mut self, st: &mut ThemeEditorState) {
        let new = st.name.text();
        let new = new.trim().to_string();
        let old = st.draft.name.clone();
        if new != old
            && !new.is_empty()
            && !is_builtin(&new)
            && !self.custom_themes.iter().any(|t| t.name == new)
        {
            if let Some(existing) = self.custom_themes.iter_mut().find(|t| t.name == old) {
                existing.name = new.clone();
            }
            if self.active_theme.as_deref() == Some(old.as_str()) {
                self.active_theme = Some(new.clone());
            }
            st.draft.name = new;
            self.save_state();
        }
        // Reflect the committed (or unchanged) name back into the editor,
        // discarding any rejected text.
        st.sync_name();
    }

    /// Write the working draft back to the selected custom theme and persist —
    /// custom-theme colour edits save themselves as you make them.
    fn theme_persist_draft(&mut self, st: &ThemeEditorState) {
        if let Some(existing) = self
            .custom_themes
            .iter_mut()
            .find(|t| t.name == st.draft.name)
        {
            *existing = st.draft.clone();
            self.save_state();
        }
    }

    /// Open the "New theme" popup, defaulting the base to whatever theme is
    /// currently selected (or the first preset).
    fn theme_open_new_popup(&mut self, st: &mut ThemeEditorState) {
        let base_idx = st
            .list_idx
            .saturating_sub(1)
            .min(self.all_themes().len().saturating_sub(1));
        st.new_popup = Some(NewThemeState {
            name: Editor::blank(),
            base_idx,
            focus: NewThemeFocus::Name,
            base_scroll: ListScroll::default(),
        });
    }

    /// Open the colour picker popup for the focused colour row.
    fn theme_open_color_popup(&mut self, st: &mut ThemeEditorState) {
        let rgb = st.draft.color(st.field);
        st.color_popup = Some(ColorPopup {
            color_idx: st.field,
            picker: ColorPicker::new(rgb),
        });
    }

    /// Create the theme described by the "New theme" popup: copy the chosen
    /// base's colours under the new name, activate it, select it, and drop into
    /// the colour rows ready to edit.
    fn theme_new_confirm(&mut self, st: &mut ThemeEditorState) {
        let (name, base_idx) = match &st.new_popup {
            Some(p) => (p.name.text().trim().to_string(), p.base_idx),
            None => return,
        };
        if name.is_empty() {
            self.status = Some(Status::ThemeNameRequired);
            return;
        }
        if is_builtin(&name) {
            self.status = Some(Status::ThemeNameReserved);
            return;
        }
        if self.custom_themes.iter().any(|t| t.name == name) {
            self.status = Some(Status::ThemeNameTaken);
            return;
        }
        let Some(base) = self.all_themes().get(base_idx).cloned() else {
            return;
        };
        let mut spec = base;
        spec.name = name.clone();
        self.custom_themes.push(spec.clone());
        self.active_theme = Some(name.clone());
        self.save_state();
        st.new_popup = None;
        st.list_idx = self.theme_index_of(&name).unwrap_or(0);
        st.load_spec(&spec);
        st.pane = ThemePane::Fields;
        st.field = 0;
        st.name_focused = false;
        self.status = Some(Status::ThemeSaved(name));
    }

    /// Delete the picker's selected custom theme. Automatic and the built-in
    /// presets can't be deleted.
    fn theme_editor_delete(&mut self, st: &mut ThemeEditorState) {
        if st.list_idx == 0 {
            self.status = Some(Status::ThemeCannotDelete);
            return;
        }
        let themes = self.all_themes();
        let Some(target) = themes.get(st.list_idx - 1).cloned() else {
            return;
        };
        if is_builtin(&target.name) {
            self.status = Some(Status::ThemeCannotDelete);
            return;
        }
        self.custom_themes.retain(|t| t.name != target.name);
        self.custom_themes.retain(|t| t.name != target.name);
        // Focus the row just above the one we deleted, then activate it so the
        // draft/preview follow (rather than snapping back to the top).
        st.pane = ThemePane::List;
        st.list_idx -= 1;
        self.theme_apply_selection(st);
        self.status = Some(Status::ThemeDeleted(target.name));
    }

    /// Key handling for the "New theme" popup (name + base picker).
    fn on_key_new_theme_popup(&mut self, st: &mut ThemeEditorState, key: KeyEvent) {
        let base_len = self.all_themes().len();
        match key.code {
            KeyCode::Esc => {
                st.new_popup = None;
                return;
            }
            KeyCode::Enter => {
                self.theme_new_confirm(st);
                return;
            }
            _ => {}
        }
        let Some(popup) = st.new_popup.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                popup.focus = match popup.focus {
                    NewThemeFocus::Name => NewThemeFocus::Base,
                    NewThemeFocus::Base => NewThemeFocus::Name,
                };
            }
            _ => match popup.focus {
                NewThemeFocus::Name => match key.code {
                    KeyCode::Down => popup.focus = NewThemeFocus::Base,
                    KeyCode::Left => popup.name.left(),
                    KeyCode::Right => popup.name.right(),
                    KeyCode::Home => popup.name.home(),
                    KeyCode::End => popup.name.end(),
                    KeyCode::Backspace => popup.name.backspace(),
                    KeyCode::Char(c) if !c.is_control() => popup.name.insert(c),
                    _ => {}
                },
                NewThemeFocus::Base => match key.code {
                    KeyCode::Up => {
                        if popup.base_idx > 0 {
                            popup.base_idx -= 1;
                        } else {
                            popup.focus = NewThemeFocus::Name;
                        }
                    }
                    KeyCode::Down => {
                        popup.base_idx = (popup.base_idx + 1).min(base_len.saturating_sub(1));
                    }
                    _ => {}
                },
            },
        }
    }

    /// Key handling for the R/G/B colour picker popup.
    fn on_key_color_popup(&mut self, st: &mut ThemeEditorState, key: KeyEvent) {
        {
            let Some(cp) = st.color_popup.as_mut() else {
                return;
            };
            match key.code {
                KeyCode::Esc => {
                    let (idx, orig) = (cp.color_idx, cp.picker.original());
                    st.draft.set_color(idx, orig);
                    st.color_popup = None;
                    return;
                }
                KeyCode::Enter => {
                    let (idx, rgb) = (cp.color_idx, cp.picker.rgb());
                    st.draft.set_color(idx, rgb);
                    st.color_popup = None;
                    self.theme_persist_draft(st);
                    return;
                }
                KeyCode::Up | KeyCode::Char('k') => cp.picker.focus_prev_channel(),
                KeyCode::Down | KeyCode::Char('j') => cp.picker.focus_next_channel(),
                KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    cp.picker.adjust(-16)
                }
                KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    cp.picker.adjust(16)
                }
                KeyCode::Left => cp.picker.adjust(-1),
                KeyCode::Right => cp.picker.adjust(1),
                KeyCode::PageDown => cp.picker.adjust(-16),
                KeyCode::PageUp => cp.picker.adjust(16),
                KeyCode::Char(c) if c.is_ascii_digit() => cp.picker.type_digit(c),
                KeyCode::Backspace => cp.picker.backspace(),
                _ => {}
            }
        }
        // Live whole-app preview of the in-progress colour.
        if let Some(cp) = &st.color_popup {
            let (idx, rgb) = (cp.color_idx, cp.picker.rgb());
            st.draft.set_color(idx, rgb);
        }
    }

    pub(crate) fn on_key_theme_editor(&mut self, mut st: ThemeEditorState, key: KeyEvent) {
        if st.new_popup.is_some() {
            self.on_key_new_theme_popup(&mut st, key);
            self.overlay = Some(Overlay::ThemeEditor(st));
            return;
        }
        if st.color_popup.is_some() {
            self.on_key_color_popup(&mut st, key);
            self.overlay = Some(Overlay::ThemeEditor(st));
            return;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                if st.pane == ThemePane::Fields && st.name_focused {
                    self.theme_commit_name(&mut st);
                }
                self.overlay = None;
                return;
            }
            KeyCode::Char('n') if ctrl => self.theme_open_new_popup(&mut st),
            KeyCode::Char('d') if ctrl => self.theme_editor_delete(&mut st),
            KeyCode::Tab | KeyCode::BackTab => match st.pane {
                ThemePane::List => self.theme_enter_fields(&mut st),
                ThemePane::Fields => {
                    if st.name_focused {
                        self.theme_commit_name(&mut st);
                    }
                    st.pane = ThemePane::List;
                }
            },
            _ => match st.pane {
                ThemePane::List => match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        if st.list_idx > 0 {
                            st.list_idx -= 1;
                            self.theme_apply_selection(&mut st);
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if st.list_idx + 1 < self.theme_picker_len() {
                            st.list_idx += 1;
                            self.theme_apply_selection(&mut st);
                        }
                    }
                    KeyCode::Enter | KeyCode::Right => self.theme_enter_fields(&mut st),
                    _ => {}
                },
                ThemePane::Fields if st.name_focused => match key.code {
                    KeyCode::Down => {
                        self.theme_commit_name(&mut st);
                        st.name_focused = false;
                        st.field = 0;
                    }
                    KeyCode::Up => {}
                    KeyCode::Left => {
                        if st.name.col == 0 {
                            self.theme_commit_name(&mut st);
                            st.pane = ThemePane::List;
                        } else {
                            st.name.left();
                        }
                    }
                    KeyCode::Enter => self.theme_commit_name(&mut st),
                    KeyCode::Right => st.name.right(),
                    KeyCode::Home => st.name.home(),
                    KeyCode::End => st.name.end(),
                    KeyCode::Backspace => st.name.backspace(),
                    KeyCode::Char(c) if !c.is_control() => st.name.insert(c),
                    _ => {}
                },
                ThemePane::Fields => match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        if st.field == 0 {
                            st.name_focused = true;
                        } else {
                            st.field -= 1;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        st.field = (st.field + 1).min(THEME_COLOR_COUNT - 1)
                    }
                    KeyCode::Left => st.pane = ThemePane::List,
                    KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => {
                        self.theme_open_color_popup(&mut st)
                    }
                    _ => {}
                },
            },
        }
        self.overlay = Some(Overlay::ThemeEditor(st));
    }
}
