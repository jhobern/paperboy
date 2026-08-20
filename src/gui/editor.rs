//! Centre-top panel: the request editor. A Postman-style method/URL bar plus
//! section tabs (Params, Headers, Body, Auth, Cookies, Options, Asserts,
//! Captures, Code) editing the selected [`HurlEntry`] in place.

use std::collections::{HashMap, HashSet};

use eframe::egui::text::LayoutJob;
use eframe::egui::{self, Color32, FontId, RichText, TextFormat};

use crate::hurl::{FormField, FormFieldKind, HurlEntry, KvRow};
use crate::i18n::Strings;
use crate::request::{SubstInfo, SubstKind, apply_request_json, build_request_json};

use super::app::{EditorSection, GuiApp};
use super::theme::GuiTheme;
use super::widgets;

/// Inline warning marker shown immediately before a substituted value whose
/// Global Environment source is shadowed by the collection's linked
/// Environment — matches the terminal UI's `SHADOW_ICON`.
const SHADOW_ICON: &str = "!";

/// Which substitution [`SubstKind`]s (and whether any shadowing) were actually
/// rendered in the Code preview, so the legend shows only the relevant dots.
#[derive(Default)]
struct SubstSeen {
    loaded: bool,
    literal: bool,
    pending: bool,
    failed: bool,
    undefined: bool,
    shadowed: bool,
}

impl SubstSeen {
    fn mark(&mut self, kind: SubstKind) {
        match kind {
            SubstKind::Loaded => self.loaded = true,
            SubstKind::Literal => self.literal = true,
            SubstKind::Pending => self.pending = true,
            SubstKind::Failed => self.failed = true,
            SubstKind::Undefined => self.undefined = true,
        }
    }

    fn any(&self) -> bool {
        self.loaded || self.literal || self.pending || self.failed || self.undefined
    }
}

/// The colour a substitution is drawn in, by resolution status — mirrors the
/// terminal UI's `subst_color` so both front-ends agree.
fn subst_color(kind: SubstKind, th: &GuiTheme) -> Color32 {
    match kind {
        SubstKind::Literal => th.subst,
        SubstKind::Loaded => th.ok,
        SubstKind::Pending => th.pending,
        SubstKind::Failed => th.err,
        SubstKind::Undefined => th.err,
    }
}

/// Render the substitution legend (coloured dots for each status present, plus
/// the shadowed hint) beneath the Code preview, matching the terminal UI.
fn subst_legend(ui: &mut egui::Ui, seen: &SubstSeen, th: &GuiTheme, s: &Strings) {
    if !seen.any() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for (present, word, color) in [
            (seen.loaded, s.subst_hint_loaded, th.ok),
            (seen.literal, s.subst_hint_literal, th.subst),
            (seen.pending, s.subst_hint_loading, th.pending),
            (seen.failed, s.subst_hint_missing, th.err),
            (seen.undefined, s.subst_hint_undefined, th.err),
        ] {
            if present {
                ui.colored_label(color, format!("\u{25cf} {word}"));
            }
        }
        if seen.shadowed {
            ui.colored_label(
                th.pending,
                format!("{SHADOW_ICON} {}", s.subst_hint_shadowed),
            );
        }
    });
}

/// Colour-code every `{{ VAR }}` token *in place* — i.e. without substituting
/// its value or inserting any marker — so the produced [`LayoutJob`] lays out
/// exactly the characters it was given. This is what an editable Code buffer
/// needs: egui's `TextEdit` layouter must return a galley for the buffer's own
/// text (a length change would corrupt the cursor). Known placeholders are
/// tinted by resolution status; unknown ones keep the default colour. `seen`
/// records which statuses appeared, for the legend.
fn highlight_code_editable(
    text: &str,
    vars: &HashMap<String, SubstInfo>,
    shadowed: &HashSet<String>,
    th: &GuiTheme,
    font: FontId,
    seen: &mut SubstSeen,
) -> LayoutJob {
    let fmt = |color: Color32| TextFormat::simple(font.clone(), color);
    let mut job = LayoutJob::default();
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        let Some(close_rel) = rest[open + 2..].find("}}") else {
            break;
        };
        let close = open + 2 + close_rel;
        let end = close + 2;
        let inner = rest[open + 2..close].trim();
        if open > 0 {
            job.append(&rest[..open], 0.0, fmt(th.text));
        }
        let token = &rest[open..end];
        match vars.get(inner) {
            Some(info) => {
                seen.mark(info.kind);
                if shadowed.contains(inner) {
                    seen.shadowed = true;
                }
                job.append(token, 0.0, fmt(subst_color(info.kind, th)));
            }
            // Nothing defines this one. It used to fall through as ordinary
            // body text, which made the only variable the user *can't* fix by
            // waiting the only one that looked perfectly fine.
            None => {
                seen.mark(SubstKind::Undefined);
                job.append(token, 0.0, fmt(subst_color(SubstKind::Undefined, th)));
            }
        }
        rest = &rest[end..];
    }
    if !rest.is_empty() {
        job.append(rest, 0.0, fmt(th.text));
    }
    job
}

/// Which substitution statuses appear in `text` — the legend's whole input.
///
/// The legend used to be collected by running the *highlighter* over the buffer
/// and throwing the resulting [`LayoutJob`] away, so every frame paid for a
/// second full colouring pass to learn four booleans. This walks the same
/// placeholders without building anything, and stops as soon as it has seen
/// everything there is to see.
fn substitution_statuses(
    text: &str,
    vars: &HashMap<String, SubstInfo>,
    shadowed: &HashSet<String>,
) -> SubstSeen {
    let mut seen = SubstSeen::default();
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        let Some(close_rel) = rest[open + 2..].find("}}") else {
            break;
        };
        let close = open + 2 + close_rel;
        let inner = rest[open + 2..close].trim();
        match vars.get(inner) {
            Some(info) => {
                seen.mark(info.kind);
                if shadowed.contains(inner) {
                    seen.shadowed = true;
                }
            }
            None => seen.mark(SubstKind::Undefined),
        }
        rest = &rest[close + 2..];
    }
    seen
}

/// [`highlight_code_editable`], reused between frames while nothing it depends
/// on has changed. See the report source editor's equivalent for why: the
/// layouter is asked for a job on every frame, and rebuilding it re-scans the
/// whole buffer.
fn cached_code_job(
    ui: &egui::Ui,
    text: &str,
    vars: &HashMap<String, SubstInfo>,
    shadowed: &HashSet<String>,
    th: &GuiTheme,
    font: FontId,
) -> LayoutJob {
    use std::hash::{Hash, Hasher};
    let id = egui::Id::new("code_edit_highlight");
    let mut h = std::collections::hash_map::DefaultHasher::new();
    crate::gui::report_editor::fnv1a(text.as_bytes(), crate::gui::report_editor::FNV_OFFSET)
        .hash(&mut h);
    font.size.to_bits().hash(&mut h);
    // Both are hash maps/sets, so combine each entry order-independently.
    let mut refs = 0u64;
    for (k, info) in vars {
        // Only the name and the kind, deliberately: the *editable* highlighter
        // leaves `{{ VAR }}` in place and merely colours it, so the resolved
        // value never reaches the buffer and hashing it would be dead cost.
        // (`highlight_code`, which does substitute, is not cached through here.)
        refs ^= crate::gui::report_editor::fnv1a(
            k.as_bytes(),
            crate::gui::report_editor::FNV_OFFSET ^ info.kind as u64,
        );
    }
    for k in shadowed {
        refs ^= crate::gui::report_editor::fnv1a(k.as_bytes(), 0x9e37_79b9_7f4a_7c15);
    }
    refs.hash(&mut h);
    // The colours themselves come from the theme.
    format!("{:?}", (th.text, th.subst, th.pending, th.err, th.ok)).hash(&mut h);
    let key = h.finish();

    if let Some((cached_key, job)) = ui.data(|d| d.get_temp::<(u64, LayoutJob)>(id))
        && cached_key == key
    {
        return job;
    }
    let mut ignored = SubstSeen::default();
    let job = highlight_code_editable(text, vars, shadowed, th, font, &mut ignored);
    ui.data_mut(|d| d.insert_temp(id, (key, job.clone())));
    job
}

