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

/// Draw `content`'s text fields flat: no outline until the pointer or the
/// keyboard arrives.
///
/// egui frames a `TextEdit` with `widgets.inactive.bg_stroke` when it is idle,
/// `widgets.hovered.bg_stroke` under the pointer and `selection.stroke` while
/// focused. That inactive hairline is shared with buttons, combo boxes and
/// checkboxes — which *should* keep their outline, since an outline is how a
/// control says it is a control — so it is dropped here, scoped to the fields,
/// rather than globally.
///
/// The affordance is not lost, only deferred: the field keeps its wash (see
/// [`GuiTheme::field`]), grows a border under the pointer, and is outlined in
/// the selection colour while it has focus. This is the report editor's chip
/// treatment applied to editable text — read as content, behave as a control.
///
/// Scoped through [`egui::Ui::scope`] because `visuals_mut` edits the `Ui`'s
/// own style: without it the change would leak into every later widget in the
/// same `Ui`, taking the checkbox column of a key/value grid with it.
pub fn flat_fields<R>(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.scope(|ui| {
        ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::NONE;
        content(ui)
    })
    .inner
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
    flat_fields(ui, |ui| {
        ui.add_sized(
            [key_w, h],
            egui::TextEdit::singleline(text)
                .hint_text(hint)
                .text_color(color),
        )
    })
}

/// A value field that shows **all** of its text, wrapping onto as many lines
/// as it needs instead of scrolling the overflow out of sight.
///
/// A single-line field is a viewport onto its value: a bearer token, a long
/// URL or a JSON fragment is a few visible characters and a promise that the
/// rest is in there somewhere, which has to be scrubbed through to read. Since
/// these fields hold the *content* of a request — the thing the screen exists
/// to show — they wrap and the row grows, exactly as the terminal UI does
/// (which edits the Hurl source directly and has always wrapped).
///
/// It is a multiline `TextEdit` for the wrapping, but not a multiline *field*:
/// `return_key(None)` means Enter never inserts a newline, so a header value
/// cannot be broken across lines by a stray keystroke into something that
/// would not survive being written out as Hurl.
pub fn wrapping_field(
    ui: &mut egui::Ui,
    width: f32,
    text: &mut String,
    hint: &str,
    color: Color32,
) -> egui::Response {
    wrapping_field_font(ui, width, text, hint, color, egui::TextStyle::Body)
}

