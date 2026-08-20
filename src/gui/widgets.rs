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

/// One row of a key/value table: its cells side by side, **top**-aligned.
///
/// Not an [`egui::Grid`], which is what this used to be. A grid aligns a cell's
/// contents to the vertical *centre* of its row — and a row's height is only
/// known as its cells are added, so each cell was centred against the tallest
/// of the cells before it and every one sat a fraction lower than the last.
/// A fifth of a pixel is nothing in the model and a whole pixel on screen once
/// a row lands on a fractional y, so a table of them visibly sagged to the
/// right. Every column's width is worked out up front here anyway (see
/// [`kv_widths`]), which was most of what the grid was for.
pub fn table_row<R>(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        content(ui)
    })
    .inner
}

/// The rows of a key/value table, spaced as the grid used to space them.
pub fn table_rows<R>(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        content(ui)
    })
    .inner
}

/// Draw a control at the height of the fields around it, by taking its own
/// vertical padding away. A button is otherwise taller than the row it sits in.
pub fn flat_buttons<R>(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.scope(|ui| {
        ui.spacing_mut().button_padding.y = 0.0;
        content(ui)
    })
    .inner
}

/// A key/value row's **key** text field, forced to exactly `key_w` wide.
///
/// The same field as [`wrapping_field`], at a width of its own. It used to be
/// a `TextEdit::singleline` sized with `add_sized` — which laid out a *tenth of
/// a pixel* differently from the wrapping value beside it. That is invisible in
/// the model and a whole pixel on screen whenever the row lands on a fractional
/// y, so the key sat one pixel above the value and the description, and a table
/// of them read as sagging to the right. Two cells that have to line up are
/// better built the same way than adjusted until they agree.
///
/// (The reason neither uses a bare `TextEdit` is width: it clamps
/// `desired_width` to the cell's `available_width`, which stays tiny for a
/// non-last grid column, so the field would render as a ~24px sliver instead of
/// honouring the [`split_key_width`] split.)
pub fn sized_key(
    ui: &mut egui::Ui,
    key_w: f32,
    text: &mut String,
    hint: &str,
    color: Color32,
) -> egui::Response {
    wrapping_field(ui, key_w, text, hint, color)
}

/// The width the [`suggesting_key`] caret button takes out of the key column.
fn suggest_width(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y
}