/// Re-parse edited Code-view `text` back into the selected entry. On success it
/// applies the result (preserving the UI-only `user_added` flag for Hurl; the
/// JSON view carries over the fields it doesn't expose from the current entry)
/// and clears the error; on failure it keeps the buffer untouched and records
/// the parse error. Returns whether the entry actually changed.
fn apply_code_edit(
    session: &mut crate::session::Session,
    code_edit: &mut super::app::CodeEdit,
    strings: &Strings,
    ci: usize,
    sel: usize,
    show_hurl: bool,
    text: &str,
) -> bool {
    if show_hurl {
        let entries = crate::hurl::parse_hurl(text);
        if entries.len() == 1 {
            let mut parsed = entries.into_iter().next().unwrap();
            let entry = &mut session.collections[ci].entries[sel];
            // `user_added` is UI-only and never written to Hurl text, so a
            // reparse always drops it; carry it over from the live entry.
            parsed.user_added = entry.user_added;
            *entry = parsed;
            code_edit.error = None;
            true
        } else {
            code_edit.error = Some(
                crate::hurl::parse_hurl_error(text)
                    .unwrap_or_else(|| strings.gui_code_parse_error.to_string()),
            );
            false
        }
    } else {
        let base = session.collections[ci].entries[sel].clone();
        match apply_request_json(&base, text) {
            Ok(parsed) => {
                session.collections[ci].entries[sel] = parsed;
                code_edit.error = None;
                true
            }
            Err(e) => {
                code_edit.error = Some(e);
                false
            }
        }
    }
}

/// The editable Code section: a full-height `TextEdit` holding either the Hurl
/// source or the resolved-JSON preview of the selected request, re-parsed on
/// every edit back into the entry. The buffer is the source of truth while you
/// type (never clobbered mid-edit); it re-syncs from the entry when you switch
/// request/representation or return to the tab. A parse failure keeps your text
/// and shows the error instead of discarding it. Returns whether the entry
/// changed.
#[allow(clippy::too_many_arguments)]
fn draw_code_section(
    app: &mut GuiApp,
    ui: &mut egui::Ui,
    theme: &GuiTheme,
    ci: usize,
    sel: usize,
    code_show_hurl: &mut bool,
    subst_vars: &HashMap<String, SubstInfo>,
    shadowed: &HashSet<String>,
) -> bool {
    let mut changed = false;

    // Representation toggle (Hurl source vs. resolved JSON), mirroring the TUI.
    let (lbl_json, lbl_hurl) = (
        app.strings.gui_code_repr_json,
        app.strings.gui_code_repr_hurl,
    );
    ui.horizontal(|ui| {
        if widgets::selectable(ui, !*code_show_hurl, lbl_json).clicked() {
            *code_show_hurl = false;
            app.code_edit.key = None;
        }
        if widgets::selectable(ui, *code_show_hurl, lbl_hurl).clicked() {
            *code_show_hurl = true;
            app.code_edit.key = None;
        }
    });
    ui.add_space(4.0);

    // Re-sync the buffer from the entry when it reflects a different
    // request/representation than we're now showing; otherwise leave the user's
    // in-progress edits untouched.
    let key = (ci, sel, *code_show_hurl);
    if app.code_edit.key != Some(key) {
        let entry = &app.session.collections[ci].entries[sel];
        app.code_edit.buf = if *code_show_hurl {
            entry.to_hurl()
        } else {
            build_request_json(entry)
        };
        app.code_edit.key = Some(key);
        app.code_edit.error = None;
    }

    // Legend: which substitution statuses appear in the current buffer.
    let seen = substitution_statuses(&app.code_edit.buf, subst_vars, shadowed);

    // A fixed-height editor that fills the panel (not shrink-wrapped to its
    // text), leaving room below for the legend and any parse error.
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
    let reserved = 44.0
        + if app.code_edit.error.is_some() {
            24.0
        } else {
            0.0
        };
    let editor_h = (ui.available_height() - reserved).max(row_h * 6.0);
    let rows = (editor_h / row_h).floor().max(6.0) as usize;

    let subst_vars_l = subst_vars;
    let shadowed_l = shadowed;
    let theme_l = theme;
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let mut job = cached_code_job(ui, buf.as_str(), subst_vars_l, shadowed_l, theme_l, font);
        job.wrap.max_width = wrap;
        ui.fonts_mut(|f| f.layout_job(job))
    };

    let resp = egui::ScrollArea::vertical()
        .max_height(editor_h)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut app.code_edit.buf)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(rows)
                    .layouter(&mut layouter),
            )
        });

    if resp.inner.changed() {
        let text = app.code_edit.buf.clone();
        if apply_code_edit(
            &mut app.session,
            &mut app.code_edit,
            &app.strings,
            ci,
            sel,
            *code_show_hurl,
            &text,
        ) {
            changed = true;
        }
    }

    ui.add_space(4.0);
    if let Some(err) = &app.code_edit.error {
        ui.colored_label(theme.err, format!("\u{26a0} {err}"));
    }
    subst_legend(ui, &seen, theme, &app.strings);
    changed
}

