//! Small shared egui widgets used across the GUI panels.

use eframe::egui::{self, Color32, RichText};

use crate::i18n::Strings;

use super::theme::{GuiTheme, method_color};

pub const METHODS: [&str; 8] = [
    "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE",
];

/// Width for the **key** cell of a two-column key/value row so the key grows
/// with the panel instead of staying a fixed sliver next to a filling value.
///
/// A filling value column (the grid's last column, `desired_width(INFINITY)`)
/// otherwise eats *all* free width, so a fixed-width key reads as "tiny key,
/// huge value". Instead we reserve the row's fixed controls (`reserved`:
/// checkbox, remove ✕, column spacing) and split the remaining free width
/// ~40% key / ~60% value — the value still fills whatever is left. The result
/// is clamped so the key never collapses to nothing and never grows past ~half
/// the free space. (The `.min(max_key)` on the lower bound keeps
/// [`f32::clamp`] from panicking when a cramped panel makes `max_key < 90`.)
///
/// Call this **before** building the grid: inside a grid cell `available_width`
/// reports the column width, not the panel width.
pub fn split_key_width(ui: &egui::Ui, reserved: f32) -> f32 {
    let usable = (ui.available_width() - reserved).max(120.0);
    let max_key = usable * 0.5;
    (usable * 0.40).clamp(90.0_f32.min(max_key), max_key)
}

/// A key/value row's **key** text field, forced to exactly `key_w` wide.
///
/// A bare `TextEdit::singleline` clamps its `desired_width` to the cell's
/// `available_width`, which stays tiny for a non-last grid column (the last,
/// filling column grabs the row's free width first). So `desired_width(key_w)`
/// alone renders as a ~24px sliver. [`egui::Ui::add_sized`] instead *allocates*
/// a `key_w`-wide rect up front and fits the field to it, so the key honours
/// the [`split_key_width`] split regardless of the grid's column feedback.
pub fn sized_key(ui: &mut egui::Ui, key_w: f32, text: &mut String, hint: &str) -> egui::Response {
    let h = ui.spacing().interact_size.y;
    ui.add_sized([key_w, h], egui::TextEdit::singleline(text).hint_text(hint))
}

/// A selectable label whose footprint never changes between the
/// unselected, hovered and selected states.
///
/// `egui`'s built-in [`egui::Ui::selectable_label`] omits the button frame
/// while inactive and only adds it on hover/selection, so the extra padding +
/// stroke make the widget (and everything after it) jump by a pixel or two the
/// moment the pointer touches it. We force `frame_when_inactive(true)` so the
/// frame's space is always reserved and hover/selection only *recolour* it in
/// place — matching the terminal UI, where selection never shifts the layout.
pub fn selectable<'a>(
    ui: &mut egui::Ui,
    selected: bool,
    atoms: impl egui::IntoAtoms<'a>,
) -> egui::Response {
    ui.add(egui::Button::selectable(selected, atoms).frame_when_inactive(true))
}

/// A panel header: a bold, **truncating** title on the left and right-aligned
/// action buttons that stay fully visible.
///
/// The title yields space to the buttons and truncates rather than growing, so
/// the header never demands more width than the panel's `min_size`. That
/// matters because the left column's headers sit outside its scroll area: if
/// one forced the content wider than a dragged-narrow panel, `egui` would clip
/// the content to the drag width while still placing the neighbouring panel at
/// the wider content edge — leaving an unpainted strip during the drag.
pub fn panel_header(
    ui: &mut egui::Ui,
    theme: &GuiTheme,
    title: impl Into<String>,
    add_buttons: impl FnOnce(&mut egui::Ui),
) {
    let title = title.into();
    ui.horizontal(|ui| {
        // Buttons are laid out from the right first (so they always fit), then
        // the title fills whatever space is left and truncates within it.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            add_buttons(ui);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(
                    egui::Label::new(RichText::new(title).strong().color(theme.text)).truncate(),
                );
            });
        });
    });
}