/// [`wrapping_field`] in a chosen text style — the URL is monospaced, so that
/// the punctuation a URL is mostly made of lines up.
pub fn wrapping_field_font(
    ui: &mut egui::Ui,
    width: f32,
    text: &mut String,
    hint: &str,
    color: Color32,
    font: egui::TextStyle,
) -> egui::Response {
    // A `TextEdit`'s `desired_width` is the width of the *text*: its margin is
    // added on top. Asking for the caller's width therefore claimed a few
    // pixels more than the column reserved, so a filled table laid its columns
    // out slightly wider than an empty one and the headers stopped lining up
    // with the fields beneath them.
    const TEXT_EDIT_MARGIN: f32 = 8.0;
    let text_w = (width - TEXT_EDIT_MARGIN).max(16.0);
    flat_fields(ui, |ui| {
        // Allocated at the width the caller worked out, with the height left
        // to the content — `add_sized` would pin the height and undo the
        // growth this exists for.
        ui.allocate_ui(egui::vec2(width, ui.spacing().interact_size.y), |ui| {
            ui.set_width(width);
            ui.add(
                egui::TextEdit::multiline(text)
                    .hint_text(hint)
                    .text_color(color)
                    .desired_width(text_w)
                    .desired_rows(1)
                    .return_key(None)
                    .font(font),
            )
        })
        .inner
    })
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

/// The width the remove ✕ column claims.
///
/// Its *natural* size — the glyph plus the button's own padding — is wider
/// than the `interact_size` a row is otherwise built from, so reserving the
/// row height for it left the filled table a few pixels wider than the header
/// that has to line up with it. Sizing the button to this reserves exactly
/// what it takes.
fn remove_width(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y + 2.0 * ui.spacing().button_padding.x
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
    // The tick, the remove ✕ and the four gaps between the five columns are
    // fixed furniture; only what's left is shared out.
    let fixed = check + remove_width(ui) + 4.0 * 8.0;
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
    // The remove ✕ is a column of its own rather than being tucked inside the
    // description cell. Sharing a cell meant the note was positioned relative
    // to a *button*, and a button is taller than a field, so the note settled
    // a pixel or two below the key and value it belongs to — every row sagged
    // towards its right-hand edge. As its own cell each field starts on the
    // same line, and the ✕ still sits immediately right of the note.
    let (check_w, key_w, val_w, desc_w) = kv_widths(ui);
    let row_h = ui.spacing().interact_size.y;
    egui::Grid::new(id)
        .num_columns(5)
        .spacing([8.0, 4.0])
        // Unstriped: every cell in the row is a filled field now, so the row
        // already reads as a row. Alternating the *background* behind fields
        // that have a background of their own gave each row two competing
        // shades and made the fields look like they were floating on top of
        // the table rather than being it.
        .min_col_width(0.0)
        .show(ui, |ui| {
            // Column titles, as in the terminal UI: without them a bare grid of
            // text boxes gives no clue that the tick is "send this row" rather
            // than "select".
            // The Phosphor tick, not a bare `\u{2713}`: egui's bundled fonts
            // have no glyph for it, so the literal rendered as a tofu box.
            sized_header(ui, theme, super::icons::PASS, check_w);
            sized_header(ui, theme, key_label, key_w);
            sized_header(ui, theme, val_label, val_w);
            sized_header(ui, theme, s.hdr_description, desc_w);
            // The ✕ column has no title — the button says what it does.
            ui.allocate_space(egui::vec2(remove_width(ui), 1.0));
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
                // The value is the one cell whose content is the request
                // itself, so it is the one that wraps rather than truncates.
                let v = wrapping_field(ui, val_w, &mut rows[i].value, val_hint, row_color);
                if v.changed() {
                    changed = true;
                }
                // The description is a note for whoever reads the file later,
                // not part of the request, so it is always dim — even on an
                // enabled row it shouldn't compete with the value beside it.
                let d = wrapping_field(
                    ui,
                    desc_w,
                    &mut rows[i].desc,
                    s.gui_hint_description,
                    theme.dim,
                );
                if d.changed() {
                    changed = true;
                }
                // Sized, not free-growing: the header row has to reserve
                // exactly what this column claims (see `kv_widths`), or an
                // empty table and a filled one lay their columns out
                // differently and everything shifts when the first row lands.
                let x_w = remove_width(ui);
                if ui
                    .add_sized(
                        [x_w, row_h],
                        egui::Button::new(RichText::new(super::icons::CLOSE).color(theme.err)),
                    )
                    .on_hover_text(s.gui_remove)
                    .clicked()
                {
                    remove = Some(i);
                }
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
        // See `kv_editor`: the filled fields are the row.
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
                // `Align::Min` so the value lines up with the key beside it
                // rather than being centred against the taller ✕ — see
                // `kv_editor`.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if ui
                        .button(RichText::new(super::icons::CLOSE).color(theme.err))
                        .clicked()
                    {
                        remove = Some(i);
                    }
                    let v = wrapping_field(
                        ui,
                        ui.available_width(),
                        &mut rows[i].1,
                        val_hint,
                        theme.text,
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

    fn screen() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(900.0, 600.0))
    }

    fn a_frame() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(screen()),
            ..Default::default()
        }
    }

    fn click_at(input: &mut egui::RawInput, pos: egui::Pos2) {
        input.events.push(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
    }

    /// The flattening is scoped. A checkbox or a button in the same row still
    /// gets its outline — an outline is how a *control* says it is a control;
    /// the fields give theirs up because they are mostly content.
    #[test]
    fn flattening_a_field_does_not_flatten_the_controls_beside_it() {
        let ctx = egui::Context::default();
        // The app's own theme, not egui's defaults: the outline this is about
        // is one PaperBoy puts there.
        GuiTheme::from_spec(&crate::theme::default_preset()).apply(&ctx);
        let mut inside = egui::Stroke::new(9.0, Color32::RED);
        let mut after = egui::Stroke::new(9.0, Color32::RED);
        ctx.run_ui(a_frame(), |ui| {
            let before = ui.visuals().widgets.inactive.bg_stroke;
            assert!(before.width > 0.0, "the app's controls are outlined");
            flat_fields(ui, |ui| {
                inside = ui.visuals().widgets.inactive.bg_stroke;
            });
            after = ui.visuals().widgets.inactive.bg_stroke;
        });
        assert_eq!(inside, egui::Stroke::NONE, "no box around an idle field");
        assert!(
            after.width > 0.0,
            "and everything after it is left alone, got {after:?}"
        );
    }

    /// A field whose value doesn't fit used to hide the rest behind a
    /// scrolling viewport. It now wraps, so the row grows and the whole value
    /// is on screen — the point of the panel.
    #[test]
    fn a_long_value_wraps_instead_of_scrolling_out_of_sight() {
        let ctx = egui::Context::default();
        let measure = |text: &str| -> f32 {
            let mut value = text.to_string();
            let mut height = 0.0;
            // Twice: egui settles galley sizes on the second pass.
            for _ in 0..2 {
                ctx.run_ui(a_frame(), |ui| {
                    height = wrapping_field(ui, 200.0, &mut value, "", Color32::WHITE)
                        .rect
                        .height();
                });
            }
            height
        };

        let short = measure("small");
        let long = measure(
            "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.a-very-long-token-that-will-not-fit              in-two-hundred-points-of-width-no-matter-how-small-the-font-is",
        );
        assert!(
            long > short * 2.0,
            "a value too long for the field must wrap onto more lines: {short} then {long}"
        );
    }

    /// The wrapping is for reading, not for multi-line values: a header broken
    /// across lines would not survive being written out as Hurl.
    #[test]
    fn enter_cannot_break_a_value_across_lines() {
        let ctx = egui::Context::default();
        let mut value = "text/plain".to_string();
        let mut input = a_frame();
        // Click into the field, then press Enter.
        click_at(&mut input, egui::pos2(60.0, 12.0));
        input.events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        for _ in 0..2 {
            ctx.run_ui(input.clone(), |ui| {
                wrapping_field(ui, 200.0, &mut value, "", Color32::WHITE);
            });
        }
        assert_eq!(value, "text/plain", "Enter must not insert a newline");
    }

    /// A dialog covers the app with a sheet that eats clicks: the menu bar and
    /// the panels underneath must not act while a dialog is waiting for an
    /// answer, or a second wizard ends up opened on top of the first. It is an
    /// *input* sink only — the frame loop keeps running, so background work
    /// (a git fetch, a report run) still finishes while the dialog is up.
    #[test]
    fn a_dialog_stops_clicks_reaching_the_app_behind_it() {
        // The button sits in the far corner, well away from the centred dialog.
        let button_pos = egui::pos2(20.0, 20.0);

        let clicked_behind = |with_dialog: bool| {
            let ctx = egui::Context::default();
            let mut clicked = false;
            // Two passes: egui needs the first to lay the widgets out before a
            // click can land on them.
            for pass in 0..2 {
                let mut input = a_frame();
                if pass == 1 {
                    click_at(&mut input, button_pos);
                }
                let _ = ctx.run_ui(input, |ui| {
                    if ui.button("behind").clicked() {
                        clicked = true;
                    }
                    if with_dialog {
                        let ctx = ui.ctx().clone();
                        dialog(&ctx, "In the way", None, |ui| {
                            ui.label("answer me");
                        });
                    }
                });
            }
            clicked
        };

        assert!(
            clicked_behind(false),
            "the test's own button is clickable with no dialog up"
        );
        assert!(
            !clicked_behind(true),
            "the same click must not reach it through an open dialog"
        );
    }

    /// The dialog opens centred but is not pinned there: one anchored to the
    /// middle cannot be dragged off whatever the user opened it to look at.
    #[test]
    fn a_dialog_opens_centred_and_can_still_be_dragged_aside() {
        let ctx = egui::Context::default();
        let draw = |input: egui::RawInput| {
            let _ = ctx.run_ui(input, |ui| {
                let ctx = ui.ctx().clone();
                dialog(&ctx, "Draggable", None, |ui| {
                    ui.label("body");
                });
            });
            // egui derives a window's Area id from its title atoms.
            egui::AreaState::load(&ctx, egui::Id::new(Some("Draggable")))
                .expect("the dialog was drawn")
                .rect()
        };

        draw(a_frame());
        let centred = draw(a_frame());
        assert!(
            (centred.center().x - screen().center().x).abs() < 2.0
                && (centred.center().y - screen().center().y).abs() < 2.0,
            "it opens in the middle: {centred:?}"
        );

        // Drag the title bar to the left, as a user would.
        let grab = egui::pos2(centred.center().x, centred.min.y + 6.0);
        let mut press = a_frame();
        press.events.push(egui::Event::PointerMoved(grab));
        press.events.push(egui::Event::PointerButton {
            pos: grab,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        draw(press);

        let mut drag = a_frame();
        drag.events
            .push(egui::Event::PointerMoved(grab - egui::vec2(200.0, 0.0)));
        let moved = draw(drag);

        assert!(
            moved.center().x < centred.center().x - 100.0,
            "dragging the title bar moves it: {moved:?} vs {centred:?}"
        );
    }

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

    /// Collect every rectangle painted in a given fill, recursing into the
    /// nested shape lists a `Ui` produces.
    fn rects_filled(shape: &egui::Shape, fill: Color32, out: &mut Vec<egui::Rect>) {
        match shape {
            egui::Shape::Rect(r) if r.fill == fill => out.push(r.rect),
            egui::Shape::Vec(v) => {
                for s in v {
                    rects_filled(s, fill, out);
                }
            }
            _ => {}
        }
    }

    /// The field backgrounds a closure paints, in paint order.
    fn field_rects(theme: &GuiTheme, mut body: impl FnMut(&mut egui::Ui)) -> Vec<egui::Rect> {
        let ctx = egui::Context::default();
        theme.apply(&ctx);
        let mut out = Vec::new();
        // Grid column widths and galley sizes settle from the previous pass.
        for _ in 0..4 {
            let full = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(1200.0, 600.0),
                    )),
                    ..Default::default()
                },
                &mut body,
            );
            out.clear();
            for cs in &full.shapes {
                rects_filled(&cs.shape, theme.field(), &mut out);
            }
        }
        out
    }

    /// A row's fields must sit on one line. The description shares its cell
    /// with the remove ✕, and a button is taller than a field — centring the
    /// cell's contents against it dropped the note ~2px below the key and
    /// value beside it, so every row visibly sagged to the right.
    #[test]
    fn the_fields_in_a_row_line_up_with_each_other() {
        let theme = GuiTheme::from_spec(&crate::theme::default_preset());
        let s = Strings::for_language(&crate::i18n::Language::English);
        let mut rows = vec![
            KvRow::new("Authorization", "Bearer abc"),
            KvRow::new("Accept", "application/json"),
        ];
        let rects = field_rects(&theme, |ui| {
            kv_editor(
                ui, &theme, &s, "kv", &mut rows, "name", "value", "Header", "Value",
            );
        });
        assert_eq!(rects.len(), 6, "three fields per row, got {rects:?}");
        for row in rects.chunks(3) {
            let top = row[0].top();
            for (i, r) in row.iter().enumerate() {
                assert!(
                    (r.top() - top).abs() < 0.5,
                    "field {i} sits at {} but the row starts at {top}",
                    r.top()
                );
            }
        }
    }

    /// Striping a table whose every cell is a filled field gives each row two
    /// competing backgrounds. The fields are the row; nothing is painted
    /// behind them.
    #[test]
    fn kv_rows_are_not_striped() {
        let theme = GuiTheme::from_spec(&crate::theme::default_preset());
        let s = Strings::for_language(&crate::i18n::Language::English);
        let mut rows = vec![KvRow::new("Accept", "application/json"); 4];
        let ctx = egui::Context::default();
        theme.apply(&ctx);
        let mut stripe = Color32::TRANSPARENT;
        let mut found = Vec::new();
        let full = ctx.run_ui(a_frame(), |ui| {
            stripe = ui.visuals().faint_bg_color;
            kv_editor(
                ui, &theme, &s, "kv", &mut rows, "name", "value", "Header", "Value",
            );
        });
        for cs in &full.shapes {
            rects_filled(&cs.shape, stripe, &mut found);
        }
        // Buttons and checkboxes share `faint_bg_color`; only a band as wide
        // as the table is a stripe.
        found.retain(|r| r.width() > 300.0);
        assert!(found.is_empty(), "row stripes painted: {found:?}");
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

/// The modal window shell shared by every GUI dialog.
///
/// Behaves the way a desktop dialog is expected to: it opens centred but can
/// be dragged aside (an anchored dialog cannot be moved off whatever you
/// opened it to look at), it carries the two ways out a windowed dialog has —
/// a ✕ in the title bar and the Escape key, both reported as
/// [`DialogFrame::dismissed`] so the caller can run its own Cancel — and it
/// puts a dimmed, click-swallowing sheet over the app behind it.
///
/// That sheet blocks *input*, not the frame loop: the app keeps painting and
/// keeps polling its background work, so a git fetch or a report run started
/// before the dialog opened still finishes while it is up.
pub(crate) fn dialog<R>(
    ctx: &egui::Context,
    title: &str,
    min_width: Option<f32>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> DialogFrame<R> {
    dialog_with(ctx, title, min_width, None, true, add)
}

/// A [`dialog`] without the sheet behind it, for the one case where the dialog
/// is *reporting* rather than *asking*: a long import that has everything it
/// needs and now only has to be waited for. The rest of the app stays usable
/// while it runs, because there is no question standing in the way of it.
pub(crate) fn dialog_modeless<R>(
    ctx: &egui::Context,
    title: &str,
    min_width: Option<f32>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> DialogFrame<R> {
    dialog_with(ctx, title, min_width, None, false, add)
}

/// A [`dialog`] the user can resize, for the ones whose body is a list: how
/// much of a repo's files or a workspace's collections fits on screen is the
/// user's call, not a number picked here.
pub(crate) fn dialog_resizable<R>(
    ctx: &egui::Context,
    title: &str,
    default_size: [f32; 2],
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> DialogFrame<R> {
    dialog_with(ctx, title, None, Some(default_size), true, add)
}

fn dialog_with<R>(
    ctx: &egui::Context,
    title: &str,
    min_width: Option<f32>,
    default_size: Option<[f32; 2]>,
    modal: bool,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> DialogFrame<R> {
    if modal {
        shade(ctx, title);
    }
    let mut open = true;
    let mut window = egui::Window::new(title)
        .collapsible(false)
        .resizable(default_size.is_some())
        // Centred on first sight, then wherever the user drags it.
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.input(|i| i.content_rect()).center())
        // Above the sheet, which is itself above the panels.
        .order(egui::Order::Foreground)
        .open(&mut open);
    if let Some(size) = default_size {
        window = window.default_size(size);
    }
    let inner = window
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
    // Escape closes the ones that are asking something. A modeless dialog is
    // sitting beside work the user is still doing, and Escape there belongs to
    // whatever they are actually typing into.
    let esc = modal && ctx.input(|i| i.key_pressed(egui::Key::Escape));
    DialogFrame {
        inner,
        dismissed: !open || esc,
    }
}

/// The dimmed sheet between a dialog and the app: it darkens what is behind
/// and swallows every click, drag and scroll aimed at it, so the menu bar and
/// the panels cannot be driven while a dialog is waiting for an answer (which
/// is how a second wizard used to end up opened on top of the first).
///
/// Deliberately *only* an input sink — nothing here stops the frame loop, so
/// background work carries on and the app never looks hung.
fn shade(ctx: &egui::Context, title: &str) {
    let screen = ctx.input(|i| i.content_rect());
    egui::Area::new(egui::Id::new(("paperboy-dialog-shade", title)))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .interactable(true)
        .show(ctx, |ui| {
            ui.painter()
                .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(96));
            ui.allocate_response(screen.size(), egui::Sense::click_and_drag());
        });
}