pub fn ui(app: &mut GuiApp, ui: &mut egui::Ui) {
    let ci = app.active_ci();
    let theme = app.theme;

    if app.session.collections[ci].entries.is_empty() {
        let no_requests = app.strings.gui_no_requests_editor;
        let new_request_btn = format!("{} {}", super::icons::PLUS, app.strings.gui_new_request_btn);
        let new_request_title = app.strings.gui_new_request;
        ui.vertical_centered(|ui| {
            ui.add_space(30.0);
            ui.colored_label(theme.dim, no_requests);
            if ui.button(new_request_btn).clicked() {
                let mut e = HurlEntry::default();
                e.method = "GET".into();
                e.url = app.session.vars.base_url.clone();
                e.title = new_request_title.into();
                e.user_added = true;
                let col = &mut app.session.collections[ci];
                col.entries.push(e);
                col.selected_entry = 0;
                col.invalidate_request_json();
            }
        });
        return;
    }

    let sel = app.session.collections[ci]
        .selected_entry
        .min(app.session.collections[ci].entries.len() - 1);

    let mut changed = false;
    let mut send = false;
    // A right-click "Extract to parameter…" anywhere in the editor lands here
    // and is turned into a dialog below, once the borrow of the entry the menu
    // was drawn over has ended.
    let mut extract: Option<PendingExtract> = None;
    let ex_label = app.strings.gui_extract_parameter;
    let section = app.editor_section;
    // Local copy of the Code-view toggle; written back after the borrow of the
    // selected entry ends (egui closures can't borrow `app` again mid-frame).
    let mut code_show_hurl = app.show_hurl;

    // ── Name / Method / URL / Send bar ────────────────────────────────────
    // Mirrors the TUI edit-request wizard, which shows the request Name above
    // the Method/URL row. The Name is the display title in the request tree.
    let send_label = format!("{} {}", app.strings.gui_send, super::icons::PLAY);
    // The keyboard shortcuts have nowhere else to announce themselves.
    let app_strings_send_tooltip = app.strings.gui_send_tooltip;
    {
        let entry = &mut app.session.collections[ci].entries[sel];
        let name_label = app.strings.gui_name;
        let url_hint = app.strings.gui_hint_url;
        ui.horizontal(|ui| {
            ui.label(RichText::new(name_label).color(theme.dim));
            let name = widgets::flat_fields(ui, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut entry.title)
                        .desired_width(f32::INFINITY)
                        .hint_text(name_label),
                )
            });
            if name.changed() {
                changed = true;
            }
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if widgets::method_combo(ui, &theme, "method", &mut entry.method) {
                changed = true;
            }
            let send_w = 92.0;
            // Wrapped, not scrolled: a URL with a path, a query string and a
            // couple of `{{ variables }}` in it is the single longest thing on
            // this screen, and reading it a dozen characters at a time through
            // a one-line viewport was the worst case of the problem.
            let url = widgets::wrapping_field_font(
                ui,
                (ui.available_width() - send_w).max(80.0),
                &mut entry.url,
                url_hint,
                theme.text,
                egui::TextStyle::Monospace,
            );
            if url.changed() {
                changed = true;
            }
            // Usually only a segment of a URL varies, so a selection narrows
            // the extraction the same way it does in the body.
            extract_menu(
                &url,
                ex_label,
                ExtractTarget::Url,
                &entry.url,
                true,
                &mut extract,
            );
            let btn = ui
                .add_sized(
                    [80.0, 24.0],
                    egui::Button::new(RichText::new(send_label).strong().color(theme.select_fg))
                        .fill(theme.accent),
                )
                .on_hover_text(app_strings_send_tooltip);
            if btn.clicked() {
                send = true;
            }
        });
    }

    ui.add_space(4.0);

    // ── Section tabs ──────────────────────────────────────────────────────
    {
        let entry = &app.session.collections[ci].entries[sel];
        let params_n = entry.queries.len();
        let headers_n = entry.headers.len();
        let cookies_n = entry.cookies.len();
        let options_n = entry.options.len();
        let asserts_n = entry.asserts.len();
        let captures_n = entry.captures.len();
        let has_body = entry.body.as_ref().map(|b| !b.is_empty()).unwrap_or(false)
            || !entry.form_fields.is_empty();
        let has_auth = entry.basic_auth.is_some();
        let mut cur = app.editor_section;
        let st = &app.strings;
        let tabs = [
            (EditorSection::All, st.tab_all.to_string()),
            (
                EditorSection::Params,
                format!("{}{}", st.gui_sec_params, widgets::count_suffix(params_n)),
            ),
            (
                EditorSection::Headers,
                format!("{}{}", st.gui_sec_headers, widgets::count_suffix(headers_n)),
            ),
            (
                EditorSection::Body,
                format!("{}{}", st.gui_sec_body, if has_body { " •" } else { "" }),
            ),
            (
                EditorSection::Auth,
                format!("{}{}", st.gui_sec_auth, if has_auth { " •" } else { "" }),
            ),
            (
                EditorSection::Cookies,
                format!("{}{}", st.gui_sec_cookies, widgets::count_suffix(cookies_n)),
            ),
            (
                EditorSection::Options,
                format!("{}{}", st.gui_sec_options, widgets::count_suffix(options_n)),
            ),
            (
                EditorSection::Asserts,
                format!("{}{}", st.gui_sec_asserts, widgets::count_suffix(asserts_n)),
            ),
            (
                EditorSection::Captures,
                format!(
                    "{}{}",
                    st.gui_sec_captures,
                    widgets::count_suffix(captures_n)
                ),
            ),
            (EditorSection::Code, st.gui_sec_code.to_string()),
        ];
        ui.horizontal_wrapped(|ui| {
            for (value, label) in &tabs {
                let selected = cur == *value;
                let mut text = RichText::new(label);
                text = if selected {
                    text.strong().color(theme.text)
                } else {
                    text.color(theme.dim)
                };
                if super::widgets::selectable(ui, selected, text).clicked() {
                    cur = *value;
                }
            }
        });
        app.editor_section = cur;
    }
    ui.separator();

    // Substitution preview data for the Code view: how each `{{ VAR }}` should
    // be shown/coloured, and which keys the linked env shadows. Computed here
    // (before the entry is mutably borrowed by the section closure) and only
    // when the Code tab is active, since it borrows the whole collection.
    //
    // Deliberately rebuilt each frame rather than cached: measured at 39us for
    // a 60-request collection against a 40-variable environment, and a cache
    // key would have to walk the same entries and variables to notice a change,
    // so it would cost about what it saved. The highlighter downstream *is*
    // cached, which is where the frame time actually went.
    let (subst_vars, shadowed) = if section == EditorSection::Code {
        let env = app.session.effective_env(ci);
        (
            crate::request::subst_map(&app.session.collections[ci], env.as_ref()),
            app.session.shadowed_env_keys(ci),
        )
    } else {
        (HashMap::new(), HashSet::new())
    };

    // ── Section body ──────────────────────────────────────────────────────
    // The editable Code buffer only mirrors the Code tab; drop its identity
    // whenever we leave so returning to Code re-syncs from the entry (which may
    // have been edited from another section in the meantime).
    if section != EditorSection::Code {
        app.code_edit.key = None;
    }
    if section == EditorSection::Code {
        // The Code editor needs mutable access to both `app.code_edit` and the
        // collection (to apply reparsed text), so it can't run inside the
        // closure below that borrows the selected entry.
        if draw_code_section(
            app,
            ui,
            &theme,
            ci,
            sel,
            &mut code_show_hurl,
            &subst_vars,
            &shadowed,
        ) {
            changed = true;
        }
    } else {
        // Resolved up front: the section body borrows the collection mutably,
        // so the session can't be consulted from inside it.
        // Which Form/Multipart file value asked for a picker this frame, if
        // any. Collected below, once the body has released the collection.
        let mut browse: Option<usize> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let entry = &mut app.session.collections[ci].entries[sel];
                let st = &app.strings;
                match section {
                    EditorSection::All => {
                        // The combined view stacks every section, mirroring the
                        // TUI wizard's default "All" tab so the whole request is
                        // visible and editable without switching tabs.
                        const STACK: [EditorSection; 8] = [
                            EditorSection::Params,
                            EditorSection::Headers,
                            EditorSection::Body,
                            EditorSection::Auth,
                            EditorSection::Cookies,
                            EditorSection::Options,
                            EditorSection::Asserts,
                            EditorSection::Captures,
                        ];
                        for (i, sec) in STACK.iter().enumerate() {
                            if i > 0 {
                                ui.add_space(8.0);
                                ui.separator();
                            }
                            ui.label(
                                RichText::new(section_title(*sec, st))
                                    .strong()
                                    .color(theme.text),
                            );
                            if draw_section(*sec, ui, &theme, st, entry, &mut browse, &mut extract)
                            {
                                changed = true;
                            }
                        }
                    }
                    other => {
                        if draw_section(other, ui, &theme, st, entry, &mut browse, &mut extract) {
                            changed = true;
                        }
                    }
                }
            });
        if let Some(field) = browse {
            let seed = app.session.collections[ci].entries[sel]
                .form_fields
                .get(field)
                .and_then(|f| super::filepick::seed_dir(&f.value))
                .or_else(|| {
                    app.session
                        .picker_dir(crate::session::PickerKind::Other)
                        .map(|p| p.to_path_buf())
                });
            app.request_pick(
                super::filepick::PickKind::File {
                    filters: Vec::new(),
                },
                app.strings.gui_browse,
                seed.as_deref(),
                super::menu::PickAction::FormFieldFile {
                    ci,
                    entry: sel,
                    field,
                },
            );
        }
    }

    if let Some(p) = extract {
        // The name is only ever a suggestion, so it is offered in a dialog
        // rather than applied: extracting is two edits that have to agree, and
        // the one thing the user cares about is what the parameter is called.
        let declared = app.session.collections[ci].entries[sel].variable_defaults();
        let name = crate::hurl::suggest_parameter_name(&p.value, &declared);
        app.dialog = Some(super::app::Dialog::ExtractParameter {
            ci,
            entry: sel,
            target: p.target,
            value: p.value,
            range: p.range,
            name,
        });
    }

    app.show_hurl = code_show_hurl;
    if changed {
        let col = &mut app.session.collections[ci];
        col.entries[sel].modified = true;
        col.invalidate_request_json();
    }
    if send {
        app.session.collections[ci].selected_entry = sel;
        app.run_active();
    }
}

/// The in-editor error for a request that carries both a raw body and form
/// fields. Returns whether the user asked to drop the body.
///
/// Deliberately an error rather than the advisory note this used to be: the
/// request still *sends*, and sends wrongly, so a line of quiet text at the top
/// of a section it may not even be looking at is not enough. The action is here
/// because the usual cause is a body that is a single stray space — invisible,
/// and not something anyone would think to go and delete.
fn conflict_notice(ui: &mut egui::Ui, theme: &super::theme::GuiTheme, st: &Strings) -> bool {
    let mut clear = false;
    egui::Frame::new()
        .fill(theme.panel)
        .stroke(egui::Stroke::new(1.0, theme.err))
        .inner_margin(6.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(
                RichText::new(st.gui_body_conflict_headline)
                    .color(theme.err)
                    .strong(),
            );
            ui.label(RichText::new(st.gui_body_conflict_detail).color(theme.text));
            if ui.button(st.gui_body_conflict_clear).clicked() {
                clear = true;
            }
        });
    clear
}