/// A collapsible tree row whose header **truncates** to the available width and
/// toggles when clicked anywhere along the row. Returns the header response.
///
/// egui's built-in [`egui::CollapsingHeader`] hardcodes `TextWrapMode::Extend`
/// for its header galley, so a long folder / environment name reports a content
/// width wider than the panel. While the splitter is dragged narrower egui then
/// clips the content to the drag width but still places the neighbouring panel
/// at the wider content edge — the "black strip" (see [`panel_header`]). We
/// build the header by hand from a [`egui::collapsing_header::CollapsingState`]
/// so the label can truncate, reusing egui's rotating-triangle icon and
/// click-anywhere-to-toggle behaviour. Reused by the request tree, the
/// environment list and (later) the workspace tree.
pub fn tree_header<R>(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    default_open: bool,
    label: RichText,
    add_body: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::Response {
    let id = ui.make_persistent_id(id_salt);
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        id,
        default_open,
    );
    let openness = state.openness(ui.ctx());

    let header = ui
        .horizontal(|ui| {
            // Reserve the full row width so short names still toggle across the
            // whole row, then draw the triangle + a truncating label.
            ui.set_min_width(ui.available_width());
            let icon_w = ui.spacing().icon_width;
            let (_rect, icon_resp) =
                ui.allocate_exact_size(egui::vec2(icon_w, icon_w), egui::Sense::hover());
            egui::collapsing_header::paint_default_icon(ui, openness, &icon_resp);
            ui.add(egui::Label::new(label).truncate().selectable(false));
        })
        .response
        .interact(egui::Sense::click());

    if header.clicked() {
        state.toggle(ui);
    }
    state.show_body_indented(&header, ui, add_body);
    header
}

/// A coloured HTTP-method badge, matching the terminal UI's method colours.
pub fn method_badge(ui: &mut egui::Ui, method: &str) {
    let col = method_color(method);
    ui.label(RichText::new(method).strong().monospace().color(col));
}

/// A method picker combo box. Returns true if the method changed.
pub fn method_combo(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    method: &mut String,
) -> bool {
    let mut changed = false;
    let col = method_color(method);
    egui::ComboBox::from_id_salt(id)
        .selected_text(RichText::new(method.clone()).strong().color(col))
        .width(96.0)
        .show_ui(ui, |ui| {
            for m in METHODS {
                if selectable(ui, method == m, RichText::new(m).color(method_color(m))).clicked() {
                    *method = m.to_string();
                    changed = true;
                }
            }
        });
    changed
}

/// An editable table of `(key, value, enabled)` rows (headers, query params,
/// cookies, options). Returns true if anything changed. Adds a trailing
/// "add row" button.
pub fn kv_editor(
    ui: &mut egui::Ui,
    theme: &GuiTheme,
    s: &Strings,
    id: impl std::hash::Hash + std::fmt::Debug,
    rows: &mut Vec<(String, String, bool)>,
    key_hint: &str,
    val_hint: &str,
) -> bool {
    let mut changed = false;
    let mut remove: Option<usize> = None;
    // The value must be the grid's *last* column for egui to stretch it to the
    // full available width — otherwise a trailing "remove" column would be the
    // one that fills and the value would stay content-sized (a narrow table).
    // So the remove ✕ is tucked into the value cell via a right-to-left layout:
    // ✕ pins to the right edge and the value fills everything to its left.
    //
    // The key must grow too (a fixed-width key next to a filling value reads as
    // "tiny key, huge value"): after reserving the fixed controls (checkbox, ✕,
    // column spacing) the free width is split ~40% key / ~60% value.
    let key_w = split_key_width(ui, 72.0);
    egui::Grid::new(id)
        .num_columns(3)
        .spacing([8.0, 4.0])
        .striped(true)
        .min_col_width(0.0)
        .show(ui, |ui| {
            for i in 0..rows.len() {
                if ui.checkbox(&mut rows[i].2, "").changed() {
                    changed = true;
                }
                let k = sized_key(ui, key_w, &mut rows[i].0, key_hint);
                if k.changed() {
                    changed = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(RichText::new(super::icons::CLOSE).color(theme.err))
                        .on_hover_text(s.gui_remove)
                        .clicked()
                    {
                        remove = Some(i);
                    }
                    let v = ui.add(
                        egui::TextEdit::singleline(&mut rows[i].1)
                            .hint_text(val_hint)
                            .desired_width(f32::INFINITY),
                    );
                    if v.changed() {
                        changed = true;
                    }
                });
                ui.end_row();
            }
        });
    if let Some(i) = remove {
        rows.remove(i);
        changed = true;
    }
    if ui.button(s.gui_add).clicked() {
        rows.push((String::new(), String::new(), true));
        changed = true;
    }
    changed
}