/// A key field with a caret beside it offering the well-known names for the
/// section — HTTP header names, for instance.
///
/// Deliberately a *visible* button rather than a popup that appears as you
/// type. The terminal UI's Key column offers the same list, and the complaint
/// was that the GUI simply didn't have it; an autocomplete that only shows
/// itself once you have already started typing the name would leave someone who
/// doesn't know the name is on offer exactly where they were.
///
/// It is also not a `ComboBox`, because these lists are shortcuts rather than
/// vocabularies: any header name is legal, and a picker that made the two dozen
/// common ones easy at the cost of making the rest impossible would be a poor
/// trade. The field stays free text; the caret is a way to fill it in.
///
/// The list narrows to what has been typed so far, so the caret stays useful
/// after a few characters instead of making the user scroll past everything.
pub fn suggesting_key(
    ui: &mut egui::Ui,
    key_w: f32,
    text: &mut String,
    hint: &str,
    color: Color32,
    options: &[&'static str],
    empty_label: &str,
) -> egui::Response {
    let caret_w = suggest_width(ui);
    let mut resp = wrapping_field(ui, (key_w - caret_w - 4.0).max(40.0), text, hint, color);
    let row_h = ui.spacing().interact_size.y;
    let mut picked: Option<&'static str> = None;
    flat_buttons(ui, |ui| {
        let button = ui.add_sized(
            [caret_w, row_h],
            egui::Button::new(RichText::new(super::icons::CARET_DOWN).small()),
        );
        egui::Popup::menu(&button).show(|ui| {
            // Narrowed to what has been typed, the same way the terminal UI's
            // Key column narrows its list.
            let typed = text.trim().to_ascii_lowercase();
            let mut any = false;
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .show(ui, |ui| {
                    for opt in options {
                        if !typed.is_empty() && !opt.to_ascii_lowercase().contains(&typed) {
                            continue;
                        }
                        any = true;
                        if ui.button(*opt).clicked() {
                            picked = Some(opt);
                            ui.close();
                        }
                    }
                });
            if !any {
                // "Nothing matches what you typed" is not the same as "there is
                // nothing here", and a silently empty menu reads as broken.
                ui.label(empty_label);
            }
        });
    });
    if let Some(name) = picked {
        *text = name.to_string();
        resp.mark_changed();
    }
    resp
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

/// A `TextEdit`'s horizontal margin: the field the user sees is this wider than
/// the `desired_width` it is asked for.
const TEXT_EDIT_MARGIN: f32 = 8.0;

/// How many lines a wrapping field grows to before it starts scrolling instead.
///
/// Wrapping exists so a token or a URL can be *read* rather than scrubbed
/// through — but an environment variable holding a JWT is a hundred lines of
/// base64, and a field that tall swallows the panel it lives in, pushing every
/// other variable off the screen. Past this many lines the field stops growing
/// and scrolls within itself: the whole value is still there to scroll or
/// select through, and the rows around it stay where they are.
const FIELD_MAX_LINES: f32 = 6.0;

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
    let text_w = (width - TEXT_EDIT_MARGIN).max(16.0);
    // How tall this value wants to be, measured the way the `TextEdit` will lay
    // it out (same font, same wrap width), and the tallest it is allowed to get
    // (see `FIELD_MAX_LINES`). Measuring up front matters: a field that simply
    // *asked* for the maximum would make its row that tall whatever it holds,
    // which in a horizontal row (the method picker, the URL, Send) pushed the
    // URL onto a line of its own.
    let font_id = font.resolve(ui.style());
    let max_h = ui.ctx().fonts_mut(|f| f.row_height(&font_id)) * FIELD_MAX_LINES + TEXT_EDIT_MARGIN;
    let wanted_h = ui
        .ctx()
        .fonts_mut(|f| f.layout(text.clone(), font_id, color, text_w).size().y)
        + TEXT_EDIT_MARGIN;
    let field = |ui: &mut egui::Ui, text: &mut String| {
        ui.add(
            egui::TextEdit::multiline(text)
                .hint_text(hint)
                .text_color(color)
                .desired_width(text_w)
                .desired_rows(1)
                .return_key(None)
                .font(font.clone()),
        )
    };
    flat_fields(ui, |ui| {
        // A value that fits is drawn exactly as it always was: no viewport, no
        // reserved height, so the fields that hold one line — which is nearly
        // all of them — lay out to the pixel as before.
        if wanted_h <= max_h {
            // Allocated at the width the caller worked out, with the height
            // left to the content — `add_sized` would pin the height and undo
            // the growth this exists for.
            return ui
                .allocate_ui(egui::vec2(width, ui.spacing().interact_size.y), |ui| {
                    ui.set_width(width);
                    field(ui, text)
                })
                .inner;
        }
        // Only an over-long value gets the capped, scrolling viewport.
        ui.allocate_ui(egui::vec2(width, max_h), |ui| {
            ui.set_width(width);
            ui.set_height(max_h);
            egui::ScrollArea::vertical()
                .max_height(max_h)
                .auto_shrink([false, false])
                .show(ui, |ui| field(ui, text))
                .inner
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

/// [`selectable`] for a row of a list or tree: no frame unless it is the
/// selected one.
///
/// A segmented control (section tabs, the Raw body / Form fields switch) wants
/// every option framed, because the frames are what make it read as one control
/// with several settings. A list is the opposite: framing every row paints a
/// grey chip behind every name, and a list of chips reads as a list of disabled
/// things — which is exactly how the request tree looked once the theme gave
/// the raised surface a colour of its own to be seen in.
///
/// The border is drawn *inside* the row rather than around it, so no row ever
/// changes size. `Button::selectable` drops the frame entirely while the row
/// is neither selected nor hovered, and egui compensates a frame's stroke
/// inside the frame's own margin (`inner_margin = button_padding -
/// stroke.width`, with the stroke drawn back around it) — so a frameless row
/// comes out `2 * stroke.width` narrower and shorter than the same row
/// selected, and picking a request grew its name and nudged the rows after it.
/// Reserving the frame in every state instead would fix the movement by
/// padding *every* row out to the selected one's size, which just spaces the
/// whole list out; the button therefore keeps the compact, frameless geometry
/// (its own stroke is suppressed so the framed states can't claim the extra
/// pixels either) and the border is painted over the edge of the row we
/// already have.
pub fn selectable_row<'a>(
    ui: &mut egui::Ui,
    selected: bool,
    atoms: impl egui::IntoAtoms<'a>,
) -> egui::Response {
    // The state is read the way `Button` itself reads it (last frame's response
    // for the id this widget is about to take), so hover still lights the row
    // up rather than being flattened along with the resting state.
    let state = ui
        .ctx()
        .read_response(ui.next_auto_id())
        .map(|r| r.widget_state())
        .unwrap_or_default();
    // Exactly when egui would have framed the row: hovered/held, or selected.
    let framed = selected || state != egui::widget_style::WidgetState::Inactive;
    let visuals = *ui.visuals().widgets.state(state);
    let response = ui.add(egui::Button::selectable(selected, atoms).stroke(egui::Stroke::NONE));
    if framed && visuals.bg_stroke.width > 0.0 {
        ui.painter().rect_stroke(
            response.rect,
            visuals.corner_radius,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
    }
    response
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

/// Vertical gap between two tree rows, in pixels.
///
/// The app-wide `item_spacing` (see `theme.rs`) is the gap between *controls*,
/// and a tree is not a stack of controls: a row is one line of a list, and the
/// air meant to keep two buttons apart reads as a list that has been spaced
/// out. Rows are separated by their own, tighter rhythm so a folder of
/// requests can be scanned as one block — and so more of it fits on screen,
/// which is most of the point of a tree.
pub const TREE_ROW_SPACING: f32 = 3.0;

/// Padding above and below a tree row's label, in pixels.
///
/// Same argument as [`TREE_ROW_SPACING`], applied to the row itself: the
/// app-wide `button_padding` is what makes a *button* comfortable to hit, and
/// a row of a list doesn't need that much air around its one line of text.
/// Left at 2px rather than nothing so the selection border doesn't sit on the
/// letters, and egui's `interact_size` still keeps the row a sane click
/// target.
pub const TREE_ROW_PADDING: f32 = 2.0;

/// Put a `Ui` on the trees' denser rhythm: every list of rows in the app (the
/// request tree, the workspace tree, the environment list) is spaced the same
/// way, so the panels read as one thing rather than three lists that happen to
/// sit above each other.
pub fn tree_rhythm(ui: &mut egui::Ui) {
    let spacing = ui.spacing_mut();
    spacing.item_spacing.y = TREE_ROW_SPACING;
    spacing.button_padding.y = TREE_ROW_PADDING;
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
    // in the paint order now and fill it once the row's rect is known. The
    // hover wash shares the slot: a tree row is a click target, and without it
    // the environment list was the one tree in the app that gave no sign the
    // pointer was over a row at all.
    let band = ui.painter().add(egui::Shape::Noop);

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

    let rect = header.rect.expand2(egui::vec2(0.0, 2.0));
    let mut shapes = Vec::new();
    if header.hovered() {
        let visuals = ui.visuals().widgets.hovered;
        shapes.push(egui::Shape::rect_filled(
            rect,
            visuals.corner_radius,
            visuals.weak_bg_fill,
        ));
    }
    if let Some(color) = highlight {
        shapes.push(egui::Shape::rect_filled(
            rect,
            3.0,
            color.gamma_multiply(0.22),
        ));
        // A solid bar on the leading edge, so the row still reads as marked on
        // a theme whose background leaves the translucent band very faint.
        shapes.push(egui::Shape::rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            1.0,
            color,
        ));
    }
    if !shapes.is_empty() {
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
pub fn remove_width(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y + 2.0 * ui.spacing().button_padding.x
}

/// The width a text button will claim, so a row can reserve it before the
/// button is added (the value field to its left has to be sized first).
pub fn button_width(ui: &egui::Ui, text: &str) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let w = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER)
        .size()
        .x;
    w + 2.0 * ui.spacing().button_padding.x
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
///
/// `extract_row` receives the index of a row whose value was right-clicked and
/// sent to "Extract to parameter…". The table only reports it: which request
/// field that row *is* is the caller's business, and the entry it belongs to is
/// borrowed here, so the write-back happens back where the section was drawn.
#[allow(clippy::too_many_arguments)]
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
    extract_label: &str,
    extract_row: &mut Option<usize>,
    key_options: &[&'static str],
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
    ui.push_id(id, |ui| {
        table_rows(ui, |ui| {
            // Column titles, as in the terminal UI: without them a bare table of
            // text boxes gives no clue that the tick is "send this row" rather
            // than "select".
            table_row(ui, |ui| {
                // The Phosphor tick, not a bare `\u{2713}`: egui's bundled fonts
                // have no glyph for it, so the literal rendered as a tofu box.
                sized_header(ui, theme, super::icons::PASS, check_w);
                sized_header(ui, theme, key_label, key_w);
                sized_header(ui, theme, val_label, val_w);
                sized_header(ui, theme, s.hdr_description, desc_w);
                // The ✕ column has no title — the button says what it does.
                ui.allocate_space(egui::vec2(remove_width(ui), 1.0));
            });
            for i in 0..rows.len() {
                table_row(ui, |ui| {
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
                    // Sections with a well-known vocabulary (header and cookie
                    // names) get the caret; a query parameter's name is the
                    // API's business and there is nothing to suggest.
                    let k = if key_options.is_empty() {
                        sized_key(ui, key_w, &mut rows[i].key, key_hint, row_color)
                    } else {
                        suggesting_key(
                            ui,
                            key_w,
                            &mut rows[i].key,
                            key_hint,
                            row_color,
                            key_options,
                            s.gui_suggest_no_matches,
                        )
                    };
                    if k.changed() {
                        changed = true;
                    }
                    // The value is the one cell whose content is the request
                    // itself, so it is the one that wraps rather than truncates.
                    let v = wrapping_field(ui, val_w, &mut rows[i].value, val_hint, row_color);
                    if v.changed() {
                        changed = true;
                    }
                    if !rows[i].value.trim().is_empty() {
                        v.context_menu(|ui| {
                            if ui.button(extract_label).clicked() {
                                *extract_row = Some(i);
                                ui.close();
                            }
                        });
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
                    // Padded to the height of the fields rather than its own: a
                    // button's vertical padding makes it taller than the row it
                    // belongs to, so it hung two pixels below the fields and drew
                    // the eye down at the end of every row.
                    let x_w = remove_width(ui);
                    let hit = flat_buttons(ui, |ui| {
                        ui.add_sized(
                            [x_w, row_h],
                            egui::Button::new(RichText::new(super::icons::CLOSE).color(theme.err)),
                        )
                    });
                    if hit.on_hover_text(s.gui_remove).clicked() {
                        remove = Some(i);
                    }
                });
            }
        });
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
    let x_w = remove_width(ui);
    let row_h = ui.spacing().interact_size.y;
    ui.push_id(id, |ui| {
        table_rows(ui, |ui| {
            // See `kv_editor`: titled columns, minus the enabled tick this
            // table doesn't have, and top-aligned rows so they don't sag.
            table_row(ui, |ui| {
                sized_header(ui, theme, key_label, key_w);
                column_header(ui, theme, val_label);
            });
            for i in 0..rows.len() {
                table_row(ui, |ui| {
                    let k = sized_key(ui, key_w, &mut rows[i].0, key_hint, theme.text);
                    if k.changed() {
                        changed = true;
                    }
                    let val_w = (ui.available_width() - x_w - 8.0).max(40.0);
                    let v = wrapping_field(ui, val_w, &mut rows[i].1, val_hint, theme.text);
                    if v.changed() {
                        changed = true;
                    }
                    let hit = flat_buttons(ui, |ui| {
                        ui.add_sized(
                            [x_w, row_h],
                            egui::Button::new(RichText::new(super::icons::CLOSE).color(theme.err)),
                        )
                    });
                    if hit.clicked() {
                        remove = Some(i);
                    }
                });
            }
        });
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

    /// A modal dialog owns the keyboard as well as the pointer: the dimmed
    /// sheet already swallowed clicks, but Tab used to walk straight out of the
    /// dialog and into the panels behind it. Naming the dialog's layer as
    /// egui's modal layer is what confines focus to it.
    fn top_modal_layer_after(modeless: bool) -> Option<egui::LayerId> {
        let ctx = egui::Context::default();
        // Two passes: the layer is registered by the pass that draws the
        // window, and egui only reports it from the pass after that.
        for _ in 0..2 {
            let _ = ctx.run_ui(a_frame(), |ui| {
                let ctx = ui.ctx().clone();
                // Something focusable behind the dialog, which is what Tab used
                // to escape into.
                let mut behind = String::new();
                ui.add(egui::TextEdit::singleline(&mut behind));
                if modeless {
                    dialog_modeless(&ctx, "Dlg", None, |ui| ui.button("ok"));
                } else {
                    dialog(&ctx, "Dlg", None, |ui| ui.button("ok"));
                }
            });
        }
        ctx.memory(|m| m.top_modal_layer())
    }

    #[test]
    fn a_modal_dialog_confines_keyboard_focus_to_its_own_layer() {
        let layer = top_modal_layer_after(false);
        // The id is egui's own (a `Window` derives it from the title), so the
        // assertion is on what matters: a modal layer exists, and it is the
        // foreground one the dialog was put in rather than the panels below.
        assert_eq!(
            layer.map(|l| l.order),
            Some(egui::Order::Foreground),
            "the dialog's own layer is the modal one, so Tab can't leave it"
        );
        assert_ne!(layer, Some(egui::LayerId::background()));
    }

    /// The modeless shell is the opposite case by design: it reports progress
    /// beside work the user is still doing, so it must not capture the keyboard.
    #[test]
    fn a_modeless_dialog_leaves_the_keyboard_alone() {
        assert_eq!(top_modal_layer_after(true), None);
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
        let _ = ctx.run_ui(a_frame(), |ui| {
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
                let _ = ctx.run_ui(a_frame(), |ui| {
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
            let _ = ctx.run_ui(input.clone(), |ui| {
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
                            // The *field*, not the text inside it: a
                            // `TextEdit`'s response rect is its text area, and
                            // the frame the user sees is that plus the margin.
                            rendered = sized_key(ui, key_w, &mut text, "", Color32::PLACEHOLDER)
                                .rect
                                .width()
                                + TEXT_EDIT_MARGIN;
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
                        ui,
                        &theme,
                        &s,
                        "kv",
                        &mut rows,
                        "name",
                        "value",
                        "Header",
                        "Value",
                        "Extract",
                        &mut None,
                        &[],
                    );
                    w = ui.min_rect().width() - before;
                },
            );
        }
        w
    }

    /// Render a `kv_editor` and report every piece of text it painted.
    fn kv_texts(rows: &mut Vec<KvRow>, key_options: &[&'static str]) -> Vec<String> {
        let ctx = egui::Context::default();
        let theme = GuiTheme::from_spec(&crate::theme::default_preset());
        let s = Strings::for_language(&crate::i18n::Language::English);
        let mut out = Vec::new();
        for _ in 0..4 {
            out.clear();
            let full = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(900.0, 400.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    kv_editor(
                        ui,
                        &theme,
                        &s,
                        "kv",
                        rows,
                        "name",
                        "value",
                        "Header",
                        "Value",
                        "Extract",
                        &mut None,
                        key_options,
                    );
                },
            );
            for cs in &full.shapes {
                collect_text(&cs.shape, &mut out);
            }
        }
        out
    }

    fn collect_text(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
            egui::Shape::Vec(v) => {
                for s in v {
                    collect_text(s, out);
                }
            }
            _ => {}
        }
    }

    /// The complaint: the terminal UI offers the common header names in its Key
    /// column and the GUI simply didn't. The caret is the affordance, so it has
    /// to be *there* — and only where there is a vocabulary to offer, since a
    /// query parameter's name is the API's business.
    #[test]
    fn a_key_column_with_a_vocabulary_gets_a_caret_and_one_without_does_not() {
        let mut rows = vec![KvRow::new("Accept", "application/json")];
        let with = kv_texts(&mut rows, crate::http::COMMON_HEADERS);
        assert!(
            with.iter().any(|t| t == super::super::icons::CARET_DOWN),
            "the headers table offers the list: {with:?}"
        );

        let mut rows = vec![KvRow::new("page", "2")];
        let without = kv_texts(&mut rows, &[]);
        assert!(
            !without.iter().any(|t| t == super::super::icons::CARET_DOWN),
            "a query parameter has nothing to suggest: {without:?}"
        );
    }

    /// Picking a name fills the cell, and the list is the same one the terminal
    /// UI narrows — both front-ends read `crate::http`.
    #[test]
    fn the_key_vocabulary_is_the_one_both_front_ends_share() {
        assert!(crate::http::COMMON_HEADERS.contains(&"Content-Type"));
        assert_eq!(
            crate::http::filter_headers("auth"),
            vec!["Authorization"],
            "the caret narrows to what has been typed"
        );
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

    /// Selecting a row must not resize it: `Button::selectable` drops its frame
    /// while inactive, and the stroke width the frame folds into its margin
    /// went with it, so picking a request in the workspace tree grew that row
    /// and shuffled the rows after it down the panel. The fix must not be paid
    /// for by padding every row out to the framed size either — a list of
    /// requests spaced out by the border it isn't showing is its own bug — so
    /// the frameless size is the one every state has to match.
    #[test]
    fn selecting_a_row_leaves_it_exactly_where_it_was() {
        let theme = GuiTheme::from_spec(&crate::theme::default_preset());
        let ctx = egui::Context::default();
        theme.apply(&ctx);
        // A pointer parked off the rows: hover survives from frame to frame,
        // so every run has to say where the pointer is, not just the hover one.
        let away = egui::pos2(390.0, 190.0);
        let rows = |pointer: egui::Pos2, body: &mut dyn FnMut(&mut egui::Ui, usize)| {
            // Twice: the first pass has no stored response to read a state
            // from, so the steady state is the second one.
            let mut shapes = Vec::new();
            for _ in 0..2 {
                let full = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::pos2(0.0, 0.0),
                            egui::vec2(400.0, 200.0),
                        )),
                        events: vec![egui::Event::PointerMoved(pointer)],
                        ..Default::default()
                    },
                    |ui| {
                        for i in 0..3 {
                            body(ui, i);
                        }
                    },
                );
                shapes = full.shapes;
            }
            shapes
        };
        let with = |selected: bool, pointer: egui::Pos2| {
            let mut rects = Vec::new();
            let shapes = rows(pointer, &mut |ui, i| {
                let r = selectable_row(ui, selected && i == 1, "GET /one").rect;
                if i == 0 {
                    rects.clear();
                }
                rects.push(r);
            });
            (rects, shapes)
        };

        let (quiet, _) = with(false, away);
        let (picked, picked_shapes) = with(true, away);
        assert_eq!(
            quiet, picked,
            "selecting the middle row moved it or its neighbours"
        );
        let (hovered, _) = with(false, quiet[0].center());
        assert_eq!(quiet, hovered, "hovering a row moved it or its neighbours");

        // The bare, unframed button is the size a row has always been.
        let mut bare = Vec::new();
        rows(away, &mut |ui, i| {
            let r = ui
                .add(egui::Button::selectable(false, "GET /one").frame_when_inactive(false))
                .rect;
            if i == 0 {
                bare.clear();
            }
            bare.push(r);
        });
        assert_eq!(
            bare, quiet,
            "rows grew to make room for a border they aren't drawing"
        );

        // The border still has to be *there*: it is drawn inside the selected
        // row's own rect rather than around it.
        let mut border = None;
        for cs in &picked_shapes {
            stroked_rects(&cs.shape, &mut |rect, width| {
                if width > 0.0 && rect == picked[1] {
                    border = Some(width);
                }
            });
        }
        assert!(
            border.is_some(),
            "the selected row lost its border: {picked_shapes:?}"
        );
    }

    /// Walk a shape tree, reporting every rectangle drawn with a visible
    /// stroke (rect, stroke width).
    fn stroked_rects(shape: &egui::Shape, out: &mut dyn FnMut(egui::Rect, f32)) {
        match shape {
            egui::Shape::Rect(r) if r.stroke.width > 0.0 => out(r.rect, r.stroke.width),
            egui::Shape::Vec(v) => {
                for s in v {
                    stroked_rects(s, out);
                }
            }
            _ => {}
        }
    }

    /// Every row of a list framed is a list that reads as disabled — the
    /// complaint that started this: the request tree looked "greyed out,
    /// almost like they are disabled" once the theme gave `raised` a colour
    /// distinct enough from the panel to be seen. A segmented control still
    /// wants its frames, so the two must part company.
    #[test]
    fn an_unselected_row_paints_no_chip_but_an_unselected_tab_does() {
        let theme = GuiTheme::from_spec(&crate::theme::default_preset());
        let ctx = egui::Context::default();
        theme.apply(&ctx);
        let chips = |body: &dyn Fn(&mut egui::Ui)| {
            let mut out = Vec::new();
            for _ in 0..2 {
                let full = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::pos2(0.0, 0.0),
                            egui::vec2(400.0, 200.0),
                        )),
                        ..Default::default()
                    },
                    |ui| body(ui),
                );
                out.clear();
                for cs in &full.shapes {
                    rects_filled(&cs.shape, theme.raised(), &mut out);
                }
            }
            out.len()
        };

        assert_eq!(
            chips(&|ui| {
                selectable_row(ui, false, "GET /one");
                selectable_row(ui, false, "GET /two");
            }),
            0,
            "an unselected list row is content, not a chip"
        );
        assert_eq!(
            chips(&|ui| {
                selectable(ui, false, "Params");
                selectable(ui, false, "Headers");
            }),
            2,
            "a segmented control keeps every option framed"
        );
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
                ui,
                &theme,
                &s,
                "kv",
                &mut rows,
                "name",
                "value",
                "Header",
                "Value",
                "Extract",
                &mut None,
                &[],
            );
        });
        assert_eq!(rects.len(), 6, "three fields per row, got {rects:?}");
        for row in rects.chunks(3) {
            let first = row[0];
            for (i, r) in row.iter().enumerate() {
                // Exactly, not nearly: a fifth of a pixel is invisible in the
                // model and a whole pixel on screen once the row lands on a
                // fractional y, which is what made the table look as though it
                // sloped. Any tolerance here is a tolerance for the bug.
                assert!(
                    (r.top() - first.top()).abs() < 0.01,
                    "field {i} sits at {} but the row starts at {}",
                    r.top(),
                    first.top()
                );
                assert!(
                    (r.height() - first.height()).abs() < 0.01,
                    "field {i} is {} tall, the row is {}",
                    r.height(),
                    first.height()
                );
            }
        }
    }

    /// The remove ✕ has to be the height of the fields it sits between. A
    /// button's own vertical padding makes it taller, so it hung below the row
    /// and drew the eye downwards at the end of every line — the sag again,
    /// this time at the right-hand edge.
    #[test]
    fn the_remove_button_does_not_hang_below_the_row() {
        let theme = GuiTheme::from_spec(&crate::theme::default_preset());
        let s = Strings::for_language(&crate::i18n::Language::English);
        let mut rows = vec![KvRow::new("Accept", "application/json")];
        let ctx = egui::Context::default();
        theme.apply(&ctx);
        let mut button_fill = Color32::TRANSPARENT;
        let mut fields = Vec::new();
        let mut buttons = Vec::new();
        let full = ctx.run_ui(a_frame(), |ui| {
            button_fill = ui.visuals().widgets.inactive.weak_bg_fill;
            kv_editor(
                ui,
                &theme,
                &s,
                "kv",
                &mut rows,
                "name",
                "value",
                "Header",
                "Value",
                "Extract",
                &mut None,
                &[],
            );
        });
        for cs in &full.shapes {
            rects_filled(&cs.shape, theme.field(), &mut fields);
            rects_filled(&cs.shape, button_fill, &mut buttons);
        }
        let field = *fields.first().expect("a field was painted");
        // The ✕ is the rightmost control on the row; the "+ Add" button below
        // it starts at the left edge.
        let x = buttons
            .iter()
            .filter(|b| b.top() < field.bottom())
            .max_by(|a, b| a.left().total_cmp(&b.left()))
            .copied()
            .expect("the remove button was painted");
        assert!(
            x.height() <= field.height() + 0.01,
            "the ✕ is {} tall next to a {} field",
            x.height(),
            field.height()
        );
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
                ui,
                &theme,
                &s,
                "kv",
                &mut rows,
                "name",
                "value",
                "Header",
                "Value",
                "Extract",
                &mut None,
                &[],
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

    /// A tree row is a click target, and the environment list — built from
    /// these headers — was the one list in the app that gave no sign the
    /// pointer was over a row.
    #[test]
    fn a_tree_row_lights_up_under_the_pointer() {
        let theme = GuiTheme::from_spec(&crate::theme::default_preset());
        let ctx = egui::Context::default();
        theme.apply(&ctx);
        let wash = ctx
            .style_of(egui::Theme::Dark)
            .visuals
            .widgets
            .hovered
            .weak_bg_fill;
        let washes = |pointer: egui::Pos2| {
            let mut out = Vec::new();
            for _ in 0..2 {
                let full = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::pos2(0.0, 0.0),
                            egui::vec2(300.0, 200.0),
                        )),
                        events: vec![egui::Event::PointerMoved(pointer)],
                        ..Default::default()
                    },
                    |ui| {
                        tree_header(ui, "hover-row", false, RichText::new("dev"), |_ui| {});
                    },
                );
                out.clear();
                for cs in &full.shapes {
                    rects_filled(&cs.shape, wash, &mut out);
                }
            }
            out.len()
        };

        assert_eq!(
            washes(egui::pos2(280.0, 190.0)),
            0,
            "a row at rest is plain"
        );
        assert_eq!(
            washes(egui::pos2(40.0, 8.0)),
            1,
            "the row under the pointer should say so"
        );
    }

    /// A field that *asks* for its maximum height makes its row that tall
    /// whatever it holds — which is what pushed the URL off the line it shares
    /// with the method picker and the Send button. A value that fits has to
    /// take exactly the room it needs.
    #[test]
    fn a_short_value_does_not_reserve_the_room_a_long_one_would() {
        let theme = GuiTheme::from_spec(&crate::theme::default_preset());
        let ctx = egui::Context::default();
        theme.apply(&ctx);
        let row_height = |value: Option<&str>| {
            let mut text = value.unwrap_or_default().to_string();
            let mut out = 0.0;
            // Galley sizes settle from the previous pass.
            for _ in 0..3 {
                let _ = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::pos2(0.0, 0.0),
                            egui::vec2(400.0, 600.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        // The URL row: a picker, the field, and a button.
                        ui.horizontal(|ui| {
                            let _ = ui.button("POST");
                            if value.is_some() {
                                wrapping_field(ui, 200.0, &mut text, "", Color32::WHITE);
                            }
                            let _ = ui.button("Send");
                            out = ui.min_rect().height();
                        });
                    },
                );
            }
            out
        };

        // The row the controls alone make, against the row they make with a
        // one-line URL between them.
        let controls = row_height(None);
        let with_url = row_height(Some("{{url}}/create_session"));
        assert!(
            with_url <= controls + 2.0,
            "a one-line URL made its row {with_url}px tall, next to {controls}px of controls"
        );
    }

    /// A JWT in an environment variable is a hundred wrapped lines, and a
    /// field that tall pushed every other variable out of the panel. Past a
    /// few lines the field scrolls within itself instead of growing.
    #[test]
    fn a_very_long_value_stops_growing_and_scrolls_instead() {
        let theme = GuiTheme::from_spec(&crate::theme::default_preset());
        let ctx = egui::Context::default();
        theme.apply(&ctx);
        let height = |value: &str| {
            let mut text = value.to_string();
            let mut out = 0.0;
            // Galley sizes settle from the previous pass.
            for _ in 0..3 {
                let _ = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::pos2(0.0, 0.0),
                            egui::vec2(300.0, 600.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        ui.scope(|ui| {
                            wrapping_field(ui, 120.0, &mut text, "", Color32::WHITE);
                            out = ui.min_rect().height();
                        });
                    },
                );
            }
            out
        };

        let one_line = height("short");
        let jwt = height(&"eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.".repeat(40));
        assert!(
            jwt > one_line,
            "a value that needs two lines still gets them"
        );
        assert!(
            jwt <= one_line * (FIELD_MAX_LINES + 1.0),
            "a huge value took the whole panel ({jwt}px for a {one_line}px row)"
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
    let shown = window.show(ctx, |ui| {
        if let Some(w) = min_width {
            ui.set_min_width(w);
        }
        add(ui)
    });
    // The sheet stops the *pointer*, but keyboard focus is a separate,
    // context-wide list: Tab out of the last field of a dialog and egui happily
    // walked on into the panels behind it, typing into a request the dialog was
    // asking about. Naming the dialog's layer as the modal one confines Tab and
    // the arrow keys to it, which is the behaviour every other desktop dialog
    // has. Registered after the window is shown because it is the window that
    // owns the layer; egui applies it from the next frame, as it does for its
    // own `Modal`.
    if modal && let Some(r) = &shown {
        ctx.memory_mut(|m| m.set_modal_layer(r.response.layer_id));
    }
    let inner = shown.and_then(|r| r.inner);
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