/// Human-readable heading for a section, used above each block in the "All"
/// combined view. Reads the same i18n tab labels as the section tab bar.
fn section_title(section: EditorSection, s: &Strings) -> &'static str {
    match section {
        EditorSection::All => s.tab_all,
        EditorSection::Params => s.gui_sec_params,
        EditorSection::Headers => s.gui_sec_headers,
        EditorSection::Body => s.gui_sec_body,
        EditorSection::Auth => s.gui_sec_auth,
        EditorSection::Cookies => s.gui_sec_cookies,
        EditorSection::Options => s.gui_sec_options,
        EditorSection::Asserts => s.gui_sec_asserts,
        EditorSection::Captures => s.gui_sec_captures,
        EditorSection::Code => s.gui_sec_code,
    }
}

/// Draw one editable request section into `ui`, returning whether the entry
/// changed. Shared by the single-section tabs and the combined "All" view.
/// `All` and `Code` are handled by the caller (they need extra state) and are
/// no-ops here.
/// Which of the request editor's text fields an in-flight "extract to
/// parameter" came from. Recorded rather than acted on immediately: the field
/// is being rendered inside a closure that already borrows the entry, and the
/// name still has to be confirmed in a dialog, so the write-back happens a
/// frame later through [`apply_extract_parameter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExtractTarget {
    Url,
    Body,
    FormField(usize),
    Kv(KvSectionKind, usize),
}

/// Which `[Options]`-style table a [`ExtractTarget::Kv`] row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KvSectionKind {
    Query,
    Header,
    Cookie,
    Option,
}

/// A field the user asked to extract, before the name has been chosen.
pub(super) struct PendingExtract {
    pub target: ExtractTarget,
    /// The text that will be replaced by `{{NAME}}`.
    pub value: String,
    /// Byte range of `value` within the field, or `None` for the whole field.
    /// Only the free-text fields (URL, Body) can be partially extracted; a
    /// table cell holds one value, so there is nothing to narrow.
    pub range: Option<std::ops::Range<usize>>,
}

/// Attach the "Extract to parameter…" context menu to a just-drawn text field.
///
/// `selectable` fields report the user's selection so only the varying part of
/// a URL or body is pulled out; everything else extracts whole. The selection
/// is read from the `TextEdit`'s own state via the response's id, which is the
/// same id the widget stored it under — no explicit `.id()` needed, because we
/// only look *after* the field has been drawn.
fn extract_menu(
    resp: &egui::Response,
    label: &str,
    target: ExtractTarget,
    full: &str,
    selectable: bool,
    out: &mut Option<PendingExtract>,
) {
    resp.context_menu(|ui| {
        if ui.button(label).clicked() {
            let range = selectable
                .then(|| egui::TextEdit::load_state(ui.ctx(), resp.id))
                .flatten()
                .and_then(|st| st.cursor.char_range())
                .map(|r| r.as_sorted_char_range())
                .and_then(|r| char_range_to_bytes(full, r.start.0..r.end.0))
                .filter(|r| !full[r.clone()].trim().is_empty());
            let value = match &range {
                Some(r) => full[r.clone()].to_string(),
                None => full.to_string(),
            };
            if !value.trim().is_empty() {
                *out = Some(PendingExtract {
                    target,
                    value,
                    range,
                });
            }
            ui.close();
        }
    });
}

/// Translate a char range (what egui's cursor speaks) into the byte range the
/// same text is spliced by. An empty or out-of-range selection yields `None`,
/// which the caller reads as "extract the whole field".
/// Turn a `kv_editor`'s "the value on row *i* was right-clicked" into a
/// [`PendingExtract`] naming which of the request's tables that row is in.
fn kv_extract(
    kind: KvSectionKind,
    row: Option<usize>,
    rows: &[KvRow],
    out: &mut Option<PendingExtract>,
) {
    let Some(i) = row else { return };
    let Some(r) = rows.get(i) else { return };
    if r.value.trim().is_empty() {
        return;
    }
    *out = Some(PendingExtract {
        target: ExtractTarget::Kv(kind, i),
        value: r.value.clone(),
        range: None,
    });
}

fn char_range_to_bytes(
    text: &str,
    chars: std::ops::Range<usize>,
) -> Option<std::ops::Range<usize>> {
    if chars.start >= chars.end {
        return None;
    }
    let mut it = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()));
    let start = it.by_ref().nth(chars.start)?;
    let end = it.nth(chars.end - chars.start - 1)?;
    Some(start..end)
}

fn draw_section(
    section: EditorSection,
    ui: &mut egui::Ui,
    theme: &super::theme::GuiTheme,
    st: &Strings,
    entry: &mut HurlEntry,
    // Where a `[Form]` file picker opens when the field is still blank.
    browse: &mut Option<usize>,
    // Where a right-click "Extract to parameter…" lands, to be confirmed in a
    // dialog once the borrow of `entry` has ended.
    extract: &mut Option<PendingExtract>,
) -> bool {
    let mut changed = false;
    let ex_label = st.gui_extract_parameter;
    match section {
        EditorSection::All | EditorSection::Code => {}
        EditorSection::Params => {
            ui.label(RichText::new(st.gui_query_parameters).color(theme.dim));
            let mut hit = None;
            if widgets::kv_editor(
                ui,
                theme,
                st,
                "params",
                &mut entry.queries,
                st.gui_hint_key,
                st.gui_hint_value,
                st.hdr_key,
                st.hdr_value,
                ex_label,
                &mut hit,
                &[],
            ) {
                changed = true;
            }
            kv_extract(KvSectionKind::Query, hit, &entry.queries, extract);
        }
        EditorSection::Headers => {
            let mut hit = None;
            if widgets::kv_editor(
                ui,
                theme,
                st,
                "headers",
                &mut entry.headers,
                st.gui_hint_header,
                st.gui_hint_value,
                st.gui_hint_header,
                st.hdr_value,
                ex_label,
                &mut hit,
                crate::http::COMMON_HEADERS,
            ) {
                changed = true;
            }
            kv_extract(KvSectionKind::Header, hit, &entry.headers, extract);
        }
        EditorSection::Body => {
            // A raw body and form fields are mutually exclusive on the wire
            // (see `HurlEntry::body_form_conflict`), so the section shows one
            // or the other rather than stacking both: a request that posts a
            // form gets the whole panel for its fields instead of half of it,
            // and the choice being a control makes the exclusivity something
            // the user can see rather than something they read about.
            //
            // Which one is showing is remembered per request (keyed by what
            // names it) rather than being global, so switching between a JSON
            // request and a form one doesn't keep flipping the panel; the
            // default follows whatever the request already carries.
            let id = egui::Id::new((
                "body_mode",
                entry.title.as_str(),
                entry.method.as_str(),
                entry.url.as_str(),
            ));
            let default_form = !entry.form_fields.is_empty();
            let mut form_mode =
                ui.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || default_form));
            ui.horizontal(|ui| {
                if super::widgets::selectable(ui, !form_mode, st.gui_body_mode_raw).clicked() {
                    form_mode = false;
                }
                if super::widgets::selectable(ui, form_mode, st.gui_body_mode_form).clicked() {
                    form_mode = true;
                }
            });
            ui.data_mut(|d| d.insert_temp(id, form_mode));
            // Shown in both modes, and regardless of which one the offending
            // content is in: the whole point is that the half you can't see is
            // the half that breaks the request.
            if entry.body_form_conflict() {
                let cleared = conflict_notice(ui, theme, st);
                if cleared {
                    entry.body = None;
                    changed = true;
                }
                ui.add_space(4.0);
            }
            if form_mode {
                if form_editor(
                    ui,
                    theme,
                    st,
                    &mut entry.form_fields,
                    browse,
                    ex_label,
                    extract,
                ) {
                    changed = true;
                }
            } else {
                let mut body = entry.body.take().unwrap_or_default();
                let resp = ui.add(
                    egui::TextEdit::multiline(&mut body)
                        .code_editor()
                        .desired_rows(10)
                        .desired_width(f32::INFINITY)
                        .hint_text(st.gui_raw_body_hint),
                );
                if resp.changed() {
                    changed = true;
                }
                // The body is the one place a *part* of the field is usually
                // what varies, so a selection narrows the extraction.
                extract_menu(&resp, ex_label, ExtractTarget::Body, &body, true, extract);
                entry.body = if body.is_empty() { None } else { Some(body) };
            }
        }
        EditorSection::Auth => {
            let mut enabled = entry.basic_auth.is_some();
            if ui.checkbox(&mut enabled, st.gui_basic_auth).changed() {
                entry.basic_auth = if enabled {
                    Some((String::new(), String::new()))
                } else {
                    None
                };
                changed = true;
            }
            if let Some((user, pass)) = entry.basic_auth.as_mut() {
                egui::Grid::new("auth").num_columns(2).show(ui, |ui| {
                    ui.label(st.gui_username);
                    if ui.text_edit_singleline(user).changed() {
                        changed = true;
                    }
                    ui.end_row();
                    ui.label(st.gui_password);
                    if ui
                        .add(egui::TextEdit::singleline(pass).password(true))
                        .changed()
                    {
                        changed = true;
                    }
                    ui.end_row();
                });
            }
        }
        EditorSection::Cookies => {
            let mut hit = None;
            if widgets::kv_editor(
                ui,
                theme,
                st,
                "cookies",
                &mut entry.cookies,
                st.gui_hint_name,
                st.gui_hint_value,
                st.hdr_name,
                st.hdr_value,
                ex_label,
                &mut hit,
                &[],
            ) {
                changed = true;
            }
            kv_extract(KvSectionKind::Cookie, hit, &entry.cookies, extract);
        }
        EditorSection::Options => {
            ui.label(RichText::new(st.gui_per_request_options).color(theme.dim));
            // A `variable:` row is not an ordinary option: it declares a
            // parameter — a default this request uses on its own, and a name a
            // PaperTrail report can steer it by. That is invisible in a table of
            // key/value rows, so the section says which names it has declared
            // (or, when it has none, how to declare one).
            let params = entry.variable_defaults();
            let note = if params.is_empty() {
                st.gui_options_declare_parameter.to_string()
            } else {
                let names = params
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                crate::i18n::fill(st.gui_options_parameters, &[&names])
            };
            ui.label(RichText::new(note).color(theme.dim));
            let mut hit = None;
            if widgets::kv_editor(
                ui,
                theme,
                st,
                "options",
                &mut entry.options,
                st.gui_hint_option,
                st.gui_hint_value,
                st.hdr_option,
                st.hdr_value,
                ex_label,
                &mut hit,
                &[],
            ) {
                changed = true;
            }
            kv_extract(KvSectionKind::Option, hit, &entry.options, extract);
        }
        EditorSection::Asserts => {
            ui.label(RichText::new(st.gui_response_assertions).color(theme.dim));
            if assert_editor(ui, theme, st, &mut entry.asserts) {
                changed = true;
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(st.gui_expected_status);
                let mut s = entry
                    .expected_status
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                if ui
                    .add(egui::TextEdit::singleline(&mut s).desired_width(60.0))
                    .changed()
                {
                    entry.expected_status = s.trim().parse::<u16>().ok();
                    changed = true;
                }
            });
        }
        EditorSection::Captures => {
            ui.label(RichText::new(st.gui_captures_help).color(theme.dim));
            if widgets::pair_editor(
                ui,
                theme,
                st,
                "captures",
                &mut entry.captures,
                st.gui_hint_name,
                st.gui_hint_query,
                st.hdr_name,
                st.hdr_query,
            ) {
                changed = true;
            }
        }
    }
    changed
}