/// An editable table of `(name, value)` pairs without an enabled flag
/// (captures, reports). Returns true if anything changed.
pub fn pair_editor(
    ui: &mut egui::Ui,
    theme: &GuiTheme,
    s: &Strings,
    id: impl std::hash::Hash + std::fmt::Debug,
    rows: &mut Vec<(String, String)>,
    key_hint: &str,
    val_hint: &str,
) -> bool {
    let mut changed = false;
    let mut remove: Option<usize> = None;
    // See `kv_editor`: the value fills only as the last column, so the remove ✕
    // shares the value cell (right-aligned) rather than being its own column,
    // and the key takes ~40% of the free width so it grows too.
    let key_w = split_key_width(ui, 42.0);
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([8.0, 4.0])
        .striped(true)
        .min_col_width(0.0)
        .show(ui, |ui| {
            for i in 0..rows.len() {
                let k = sized_key(ui, key_w, &mut rows[i].0, key_hint);
                if k.changed() {
                    changed = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(RichText::new(super::icons::CLOSE).color(theme.err))
                        .clicked()
                    {
                        remove = Some(i);
                    }
                    let v = ui.add(
                        egui::TextEdit::singleline(&mut rows[i].1)
                            .hint_text(val_hint)
                            .desired_width(f32::INFINITY),
                    );
                    if v.changed() {
                        changed = true;
                    }
                });
                ui.end_row();
            }
        });
    if let Some(i) = remove {
        rows.remove(i);
        changed = true;
    }
    if ui.button(s.gui_add).clicked() {
        rows.push((String::new(), String::new()));
        changed = true;
    }
    changed
}

/// A horizontal row of pill "section" tabs; sets `*current` to the clicked one.
pub fn section_tabs<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    theme: &GuiTheme,
    current: &mut T,
    tabs: &[(T, &str)],
) {
    ui.horizontal_wrapped(|ui| {
        for (value, label) in tabs {
            let selected = *current == *value;
            let mut text = RichText::new(*label);
            text = if selected {
                text.strong().color(theme.text)
            } else {
                text.color(theme.dim)
            };
            if selectable(ui, selected, text).clicked() {
                *current = *value;
            }
        }
    });
}

/// A small count suffix like " (3)" for a section that has content.
pub fn count_suffix(n: usize) -> String {
    if n == 0 {
        String::new()
    } else {
        format!(" ({n})")
    }
}

/// Colour a status code by class (2xx ok, 4xx/5xx error, else pending).
pub fn status_color(theme: &GuiTheme, status: u16) -> Color32 {
    match status {
        200..=299 => theme.ok,
        400..=599 => theme.err,
        _ => theme.pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare `desired_width` key field collapses to a sliver inside a grid
    /// whose last column fills; `sized_key` must instead render at the full
    /// [`split_key_width`] width. Regression test for "the key field is tiny".
    fn measure_key(screen_w: f32) -> (f32, f32) {
        let ctx = egui::Context::default();
        let mut key_w = 0.0;
        let mut rendered = 0.0;
        let mut text = "Content-Type".to_string();
        let mut value = "application/json".to_string();
        for _ in 0..4 {
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(screen_w, 400.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    key_w = split_key_width(ui, 72.0);
                    egui::Grid::new("t")
                        .num_columns(3)
                        .min_col_width(0.0)
                        .show(ui, |ui| {
                            ui.checkbox(&mut true, "");
                            rendered = sized_key(ui, key_w, &mut text, "").rect.width();
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let _ = ui.button("x");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut value)
                                            .desired_width(f32::INFINITY),
                                    );
                                },
                            );
                            ui.end_row();
                        });
                },
            );
        }
        (key_w, rendered)
    }

    #[test]
    fn key_field_renders_at_the_computed_split_width() {
        let (key_w, rendered) = measure_key(600.0);
        // The key must fill (near enough) the computed width, not collapse to
        // the ~24px minimum a bare grid cell would give it.
        assert!(key_w > 150.0, "split width should be substantial: {key_w}");
        assert!(
            (rendered - key_w).abs() < 2.0,
            "key rendered {rendered}, expected ~{key_w}"
        );
    }
}
