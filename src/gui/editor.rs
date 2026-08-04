//! Centre-top panel: the request editor. A Postman-style method/URL bar plus
//! section tabs (Params, Headers, Body, Auth, Cookies, Options, Asserts,
//! Captures, Code) editing the selected [`HurlEntry`] in place.

use std::collections::{HashMap, HashSet};

use eframe::egui::text::LayoutJob;
use eframe::egui::{self, Color32, FontId, RichText, TextFormat};

use crate::hurl::{FormField, FormFieldKind, HurlEntry};
use crate::i18n::Strings;
use crate::request::{SubstInfo, SubstKind, build_request_json};

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
    shadowed: bool,
}

impl SubstSeen {
    fn mark(&mut self, kind: SubstKind) {
        match kind {
            SubstKind::Loaded => self.loaded = true,
            SubstKind::Literal => self.literal = true,
            SubstKind::Pending => self.pending = true,
            SubstKind::Failed => self.failed = true,
        }
    }

    fn any(&self) -> bool {
        self.loaded || self.literal || self.pending || self.failed
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
    }
}

/// Build a colour-coded [`LayoutJob`] for a request preview: each known
/// `{{ VAR }}` is substituted with its resolved value (coloured by status) or,
/// when unavailable, kept as the placeholder in its status colour; unknown
/// placeholders keep the default colour. A shadowed key gets a leading
/// [`SHADOW_ICON`]. `seen` records which statuses appeared, for the legend.
fn highlight_code(
    text: &str,
    vars: &HashMap<String, SubstInfo>,
    shadowed: &HashSet<String>,
    th: &GuiTheme,
    seen: &mut SubstSeen,
) -> LayoutJob {
    let font = FontId::monospace(12.0);
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
        match vars.get(inner) {
            Some(info) => {
                seen.mark(info.kind);
                let color = subst_color(info.kind, th);
                match &info.shown {
                    Some(val) => {
                        if shadowed.contains(inner) {
                            job.append(SHADOW_ICON, 0.0, fmt(th.pending));
                            seen.shadowed = true;
                        }
                        job.append(val, 0.0, fmt(color));
                    }
                    None => job.append(&rest[open..end], 0.0, fmt(color)),
                }
            }
            None => job.append(&rest[open..end], 0.0, fmt(th.text)),
        }
        rest = &rest[end..];
    }
    if !rest.is_empty() {
        job.append(rest, 0.0, fmt(th.text));
    }
    job
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
    let section = app.editor_section;
    // Local copy of the Code-view toggle; written back after the borrow of the
    // selected entry ends (egui closures can't borrow `app` again mid-frame).
    let mut code_show_hurl = app.show_hurl;

    // ── Method / URL / Send bar ───────────────────────────────────────────
    let send_label = format!("{} {}", app.strings.gui_send, super::icons::PLAY);
    {
        let entry = &mut app.session.collections[ci].entries[sel];
        ui.horizontal(|ui| {
            if widgets::method_combo(ui, "method", &mut entry.method) {
                changed = true;
            }
            let send_w = 92.0;
            let url = ui.add_sized(
                [ui.available_width() - send_w, 24.0],
                egui::TextEdit::singleline(&mut entry.url)
                    .hint_text("https://api.example.com/path")
                    .font(egui::TextStyle::Monospace),
            );
            if url.changed() {
                changed = true;
            }
            let btn = ui.add_sized(
                [80.0, 24.0],
                egui::Button::new(RichText::new(send_label).strong().color(theme.select_fg))
                    .fill(theme.accent),
            );
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
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let entry = &mut app.session.collections[ci].entries[sel];
            let st = &app.strings;
            match section {
                EditorSection::Params => {
                    ui.label(RichText::new(st.gui_query_parameters).color(theme.dim));
                    if widgets::kv_editor(
                        ui,
                        &theme,
                        st,
                        "params",
                        &mut entry.queries,
                        st.gui_hint_key,
                        st.gui_hint_value,
                    ) {
                        changed = true;
                    }
                }
                EditorSection::Headers => {
                    if widgets::kv_editor(
                        ui,
                        &theme,
                        st,
                        "headers",
                        &mut entry.headers,
                        st.gui_hint_header,
                        st.gui_hint_value,
                    ) {
                        changed = true;
                    }
                }
                EditorSection::Body => {
                    if !entry.form_fields.is_empty() {
                        ui.colored_label(theme.pending, st.gui_form_mutually_exclusive);
                    }
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
                    entry.body = if body.is_empty() { None } else { Some(body) };

                    ui.add_space(8.0);
                    ui.separator();
                    ui.label(RichText::new(st.gui_form_fields).color(theme.dim));
                    if form_editor(ui, &theme, st, &mut entry.form_fields) {
                        changed = true;
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
                    if widgets::kv_editor(
                        ui,
                        &theme,
                        st,
                        "cookies",
                        &mut entry.cookies,
                        st.gui_hint_name,
                        st.gui_hint_value,
                    ) {
                        changed = true;
                    }
                }
                EditorSection::Options => {
                    ui.label(RichText::new(st.gui_per_request_options).color(theme.dim));
                    if widgets::kv_editor(
                        ui,
                        &theme,
                        st,
                        "options",
                        &mut entry.options,
                        st.gui_hint_option,
                        st.gui_hint_value,
                    ) {
                        changed = true;
                    }
                }
                EditorSection::Asserts => {
                    ui.label(RichText::new(st.gui_response_assertions).color(theme.dim));
                    if assert_editor(ui, &theme, st, &mut entry.asserts) {
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
                        &theme,
                        st,
                        "captures",
                        &mut entry.captures,
                        st.gui_hint_name,
                        st.gui_hint_query,
                    ) {
                        changed = true;
                    }
                }
                EditorSection::Code => {
                    ui.horizontal(|ui| {
                        if super::widgets::selectable(ui, !code_show_hurl, "JSON").clicked() {
                            code_show_hurl = false;
                        }
                        if super::widgets::selectable(ui, code_show_hurl, "Hurl").clicked() {
                            code_show_hurl = true;
                        }
                    });
                    let code = if code_show_hurl {
                        entry.to_hurl()
                    } else {
                        build_request_json(entry)
                    };
                    ui.add_space(4.0);
                    // Substitute + colour-code every `{{ VAR }}` (matching the
                    // terminal UI's request preview); the shown text is what a
                    // copy/selection yields, not the raw template.
                    let mut seen = SubstSeen::default();
                    let job = highlight_code(&code, &subst_vars, &shadowed, &theme, &mut seen);
                    egui::Frame::new()
                        .fill(theme.sunken())
                        .inner_margin(6.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(job)
                                    .selectable(true)
                                    .wrap_mode(egui::TextWrapMode::Extend),
                            );
                        });
                    ui.add_space(4.0);
                    subst_legend(ui, &seen, &theme, st);
                }
            }
        });

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
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut asserts[i])
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("jsonpath \"$.status\" == \"ok\""),
            );
            if r.changed() {
                changed = true;
            }
            if ui
                .button(RichText::new(super::icons::CLOSE).color(theme.err))
                .clicked()
            {
                remove = Some(i);
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
) -> bool {
    let mut changed = false;
    let mut remove = None;
    for i in 0..fields.len() {
        ui.horizontal(|ui| {
            if ui.checkbox(&mut fields[i].enabled, "").changed() {
                changed = true;
            }
            if ui
                .add(
                    egui::TextEdit::singleline(&mut fields[i].key)
                        .desired_width(120.0)
                        .hint_text(s.gui_hint_field),
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
                .width(80.0)
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
            let hint = match kind {
                FormFieldKind::Text => s.gui_hint_value,
                _ => "/path/to/file",
            };
            if ui
                .add(
                    egui::TextEdit::singleline(&mut fields[i].value)
                        .desired_width(f32::INFINITY)
                        .hint_text(hint),
                )
                .changed()
            {
                changed = true;
            }
            if ui
                .button(RichText::new(super::icons::CLOSE).color(theme.err))
                .clicked()
            {
                remove = Some(i);
            }
        });
        if fields[i].kind == FormFieldKind::Base64File {
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                ui.label(RichText::new(s.gui_base64_prefix).color(theme.dim).small());
                let mut prefix = fields[i].base64_prefix.clone().unwrap_or_default();
                if ui
                    .add(egui::TextEdit::singleline(&mut prefix).desired_width(240.0))
                    .changed()
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
