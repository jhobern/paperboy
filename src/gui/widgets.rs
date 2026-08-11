//! Small shared egui widgets used across the GUI panels.

use eframe::egui::{self, Color32, RichText};

use crate::hurl::KvRow;
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
pub fn sized_key(
    ui: &mut egui::Ui,
    key_w: f32,
    text: &mut String,
    hint: &str,
    color: Color32,
) -> egui::Response {
    let h = ui.spacing().interact_size.y;
    ui.add_sized(
        [key_w, h],
        egui::TextEdit::singleline(text)
            .hint_text(hint)
            .text_color(color),
    )
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
/// so the label can truncate, with the same explicit Phosphor carets the
/// workspace tree uses and click-anywhere-to-toggle behaviour. Reused by the
/// request tree, the environment list and (later) the workspace tree.
pub fn tree_header<R>(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    default_open: bool,
    label: RichText,
    add_body: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::Response {
    tree_header_marked(ui, id_salt, default_open, false, label, None, add_body)
}

/// [`tree_header`] with an optional highlight colour painted as a full-width
/// band behind the row and a matching bar down its left edge.
///
/// Colouring the *text* alone (which is all this used to do for the active
/// environment) is easy to miss in a list of similar rows — the terminal UI
/// gets away with less because a terminal list is denser. A filled band reads
/// at a glance from anywhere in the panel.
pub fn tree_header_marked<R>(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    default_open: bool,
    force_open: bool,
    label: RichText,
    highlight: Option<egui::Color32>,
    add_body: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::Response {
    // The caller supplies a namespaced, model-derived salt; do not mix in the
    // current Ui id, because filtered trees can move the same model row between
    // containers and should still keep its own expansion state.
    let id = egui::Id::new(id_salt);
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        id,
        default_open,
    );
    // `force_open` is how something *outside* this row (opening an environment
    // from the workspace tree, say) says "reveal this one". It only ever opens:
    // a caller asking to reveal a row the user has already opened must not
    // toggle it shut.
    if force_open && !state.is_open() {
        state.set_open(true);
    }
    let open = state.is_open();

    // The band has to be painted *under* the row's contents, so reserve a slot
    // in the paint order now and fill it once the row's rect is known.
    let band = highlight.map(|_| ui.painter().add(egui::Shape::Noop));

    let header = ui
        .horizontal(|ui| {
            // Reserve the full row width so short names still toggle across the
            // whole row, then draw the triangle + a truncating label.
            ui.set_min_width(ui.available_width());
            let caret = if open {
                super::icons::CARET_DOWN
            } else {
                super::icons::CARET_RIGHT
            };
            // egui's built-in painted triangle is a tiny vector shape, so the
            // closed state could read as "no affordance" beside the Phosphor
            // glyphs used by the workspace tree. Text carets keep both trees
            // visually consistent and use the icon font PaperBoy installs.
            ui.add_sized(
                egui::vec2(ui.spacing().icon_width, ui.spacing().interact_size.y),
                egui::Label::new(caret).selectable(false),
            );
            ui.add(egui::Label::new(label).truncate().selectable(false));
        })
        .response
        .interact(egui::Sense::click());

    if let (Some(band), Some(color)) = (band, highlight) {
        let rect = header.rect.expand2(egui::vec2(0.0, 2.0));
        let mut shapes = vec![egui::Shape::rect_filled(
            rect,
            3.0,
            color.gamma_multiply(0.22),
        )];
        // A solid bar on the leading edge, so the row still reads as marked on
        // a theme whose background leaves the translucent band very faint.
        shapes.push(egui::Shape::rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            1.0,
            color,
        ));
        ui.painter().set(band, egui::Shape::Vec(shapes));
    }

    if header.clicked() {
        state.toggle(ui);
    }
    state.show_body_indented(&header, ui, add_body);
    header
}

/// A coloured HTTP-method badge, matching the terminal UI's method colours.
/// `theme` supplies the colour for any verb the shared table doesn't name.
pub fn method_badge(ui: &mut egui::Ui, theme: &GuiTheme, method: &str) {
    let col = method_color(method, theme.dim);
    ui.label(RichText::new(method).strong().monospace().color(col));
}

/// A method picker combo box. Returns true if the method changed.
pub fn method_combo(
    ui: &mut egui::Ui,
    theme: &GuiTheme,
    id: impl std::hash::Hash + std::fmt::Debug,
    method: &mut String,
) -> bool {
    let mut changed = false;
    let col = method_color(method, theme.dim);
    egui::ComboBox::from_id_salt(id)
        .selected_text(RichText::new(method.clone()).strong().color(col))
        .width(96.0)
        .show_ui(ui, |ui| {
            for m in METHODS {
                if selectable(
                    ui,
                    method == m,
                    RichText::new(m).color(method_color(m, theme.dim)),
                )
                .clicked()
                {
                    *method = m.to_string();
                    changed = true;
                }
            }
        });
    changed
}

/// One column title in a key/value table — dim and bold, so it reads as a
/// label for the column rather than as another editable row.
fn column_header(ui: &mut egui::Ui, theme: &GuiTheme, text: &str) {
    ui.label(RichText::new(text).strong().color(theme.dim));
}

/// A column title that *allocates* `w`, exactly as the cell below it does.
///
/// A bare label only claims the width of its own text, so on an empty table the
/// grid's columns shrank to fit the words "Header Value Description" and the
/// titles bunched together — then sprang apart the moment a row was added and
/// the real fields set the column widths. Sizing the titles from the same
/// numbers as the fields keeps every label still.
fn sized_header(ui: &mut egui::Ui, theme: &GuiTheme, text: &str, w: f32) {
    let h = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(
        egui::vec2(w, h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(w);
            column_header(ui, theme, text);
        },
    );
}

/// The four column widths of a [`kv_editor`] row: tick, key, value, note.
///
/// The description used to be whatever the key and value left over — and they
/// claimed 40% + 60% of the row between them, so it collapsed to a sliver you
/// couldn't read a word in. All three text columns are now shares of the same
/// free width, so a note has room without starving the key or the value.
///
/// Every width is returned (rather than letting the last column fill) because
/// the header row has to allocate exactly what the rows below it do; see
/// [`sized_header`].
fn kv_widths(ui: &egui::Ui) -> (f32, f32, f32, f32) {
    let check = ui.spacing().interact_size.y + 4.0;
    // The tick, the remove ✕ and the three gaps between the four columns are
    // fixed furniture; only what's left is shared out.
    let fixed = check + ui.spacing().interact_size.y + 8.0 + 3.0 * 8.0;
    let free = (ui.available_width() - fixed).max(240.0);
    let key = free * 0.28;
    let val = free * 0.38;
    (check, key, val, free - key - val)
}

/// An editable table of [`KvRow`]s (headers, query params, cookies, options).
/// Returns true if anything changed. Adds a trailing "add row" button.
pub fn kv_editor(
    ui: &mut egui::Ui,
    theme: &GuiTheme,
    s: &Strings,
    id: impl std::hash::Hash + std::fmt::Debug,
    rows: &mut Vec<KvRow>,
    key_hint: &str,
    val_hint: &str,
    key_label: &str,
    val_label: &str,
) -> bool {
    let mut changed = false;
    let mut remove: Option<usize> = None;
    // The remove ✕ is tucked into the description cell via a right-to-left
    // layout — ✕ pins to the right edge and the description fills everything to
    // its left — rather than being a fifth column, so the note stays adjacent
    // to the value it annotates.
    let (check_w, key_w, val_w, desc_w) = kv_widths(ui);
    let row_h = ui.spacing().interact_size.y;
    egui::Grid::new(id)
        .num_columns(4)
        .spacing([8.0, 4.0])
        .striped(true)
        .min_col_width(0.0)
        .show(ui, |ui| {
            // Column titles, as in the terminal UI: without them a bare grid of
            // text boxes gives no clue that the tick is "send this row" rather
            // than "select".
            sized_header(ui, theme, "\u{2713}", check_w);
            sized_header(ui, theme, key_label, key_w);
            sized_header(ui, theme, val_label, val_w);
            sized_header(ui, theme, s.hdr_description, desc_w);
            ui.end_row();
            for i in 0..rows.len() {
                if ui
                    .add_sized(
                        [check_w, row_h],
                        egui::Checkbox::without_text(&mut rows[i].enabled),
                    )
                    .changed()
                {
                    changed = true;
                }
                // A disabled row (checkbox unticked) isn't sent, so grey its
                // key/value out to read as inactive — matching the terminal UI.
                let row_color = if rows[i].enabled {
                    theme.text
                } else {
                    theme.dim
                };
                let k = sized_key(ui, key_w, &mut rows[i].key, key_hint, row_color);
                if k.changed() {
                    changed = true;
                }
                let v = sized_key(ui, val_w, &mut rows[i].value, val_hint, row_color);
                if v.changed() {
                    changed = true;
                }
                ui.allocate_ui_with_layout(
                    egui::vec2(desc_w, row_h),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.set_min_width(desc_w);
                        if ui
                            .button(RichText::new(super::icons::CLOSE).color(theme.err))
                            .on_hover_text(s.gui_remove)
                            .clicked()
                        {
                            remove = Some(i);
                        }
                        // The description is a note for whoever reads the file
                        // later, not part of the request, so it is always dim —
                        // even on an enabled row it shouldn't compete with the
                        // value beside it.
                        let d = ui.add(
                            egui::TextEdit::singleline(&mut rows[i].desc)
                                .hint_text(s.gui_hint_description)
                                .text_color(theme.dim)
                                .desired_width(f32::INFINITY),
                        );
                        if d.changed() {
                            changed = true;
                        }
                    },
                );
                ui.end_row();
            }
        });
    if let Some(i) = remove {
        rows.remove(i);
        changed = true;
    }
    if ui.button(s.gui_add).clicked() {
        rows.push(KvRow::toggled(String::new(), String::new(), true));
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
    key_label: &str,
    val_label: &str,
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
            // See `kv_editor`: titled columns, minus the enabled tick this
            // table doesn't have.
            column_header(ui, theme, key_label);
            column_header(ui, theme, val_label);
            ui.end_row();
            for i in 0..rows.len() {
                let k = sized_key(ui, key_w, &mut rows[i].0, key_hint, theme.text);
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

    /// Paint one `tree_header_marked` row and return the fills of every solid
    /// rectangle it drew, so a test can tell a marked row from a plain one.
    fn header_fills(highlight: Option<egui::Color32>) -> Vec<egui::Color32> {
        let ctx = egui::Context::default();
        let mut fills = Vec::new();
        for _ in 0..3 {
            let out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(300.0, 200.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    tree_header_marked(
                        ui,
                        "env-row",
                        false,
                        false,
                        RichText::new("dev"),
                        highlight,
                        |_ui| {},
                    );
                },
            );
            fills.clear();
            // The band is pushed as a single `Shape::Vec`, so walk into groups
            // rather than only looking at top-level shapes.
            fn collect(shape: &egui::Shape, out: &mut Vec<egui::Color32>) {
                match shape {
                    egui::Shape::Rect(r) if r.fill != egui::Color32::TRANSPARENT => {
                        out.push(r.fill)
                    }
                    egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, out)),
                    _ => {}
                }
            }
            out.shapes
                .iter()
                .for_each(|s| collect(&s.shape, &mut fills));
        }
        fills
    }

    /// The active Global Environment has to be obvious at a glance, not a tinted
    /// word among identically-shaped rows: a marked header paints a band plus a
    /// solid leading bar in the highlight colour, and an unmarked one paints
    /// neither.
    #[test]
    fn a_marked_tree_header_paints_a_band_in_the_highlight_colour() {
        let mark = egui::Color32::from_rgb(0x3d, 0xd6, 0x8c);

        let marked = header_fills(Some(mark));
        assert!(
            marked.contains(&mark),
            "the solid leading bar uses the highlight colour: {marked:?}"
        );
        assert!(
            marked
                .iter()
                .any(|c| *c != mark && c.r() > 0 && c.g() > c.r() && c.g() > c.b()),
            "a translucent band of the same hue sits behind the row: {marked:?}"
        );

        let plain = header_fills(None);
        assert!(
            !plain.contains(&mark),
            "an unmarked row paints no highlight: {plain:?}"
        );
        assert!(
            plain.len() < marked.len(),
            "the marking is the only difference between the two rows"
        );
    }

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
                            rendered = sized_key(ui, key_w, &mut text, "", Color32::PLACEHOLDER)
                                .rect
                                .width();
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

    /// Render a `kv_editor` with `n` rows in a fixed-width window and report
    /// the width the whole table claimed.
    fn kv_table_width(n: usize) -> f32 {
        let ctx = egui::Context::default();
        let theme = GuiTheme::from_spec(&crate::theme::default_preset());
        let s = Strings::for_language(&crate::i18n::Language::English);
        let mut rows: Vec<KvRow> = (0..n)
            .map(|i| KvRow::new(&format!("Header-{i}"), "a value"))
            .collect();
        let mut w = 0.0;
        // Grid column widths settle from the previous pass, so run a few.
        for _ in 0..4 {
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(900.0, 400.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    let before = ui.min_rect().width();
                    kv_editor(
                        ui, &theme, &s, "kv", &mut rows, "name", "value", "Header", "Value",
                    );
                    w = ui.min_rect().width() - before;
                },
            );
        }
        w
    }

    /// The column titles used to be bare labels, so an empty table sized its
    /// columns to the words "Header Value Description" and everything jumped
    /// when the first row appeared. The table must claim the same width either
    /// way.
    #[test]
    fn empty_and_filled_tables_lay_out_their_columns_identically() {
        let empty = kv_table_width(0);
        let filled = kv_table_width(2);
        assert!(
            (empty - filled).abs() < 1.0,
            "empty table was {empty} wide, filled was {filled}"
        );
    }

    /// The description column used to be whatever the key (40%) and value (60%)
    /// left over — i.e. nothing. It must get a readable share of its own.
    #[test]
    fn the_description_column_gets_a_readable_share() {
        let ctx = egui::Context::default();
        let mut got = (0.0, 0.0, 0.0, 0.0);
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(900.0, 400.0),
                )),
                ..Default::default()
            },
            |ui| got = kv_widths(ui),
        );
        let (check, key, val, desc) = got;
        assert!(desc > 150.0, "description was only {desc} wide");
        assert!(key > 150.0 && val > key, "key {key}, value {val}");
        // Nothing may overflow the row: the four columns plus the fixed
        // furniture have to fit what the table was given.
        let total = check + key + val + desc + 3.0 * 8.0 + 24.0;
        assert!(total <= 900.0, "columns sum to {total}, wider than the row");
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

    /// Whether the row's body was drawn — the observable half of "reveal this
    /// environment": a caller outside the panel asks for it, and the collapsing
    /// row opens without the user having clicked it.
    fn body_drawn(force_open: bool, id: &'static str) -> bool {
        let ctx = egui::Context::default();
        let mut drawn = false;
        for _ in 0..3 {
            drawn = false;
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(300.0, 200.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    tree_header_marked(
                        ui,
                        id,
                        false,
                        force_open,
                        RichText::new("dev"),
                        None,
                        |_ui| {
                            drawn = true;
                        },
                    );
                },
            );
        }
        drawn
    }

    #[test]
    fn a_collapsed_row_can_be_opened_by_its_caller_rather_than_by_a_click() {
        assert!(
            !body_drawn(false, "env-closed"),
            "a default-closed row starts closed"
        );
        assert!(
            body_drawn(true, "env-revealed"),
            "asking to reveal it should open it with no click involved"
        );
    }
}