/// Editable list of `[Asserts]` expression strings.
fn assert_editor(
    ui: &mut egui::Ui,
    theme: &super::theme::GuiTheme,
    s: &Strings,
    asserts: &mut Vec<String>,
) -> bool {
    let mut changed = false;
    let mut remove = None;
    for i in 0..asserts.len() {
        // Pin the remove ✕ to the right and let the value fill everything to its
        // left: an infinite-width field laid out left-to-right would instead
        // claim the whole row and shove the ✕ off the edge (see `kv_editor`).
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button(RichText::new(super::icons::CLOSE).color(theme.err))
                .clicked()
            {
                remove = Some(i);
            }
            let r = widgets::wrapping_field_font(
                ui,
                ui.available_width(),
                &mut asserts[i],
                s.gui_hint_assert,
                theme.text,
                egui::TextStyle::Monospace,
            );
            if r.changed() {
                changed = true;
            }
        });
    }
    if let Some(i) = remove {
        asserts.remove(i);
        changed = true;
    }
    if ui.button(s.gui_add_assert).clicked() {
        asserts.push(String::new());
        changed = true;
    }
    changed
}

/// Editable list of `[Form]`/`[Multipart]` fields.
fn form_editor(
    ui: &mut egui::Ui,
    theme: &super::theme::GuiTheme,
    s: &Strings,
    fields: &mut Vec<FormField>,
    browse: &mut Option<usize>,
    ex_label: &str,
    extract: &mut Option<PendingExtract>,
) -> bool {
    let mut changed = false;
    let mut remove = None;
    // Explicit column widths (not a `ui.horizontal` that lets each cell take
    // what it likes) keep every column aligned across rows: the kind ComboBox
    // is taller than the text cells, so laying each row out freely let the
    // dropdowns and values drift down the further right they sat. The key gets
    // ~40% of the free width — see `widgets::split_key_width`.
    let key_w = super::widgets::split_key_width(ui, 160.0);
    let kind_w = 80.0;
    let check_w = ui.spacing().interact_size.y + 4.0;
    let x_w = super::widgets::remove_width(ui);
    let row_h = ui.spacing().interact_size.y;
    let browse_w = super::widgets::button_width(ui, s.gui_browse);
    super::widgets::table_rows(ui, |ui| {
        for i in 0..fields.len() {
            super::widgets::table_row(ui, |ui| {
                if ui.checkbox(&mut fields[i].enabled, "").changed() {
                    changed = true;
                }
                // Grey a disabled form field's key/value so it reads as inactive
                // (it isn't sent), matching the terminal UI.
                let row_color = if fields[i].enabled {
                    theme.text
                } else {
                    theme.dim
                };
                if super::widgets::sized_key(
                    ui,
                    key_w,
                    &mut fields[i].key,
                    s.gui_hint_field,
                    row_color,
                )
                .changed()
                {
                    changed = true;
                }
                // Kind picker.
                let mut kind = fields[i].kind;
                egui::ComboBox::from_id_salt(("formkind", i))
                    .selected_text(match kind {
                        FormFieldKind::Text => s.gui_kind_text,
                        FormFieldKind::File => s.gui_kind_file,
                        FormFieldKind::Base64File => s.gui_kind_base64,
                    })
                    .width(kind_w)
                    .show_ui(ui, |ui| {
                        for (k, label) in [
                            (FormFieldKind::Text, s.gui_kind_text),
                            (FormFieldKind::File, s.gui_kind_file),
                            (FormFieldKind::Base64File, s.gui_kind_base64),
                        ] {
                            if super::widgets::selectable(ui, kind == k, label).clicked() {
                                kind = k;
                                changed = true;
                            }
                        }
                    });
                fields[i].kind = kind;
                let is_file = matches!(kind, FormFieldKind::File | FormFieldKind::Base64File);
                let hint = match kind {
                    FormFieldKind::Text => s.gui_hint_value,
                    _ => s.gui_hint_file_path,
                };
                // The value fills what the ✕ (and, for a path, Browse) leave.
                let mut spare = ui.available_width() - x_w - 8.0;
                if is_file {
                    spare -= browse_w + 8.0;
                }
                // A form value is often a path or a long token, so it
                // wraps rather than hiding its tail.
                let val = super::widgets::wrapping_field(
                    ui,
                    spare.max(40.0),
                    &mut fields[i].value,
                    hint,
                    row_color,
                );
                if val.changed() {
                    changed = true;
                }
                // A form row's value *is* the path — the kind and content-type
                // live in their own columns — so there is nothing to select
                // within it and the whole cell is what gets extracted.
                extract_menu(
                    &val,
                    ex_label,
                    ExtractTarget::FormField(i),
                    &fields[i].value,
                    false,
                    extract,
                );
                super::widgets::flat_buttons(ui, |ui| {
                    // File/Base64 values are paths — offer a native file picker
                    // (the terminal UI has its in-app browser for the same).
                    if is_file
                        && ui
                            .add_sized([browse_w, row_h], egui::Button::new(s.gui_browse))
                            .clicked()
                    {
                        // Recorded rather than opened: the picker needs `app`,
                        // which the section body has already borrowed mutably.
                        // See `super::filepick`.
                        *browse = Some(i);
                    }
                    if ui
                        .add_sized(
                            [x_w, row_h],
                            egui::Button::new(RichText::new(super::icons::CLOSE).color(theme.err)),
                        )
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
            });
            if fields[i].kind == FormFieldKind::Base64File {
                super::widgets::table_row(ui, |ui| {
                    ui.add_space(check_w);
                    ui.add_sized(
                        [key_w, row_h],
                        egui::Label::new(
                            RichText::new(s.gui_base64_prefix).color(theme.dim).small(),
                        ),
                    );
                    ui.add_space(kind_w);
                    let mut prefix = fields[i].base64_prefix.clone().unwrap_or_default();
                    let w = (ui.available_width() - x_w - 8.0).max(40.0);
                    if super::widgets::wrapping_field(ui, w, &mut prefix, "", theme.text).changed()
                    {
                        fields[i].base64_prefix = if prefix.is_empty() {
                            None
                        } else {
                            Some(prefix)
                        };
                        changed = true;
                    }
                });
            }
        }
    });
    if let Some(i) = remove {
        fields.remove(i);
        changed = true;
    }
    if ui.button(s.gui_add_field).clicked() {
        fields.push(FormField {
            enabled: true,
            ..Default::default()
        });
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;
    use crate::session::Session;
    use eframe::egui::FontId;

    fn session_with_entry() -> Session {
        let mut s = Session::default();
        let mut e = HurlEntry::default();
        e.method = "GET".into();
        e.url = "https://example.com/api".into();
        e.title = "Demo".into();
        s.collections[0].entries = vec![e];
        s.collections[0].selected_entry = 0;
        s
    }

    /// The in-place highlighter is used as a `TextEdit` layouter, so its galley
    /// must lay out *exactly* the buffer's characters — a length change would
    /// desync the cursor. This asserts the produced job text is identical to
    /// the input, `{{ VAR }}` tokens included (i.e. never substituted).
    /// Every string a frame painted, so a section can be checked for by what
    /// the user reads rather than by poking at internal state.
    fn painted(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(t) => out.push(t.galley.text().to_string()),
                egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for c in shapes {
            walk(&c.shape, &mut out);
        }
        out
    }

    /// Draw the Body section for `entry` and report what it painted.
    fn draw_body_section(entry: &mut HurlEntry) -> Vec<String> {
        let th = GuiTheme::from_spec(&crate::theme::default_preset());
        let st = Strings::for_language(&Language::English);
        let ctx = egui::Context::default();
        th.apply(&ctx);
        let mut browse = None;
        let mut out = Vec::new();
        // Twice: the first pass is what sizes the fields.
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            };
            let full = ctx.run_ui(input, |ui| {
                draw_section(
                    EditorSection::Body,
                    ui,
                    &th,
                    &st,
                    entry,
                    &mut browse,
                    &mut None,
                );
            });
            out = painted(&full.shapes);
        }
        out
    }

    /// A raw body and form fields are mutually exclusive on the wire, so the
    /// section shows one or the other — and opens on whichever the request
    /// already uses, rather than always giving half the panel to an empty body
    /// box.
    #[test]
    fn the_body_section_opens_on_the_form_when_the_request_posts_one() {
        let mut entry = HurlEntry {
            form_fields: vec![crate::hurl::FormField {
                key: "grant_type".into(),
                value: "client_credentials".into(),
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let shown = draw_body_section(&mut entry);
        assert!(
            shown.iter().any(|t| t == "grant_type"),
            "the form field is shown: {shown:?}"
        );
        let st = Strings::for_language(&Language::English);
        assert!(
            !shown.iter().any(|t| t == st.gui_raw_body_hint),
            "the empty body box is not taking up the panel: {shown:?}"
        );
    }

    /// …and on the body when there are no fields, so a JSON request is not
    /// made to hunt for its own editor.
    #[test]
    fn the_body_section_opens_on_the_body_when_there_are_no_form_fields() {
        let mut entry = HurlEntry::default();
        let shown = draw_body_section(&mut entry);
        let st = Strings::for_language(&Language::English);
        assert!(
            shown.iter().any(|t| t == st.gui_raw_body_hint),
            "the raw body editor is shown: {shown:?}"
        );
    }

    /// The half that is hidden is exactly the half that breaks the request, so
    /// carrying both has to be said out loud whichever one is on screen.
    #[test]
    fn carrying_both_a_body_and_form_fields_is_reported_in_the_section() {
        let mut entry = HurlEntry {
            body: Some(" ".into()),
            form_fields: vec![crate::hurl::FormField {
                key: "grant_type".into(),
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let shown = draw_body_section(&mut entry);
        let st = Strings::for_language(&Language::English);
        assert!(
            shown.iter().any(|t| t == st.gui_body_conflict_headline),
            "the conflict is named: {shown:?}"
        );
        assert!(
            shown.iter().any(|t| t == st.gui_body_conflict_clear),
            "and the fix is offered: {shown:?}"
        );
    }

    #[test]
    fn editable_highlighter_preserves_the_buffer_text_verbatim() {
        let th = GuiTheme::from_spec(&Session::default().active_theme_spec());
        let vars = HashMap::new();
        let shadowed = HashSet::new();
        let mut seen = SubstSeen::default();
        for text in [
            "GET https://x/{{ host }}/api\nAuthorization: {{ token }}",
            "no placeholders here",
            "trailing {{ unclosed",
            "{{a}}{{b}} back to back",
        ] {
            let job = highlight_code_editable(
                text,
                &vars,
                &shadowed,
                &th,
                FontId::monospace(12.0),
                &mut seen,
            );
            assert_eq!(job.text, text, "layouter must not alter the buffer text");
        }
    }

    #[test]
    fn editing_the_hurl_buffer_roundtrips_a_new_header_into_the_entry() {
        let strings = Strings::for_language(&Language::English);
        let mut session = session_with_entry();
        let mut code = super::super::app::CodeEdit::default();

        // The same request, plus one extra header, serialised back to Hurl.
        let mut edited = session.collections[0].entries[0].clone();
        edited.headers.push(KvRow::toggled("X-Test", "hello", true));
        let text = edited.to_hurl();

        let changed = apply_code_edit(&mut session, &mut code, &strings, 0, 0, true, &text);
        assert!(changed, "a valid edit should report a change");
        assert!(code.error.is_none(), "a valid edit clears the error");
        let hdrs = &session.collections[0].entries[0].headers;
        assert!(
            hdrs.iter().any(|r| r.key == "X-Test" && r.value == "hello"),
            "expected the new header to be applied, got {hdrs:?}"
        );
    }

    #[test]
    fn invalid_hurl_keeps_the_entry_and_records_an_error() {
        let strings = Strings::for_language(&Language::English);
        let mut session = session_with_entry();
        let before = session.collections[0].entries[0].clone();
        let mut code = super::super::app::CodeEdit::default();

        // Lowercase "not" is not a valid HTTP method → zero parsed entries.
        let changed = apply_code_edit(
            &mut session,
            &mut code,
            &strings,
            0,
            0,
            true,
            "not a request",
        );
        assert!(!changed, "an unparseable edit must not report a change");
        assert!(code.error.is_some(), "an unparseable edit records an error");
        let entry = &session.collections[0].entries[0];
        assert_eq!(entry.method, before.method);
        assert_eq!(entry.url, before.url);
        assert_eq!(entry.headers, before.headers);
    }

    #[test]
    fn editing_the_json_buffer_roundtrips_the_method_into_the_entry() {
        let strings = Strings::for_language(&Language::English);
        let mut session = session_with_entry();
        let mut code = super::super::app::CodeEdit::default();

        let mut edited = session.collections[0].entries[0].clone();
        edited.method = "POST".into();
        let text = build_request_json(&edited);

        let changed = apply_code_edit(&mut session, &mut code, &strings, 0, 0, false, &text);
        assert!(changed, "a valid JSON edit should report a change");
        assert!(code.error.is_none());
        assert_eq!(session.collections[0].entries[0].method, "POST");
    }

    #[test]
    fn invalid_json_keeps_the_entry_and_records_an_error() {
        let strings = Strings::for_language(&Language::English);
        let mut session = session_with_entry();
        let before_method = session.collections[0].entries[0].method.clone();
        let mut code = super::super::app::CodeEdit::default();

        let changed = apply_code_edit(&mut session, &mut code, &strings, 0, 0, false, "{ not json");
        assert!(!changed, "malformed JSON must not report a change");
        assert!(code.error.is_some(), "malformed JSON records an error");
        assert_eq!(session.collections[0].entries[0].method, before_method);
    }
}

#[cfg(test)]
mod highlight_cache_tests {
    use super::*;
    use crate::gui::theme::GuiTheme;
    use crate::request::{SubstInfo, SubstKind};

    fn vars() -> HashMap<String, SubstInfo> {
        [
            (
                "BASE",
                SubstInfo {
                    shown: Some("https://x".into()),
                    kind: SubstKind::Literal,
                },
            ),
            (
                "TOKEN",
                SubstInfo {
                    shown: None,
                    kind: SubstKind::Pending,
                },
            ),
            (
                "SECRET",
                SubstInfo {
                    shown: Some("s".into()),
                    kind: SubstKind::Loaded,
                },
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }

    /// The legend used to be built by running the highlighter over the whole
    /// buffer and throwing the coloured job away. The cheap scan that replaced
    /// it has to report exactly what that pass did, or the legend would start
    /// listing the wrong statuses.
    #[test]
    fn the_legend_scan_agrees_with_the_highlighter_it_replaced() {
        let vars = vars();
        let shadowed: HashSet<String> = ["SECRET".to_string()].into_iter().collect();
        for text in [
            "",
            "GET {{ BASE }}/a",
            "GET {{BASE}}/a\nAuth: {{ TOKEN }}\nX: {{ SECRET }}\n",
            "{{ UNKNOWN }} and an unclosed {{ one",
            "no placeholders at all",
        ] {
            let mut from_highlighter = SubstSeen::default();
            let _ = highlight_code_editable(
                text,
                &vars,
                &shadowed,
                &GuiTheme::from_spec(&crate::theme::default_preset()),
                FontId::monospace(12.0),
                &mut from_highlighter,
            );
            let scanned = substitution_statuses(text, &vars, &shadowed);
            assert_eq!(
                (
                    scanned.loaded,
                    scanned.literal,
                    scanned.pending,
                    scanned.failed,
                    scanned.shadowed
                ),
                (
                    from_highlighter.loaded,
                    from_highlighter.literal,
                    from_highlighter.pending,
                    from_highlighter.failed,
                    from_highlighter.shadowed
                ),
                "disagreed on {text:?}"
            );
        }
    }

    /// The cache keys on a variable's name and kind but not its value, which is
    /// only safe because the editable highlighter leaves the `{{ VAR }}` token
    /// in the buffer rather than substituting into it. Pin that, so the key
    /// would have to be widened if the Code view ever started substituting.
    #[test]
    fn the_editable_highlighter_shows_the_placeholder_not_the_value() {
        let th = GuiTheme::from_spec(&crate::theme::default_preset());
        let ctx = egui::Context::default();
        let font = FontId::monospace(12.0);
        let shadowed = HashSet::new();
        let text = "GET {{ BASE }}/a";

        let render = |shown: &str| {
            let vars: HashMap<String, SubstInfo> = [(
                "BASE".to_string(),
                SubstInfo {
                    shown: Some(shown.to_string()),
                    kind: SubstKind::Literal,
                },
            )]
            .into_iter()
            .collect();
            let mut out = String::new();
            for _ in 0..2 {
                let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                    out = cached_code_job(ui, text, &vars, &shadowed, &th, font.clone()).text;
                });
            }
            out
        };

        assert_eq!(
            render("https://staging"),
            text,
            "the buffer keeps its placeholders; only their colour comes from the value"
        );
        assert_eq!(render("https://staging"), render("https://prod"));
    }

    /// The cached job must be the one the highlighter would have built, and must
    /// follow an edit rather than holding on to the previous buffer.
    #[test]
    fn the_cached_job_matches_a_freshly_built_one_and_follows_an_edit() {
        let vars = vars();
        let shadowed = HashSet::new();
        let th = GuiTheme::from_spec(&crate::theme::default_preset());
        let ctx = egui::Context::default();
        let font = FontId::monospace(12.0);

        let sections = |text: &str| {
            let mut got = Vec::new();
            // Twice, so the second pass is the one served from the cache.
            for _ in 0..2 {
                let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                    let job = cached_code_job(ui, text, &vars, &shadowed, &th, font.clone());
                    got = job
                        .sections
                        .iter()
                        .map(|s| (s.byte_range.clone(), s.format.color))
                        .collect();
                });
            }
            got
        };

        let fresh = |text: &str| {
            let mut ignored = SubstSeen::default();
            highlight_code_editable(text, &vars, &shadowed, &th, font.clone(), &mut ignored)
                .sections
                .iter()
                .map(|s| (s.byte_range.clone(), s.format.color))
                .collect::<Vec<_>>()
        };

        let one = "GET {{ BASE }}/a";
        assert_eq!(sections(one), fresh(one), "same colouring, cached or not");

        let two = "GET {{ TOKEN }}/a";
        assert_eq!(
            sections(two),
            fresh(two),
            "an edit re-colours rather than serving the previous buffer"
        );
        assert_ne!(
            sections(one),
            sections(two),
            "and the two really do colour differently"
        );
    }
}

/// Commit an "extract to parameter": replace `range` (or the whole field) in
/// `target` with `{{name}}`, and declare `name` as a request parameter unless
/// the request already declares it with this very value — in which case the two
/// fields simply come to share one parameter, which is the point.
///
/// Every index is re-checked rather than trusted, for the same reason as
/// [`apply_picked_form_file`]: the dialog is modal to the user, not to the
/// program, and the row it named may be gone by the time it is answered.
pub(super) fn apply_extract_parameter(
    app: &mut GuiApp,
    ci: usize,
    entry: usize,
    target: ExtractTarget,
    range: Option<std::ops::Range<usize>>,
    value: &str,
    name: &str,
) {
    let name = name.trim();
    let Some(col) = app.session.collections.get_mut(ci) else {
        return;
    };
    let Some(e) = col.entries.get_mut(entry) else {
        return;
    };
    if crate::hurl::check_parameter_name(name, value, &e.variable_defaults()).is_some() {
        return;
    }
    let already = e.declares_variable(name);
    let placeholder = format!("{{{{{name}}}}}");
    // Splicing the recorded range only if the text there is still what was
    // extracted: the field is editable while the dialog is up, and replacing
    // whatever now occupies those bytes would be a silent corruption.
    let replace = |field: &mut String| -> bool {
        match &range {
            Some(r) if field.get(r.clone()) == Some(value) => {
                field.replace_range(r.clone(), &placeholder);
                true
            }
            Some(_) => false,
            None if field == value => {
                *field = placeholder.clone();
                true
            }
            None => false,
        }
    };
    let applied = match target {
        ExtractTarget::Url => replace(&mut e.url),
        ExtractTarget::Body => match e.body.as_mut() {
            Some(b) => replace(b),
            None => false,
        },
        ExtractTarget::FormField(i) => match e.form_fields.get_mut(i) {
            Some(f) => replace(&mut f.value),
            None => false,
        },
        ExtractTarget::Kv(kind, i) => {
            let rows = match kind {
                KvSectionKind::Query => &mut e.queries,
                KvSectionKind::Header => &mut e.headers,
                KvSectionKind::Cookie => &mut e.cookies,
                KvSectionKind::Option => &mut e.options,
            };
            match rows.get_mut(i) {
                Some(r) => replace(&mut r.value),
                None => false,
            }
        }
    };
    if !applied {
        return;
    }
    if !already {
        e.options.push(KvRow::toggled(
            "variable".to_string(),
            format!("{name}={value}"),
            true,
        ));
    }
    e.modified = true;
    col.invalidate_request_json();
}

/// Write back the file a Form/Multipart value's Browse dialog returned.
///
/// Every index is re-checked rather than trusted: the user can switch request,
/// close the collection or delete the row while the dialog is up, and writing a
/// path into whatever now sits at that index would be worse than dropping it.
pub(super) fn apply_picked_form_file(
    app: &mut GuiApp,
    ci: usize,
    entry: usize,
    field: usize,
    picked: Option<std::path::PathBuf>,
) {
    let Some(path) = picked else {
        return; // cancelled
    };
    let Some(col) = app.session.collections.get_mut(ci) else {
        return;
    };
    let Some(e) = col.entries.get_mut(entry) else {
        return;
    };
    let Some(f) = e.form_fields.get_mut(field) else {
        return;
    };
    f.value = path.to_string_lossy().into_owned();
    e.modified = true;
    col.invalidate_request_json();
}

#[cfg(test)]
mod pick_tests {
    use super::*;

    fn app_with_form_field(value: &str) -> GuiApp {
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let entry = crate::hurl::HurlEntry {
            title: "A".into(),
            url: "http://127.0.0.1:1/".into(),
            form_fields: vec![crate::hurl::FormField {
                key: "f".into(),
                kind: crate::hurl::FormFieldKind::File,
                value: value.into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        session.collections.push(crate::collection::Collection::new(
            "api".into(),
            vec![entry],
        ));
        GuiApp::for_test(session)
    }

    #[test]
    fn a_picked_form_file_lands_in_its_field_and_marks_it_modified() {
        let mut app = app_with_form_field("");
        apply_picked_form_file(&mut app, 0, 0, 0, Some("/tmp/face.jpg".into()));
        assert_eq!(
            app.session.collections[0].entries[0].form_fields[0].value,
            "/tmp/face.jpg"
        );
        assert!(app.session.collections[0].entries[0].modified);
    }

    /// Cancelling must leave the value the user already typed alone -- backing
    /// out of a picker is not a request to clear the field.
    #[test]
    fn cancelling_leaves_the_field_untouched() {
        let mut app = app_with_form_field("kept.jpg");
        apply_picked_form_file(&mut app, 0, 0, 0, None);
        assert_eq!(
            app.session.collections[0].entries[0].form_fields[0].value,
            "kept.jpg"
        );
        assert!(!app.session.collections[0].entries[0].modified);
    }

    /// The dialog outlives its context: by the time a path arrives the row it
    /// was opened for may be gone, and the path must be dropped rather than
    /// written into whatever now sits at that index.
    #[test]
    fn a_path_for_a_row_that_no_longer_exists_is_dropped() {
        let mut app = app_with_form_field("");
        apply_picked_form_file(&mut app, 0, 0, 7, Some("/tmp/x.jpg".into()));
        apply_picked_form_file(&mut app, 0, 9, 0, Some("/tmp/x.jpg".into()));
        apply_picked_form_file(&mut app, 5, 0, 0, Some("/tmp/x.jpg".into()));
        assert_eq!(
            app.session.collections[0].entries[0].form_fields[0].value,
            ""
        );
        assert!(!app.session.collections[0].entries[0].modified);
    }
}

#[cfg(test)]
mod extract_tests {
    use super::*;
    use crate::hurl::{FormField, FormFieldKind, HurlEntry, KvRow};

    fn app_with(entry: HurlEntry) -> GuiApp {
        let mut session = crate::session::Session::default();
        session.collections.clear();
        session.collections.push(crate::collection::Collection::new(
            "api".into(),
            vec![entry],
        ));
        GuiApp::for_test(session)
    }

    fn form_entry(value: &str) -> HurlEntry {
        HurlEntry {
            title: "upload".into(),
            url: "http://h/upload".into(),
            form_fields: vec![FormField {
                key: "document".into(),
                kind: FormFieldKind::File,
                value: value.into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// The whole point: one action does both halves. Done by hand they are two
    /// edits that have to agree, and the field silently sends nothing useful if
    /// they don't.
    #[test]
    fn extracting_a_form_value_replaces_it_and_declares_the_parameter() {
        let mut app = app_with(form_entry("./samples/example.pdf"));
        apply_extract_parameter(
            &mut app,
            0,
            0,
            ExtractTarget::FormField(0),
            None,
            "./samples/example.pdf",
            "FILE",
        );
        let e = &app.session.collections[0].entries[0];
        assert_eq!(e.form_fields[0].value, "{{FILE}}");
        assert_eq!(
            e.variable_defaults(),
            vec![("FILE".to_string(), "./samples/example.pdf".to_string())]
        );
        assert!(e.modified, "the request now differs from the file on disk");
    }

    /// A selection in a URL pulls out only the part that varies — the rest of
    /// the URL is still the request's own.
    #[test]
    fn extracting_a_url_selection_leaves_the_rest_of_the_url_alone() {
        let mut app = app_with(HurlEntry {
            url: "http://h/orders/12345/items".into(),
            ..form_entry("x")
        });
        let range = "http://h/orders/".len().."http://h/orders/12345".len();
        apply_extract_parameter(
            &mut app,
            0,
            0,
            ExtractTarget::Url,
            Some(range),
            "12345",
            "ORDER",
        );
        let e = &app.session.collections[0].entries[0];
        assert_eq!(e.url, "http://h/orders/{{ORDER}}/items");
        assert_eq!(
            e.variable_defaults(),
            vec![("ORDER".to_string(), "12345".to_string())]
        );
    }

    /// Two fields carrying the same file should come to read the same
    /// parameter, not declare it twice and then drift apart.
    #[test]
    fn extracting_the_same_value_twice_reuses_the_one_declaration() {
        let mut entry = form_entry("./samples/example.pdf");
        entry
            .headers
            .push(KvRow::toggled("X-Source", "./samples/example.pdf", true));
        let mut app = app_with(entry);
        for target in [
            ExtractTarget::FormField(0),
            ExtractTarget::Kv(KvSectionKind::Header, 0),
        ] {
            apply_extract_parameter(
                &mut app,
                0,
                0,
                target,
                None,
                "./samples/example.pdf",
                "FILE",
            );
        }
        let e = &app.session.collections[0].entries[0];
        assert_eq!(e.form_fields[0].value, "{{FILE}}");
        assert_eq!(e.headers[0].value, "{{FILE}}");
        assert_eq!(e.variable_defaults().len(), 1, "one declaration, shared");
    }

    /// Reusing a name that already means something else would silently repoint
    /// the field at the other one's default — refused, not merged.
    #[test]
    fn a_name_that_already_means_something_else_is_refused() {
        let mut entry = form_entry("./samples/other.pdf");
        entry.options.push(KvRow::toggled(
            "variable",
            "FILE=./samples/example.pdf",
            true,
        ));
        let mut app = app_with(entry);
        apply_extract_parameter(
            &mut app,
            0,
            0,
            ExtractTarget::FormField(0),
            None,
            "./samples/other.pdf",
            "FILE",
        );
        let e = &app.session.collections[0].entries[0];
        assert_eq!(
            e.form_fields[0].value, "./samples/other.pdf",
            "the field is untouched"
        );
        assert_eq!(e.variable_defaults().len(), 1, "and nothing was declared");
    }

    /// The dialog is modal to the user, not to the program: the field can be
    /// edited, or the row deleted, while it is up. Writing `{{NAME}}` over
    /// whatever now sits there would be a silent corruption.
    #[test]
    fn a_field_that_changed_under_the_dialog_is_left_alone() {
        let mut app = app_with(form_entry("./samples/changed.pdf"));
        apply_extract_parameter(
            &mut app,
            0,
            0,
            ExtractTarget::FormField(0),
            None,
            "./samples/example.pdf",
            "FILE",
        );
        let e = &app.session.collections[0].entries[0];
        assert_eq!(e.form_fields[0].value, "./samples/changed.pdf");
        assert!(e.variable_defaults().is_empty(), "and nothing was declared");
        assert!(!e.modified);
    }

    /// A row index that no longer exists is dropped rather than applied to
    /// whatever slid into its place.
    #[test]
    fn a_row_that_no_longer_exists_is_dropped() {
        let mut app = app_with(form_entry("./samples/example.pdf"));
        apply_extract_parameter(
            &mut app,
            0,
            0,
            ExtractTarget::FormField(7),
            None,
            "./samples/example.pdf",
            "FILE",
        );
        let e = &app.session.collections[0].entries[0];
        assert_eq!(e.form_fields[0].value, "./samples/example.pdf");
        assert!(e.variable_defaults().is_empty());
    }

    /// egui's cursor speaks characters; the text is spliced by bytes. A
    /// multi-byte character before the selection used to shift the splice.
    #[test]
    fn a_char_range_over_multibyte_text_maps_to_the_right_bytes() {
        let text = "héllo wörld";
        let chars: Vec<char> = text.chars().collect();
        let start = 6; // 'w'
        let end = 11;
        let r = char_range_to_bytes(text, start..end).expect("a non-empty range");
        assert_eq!(&text[r], chars[start..end].iter().collect::<String>());
        assert_eq!(
            char_range_to_bytes(text, 3..3),
            None,
            "an empty range is no selection"
        );
    }
}