/// What a [`dialog`] frame produced, and whether the user asked to close it.
///
/// `inner` is `None` when egui declined to draw the window at all — which is
/// not an answer, so callers keep the dialog armed rather than deciding for
/// the user.
pub(crate) struct DialogFrame<R> {
    pub inner: Option<R>,
    /// The user pressed Escape or clicked the window's ✕ this frame. Every
    /// dialog treats this as its cancel, so there is always a way out that
    /// doesn't involve finding the right button.
    pub dismissed: bool,
}

impl<R> DialogFrame<R> {
    /// The frame's answer, or `default` when egui drew nothing.
    pub fn inner_or(self, default: R) -> R {
        self.inner.unwrap_or(default)
    }
}

/// The centred modal window shell shared by every GUI dialog.
///
/// Carries the two ways out a windowed dialog is expected to have: a ✕ in the
/// title bar (egui draws it for an `open`ed window) and the Escape key. Both
/// come back as [`DialogFrame::dismissed`] so the caller can run the same path
/// as its own Cancel button — a dialog that can only be answered by finding
/// the right button is a dialog people get stuck in.
pub(crate) fn dialog<R>(
    ctx: &egui::Context,
    title: &str,
    min_width: Option<f32>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> DialogFrame<R> {
    let mut open = true;
    let inner = egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            if let Some(w) = min_width {
                ui.set_min_width(w);
            }
            add(ui)
        })
        .and_then(|r| r.inner);
    // Escape is read from the raw input rather than consumed: a dialog is the
    // top-most thing on screen, so nothing underneath it should be acting on
    // the same press anyway.
    let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    DialogFrame {
        inner,
        dismissed: !open || esc,
    }
}
