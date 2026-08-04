//! The top menu bar and the modal dialogs (open/save, rename, prompt) plus the
//! theme editor. Everything the terminal UI reaches through its `:`-menu and
//! wizards, driven through the shared [`crate::session::Session`].

use std::path::PathBuf;

use eframe::egui::{self, Align2, RichText};

use crate::i18n::{Language, Strings};
use crate::request::RequestView;
use crate::theme::{THEME_COLOR_COUNT, ThemeSpec, is_builtin};

use super::app::{Dialog, GuiApp, OpenKind, PromptKind, RenameTarget, SaveKind};

/// In-progress theme edit: the spec being edited plus the name it started with
/// (so applying can replace an existing custom theme rather than duplicate it).
pub struct ThemeEditState {
    pub spec: ThemeSpec,
    pub original_name: String,
}

// ── Menu bar ────────────────────────────────────────────────────────────────

pub fn menu_bar(app: &mut GuiApp, ui: &mut egui::Ui) {
    egui::MenuBar::new().ui(ui, |ui| {
        file_menu(app, ui);
        settings_menu(app, ui);
        view_menu(app, ui);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button(
                    RichText::new(format!("{} {}", app.strings.gui_send, super::icons::PLAY))
                        .color(app.theme.accent),
                )
                .on_hover_text(app.strings.gui_send_tooltip)
                .clicked()
            {
                app.run_active();
            }
        });
    });
}

fn file_menu(app: &mut GuiApp, ui: &mut egui::Ui) {
    ui.menu_button(app.strings.gui_menu_file, |ui| {
        if ui.button(app.strings.gui_new_collection_ellipsis).clicked() {
            app.dialog = Some(Dialog::Prompt {
                kind: PromptKind::NewCollectionName,
                text: String::new(),
            });
            ui.close();
        }
        if ui
            .button(app.strings.gui_open_collection_ellipsis)
            .clicked()
        {
            app.dialog = Some(Dialog::OpenFile {
                kind: OpenKind::Collection,
                path: String::new(),
                error: None,
            });
            ui.close();
        }
        if ui.button(app.strings.gui_import_postman).clicked() {
            // Same loader — it auto-detects Postman JSON vs. Hurl.
            app.dialog = Some(Dialog::OpenFile {
                kind: OpenKind::Collection,
                path: String::new(),
                error: None,
            });
            ui.close();
        }
        if ui
            .button(app.strings.gui_open_environment_ellipsis)
            .clicked()
        {
            app.dialog = Some(Dialog::OpenFile {
                kind: OpenKind::Environment,
                path: String::new(),
                error: None,
            });
            ui.close();
        }
        if ui.button(app.strings.gui_open_workspace_ellipsis).clicked() {
            app.dialog = Some(Dialog::OpenFile {
                kind: OpenKind::Workspace,
                path: String::new(),
                error: None,
            });
            ui.close();
        }
        ui.separator();
        if ui
            .button(app.strings.gui_save_collection_ellipsis)
            .clicked()
        {
            let ci = app.active_ci();
            let path = app.session.collections[ci]
                .path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            app.dialog = Some(Dialog::SaveFile {
                kind: SaveKind::Collection,
                path,
                error: None,
            });
            ui.close();
        }
        if ui.button(app.strings.gui_save_response_ellipsis).clicked() {
            app.dialog = Some(Dialog::SaveFile {
                kind: SaveKind::Response,
                path: String::new(),
                error: None,
            });
            ui.close();
        }
        ui.separator();
        if ui.button(app.strings.gui_load_from_git).clicked() {
            app.remote.open_load();
            ui.close();
        }
        if ui.button(app.strings.gui_save_collection_git).clicked() {
            app.remote.open_save_collection(app.active_ci());
            ui.close();
        }
        ui.separator();
        if ui.button(app.strings.gui_set_base_url).clicked() {
            app.dialog = Some(Dialog::Prompt {
                kind: PromptKind::BaseUrl,
                text: app.session.vars.base_url.clone(),
            });
            ui.close();
        }
        ui.separator();
        if ui.button(app.strings.gui_close_tab).clicked() {
            app.session.close_tab(app.active_ci());
            ui.close();
        }
        if ui.button(app.strings.gui_quit).clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    });
}

fn settings_menu(app: &mut GuiApp, ui: &mut egui::Ui) {
    ui.menu_button(app.strings.gui_menu_settings, |ui| {
        ui.menu_button(app.strings.gui_language, |ui| {
            for (lang, label) in [
                (Language::English, "English"),
                (Language::French, "Français"),
                (Language::Danish, "Dansk"),
            ] {
                if ui.radio(app.session.language == lang, label).clicked() {
                    app.session.language = lang;
                    app.session.save();
                    ui.close();
                }
            }
        });
        ui.menu_button(app.strings.gui_theme_menu, |ui| {
            let active = app.session.active_theme.clone();
            for spec in app.session.all_themes() {
                let selected = active.as_deref() == Some(spec.name.as_str());
                if ui.radio(selected, &spec.name).clicked() {
                    app.session.active_theme = Some(spec.name.clone());
                    app.session.save();
                    ui.close();
                }
            }
            ui.separator();
            if ui.button(app.strings.gui_follow_language).clicked() {
                app.session.active_theme = None;
                app.session.save();
                ui.close();
            }
            ui.separator();
            if ui.button(app.strings.gui_new_custom_theme).clicked() {
                let mut spec = app.session.active_theme_spec();
                spec.name = unique_theme_name(app, app.strings.gui_custom);
                app.dialog = Some(Dialog::Theme(Box::new(ThemeEditState {
                    original_name: spec.name.clone(),
                    spec,
                })));
                ui.close();
            }
            let editable = active.as_ref().map(|n| !is_builtin(n)).unwrap_or(false);
            if ui
                .add_enabled(
                    editable,
                    egui::Button::new(app.strings.gui_edit_current_theme),
                )
                .clicked()
            {
                let spec = app.session.active_theme_spec();
                app.dialog = Some(Dialog::Theme(Box::new(ThemeEditState {
                    original_name: spec.name.clone(),
                    spec,
                })));
                ui.close();
            }
        });
        ui.separator();
        ui.label(
            RichText::new(app.strings.gui_preferences)
                .small()
                .color(app.theme.dim),
        );
        if ui
            .checkbox(
                &mut app.session.confirm_on_exit,
                app.strings.gui_confirm_on_exit,
            )
            .changed()
        {
            app.session.save();
        }
        if ui
            .checkbox(
                &mut app.session.confirm_on_clear,
                app.strings.gui_confirm_on_clear,
            )
            .changed()
        {
            app.session.save();
        }
        if ui
            .checkbox(
                &mut app.session.confirm_on_delete_env,
                app.strings.gui_confirm_delete_env,
            )
            .changed()
        {
            app.session.save();
        }
        if ui
            .checkbox(
                &mut app.session.run_all_batch_mode,
                app.strings.gui_run_all_batch,
            )
            .changed()
        {
            app.session.save();
        }
        let mut hurl_default = app.session.default_request_view == RequestView::Hurl;
        if ui
            .checkbox(&mut hurl_default, app.strings.gui_default_code_hurl)
            .changed()
        {
            app.session.default_request_view = if hurl_default {
                RequestView::Hurl
            } else {
                RequestView::Json
            };
            app.show_hurl = hurl_default;
            app.session.save();
        }
    });
}

fn view_menu(app: &mut GuiApp, ui: &mut egui::Ui) {
    ui.menu_button(app.strings.gui_menu_view, |ui| {
        if ui
            .radio(!app.show_reports, app.strings.gui_request_response)
            .clicked()
        {
            app.show_reports = false;
            ui.close();
        }
        if ui
            .radio(app.show_reports, app.strings.gui_reports)
            .clicked()
        {
            app.show_reports = true;
            ui.close();
        }
    });
}

fn unique_theme_name(app: &GuiApp, base: &str) -> String {
    let exists = |name: &str| app.session.all_themes().iter().any(|t| t.name == name);
    if !exists(base) {
        return base.to_string();
    }
    let mut n = 2;
    while exists(&format!("{base} {n}")) {
        n += 1;
    }
    format!("{base} {n}")
}

// ── Dialogs ─────────────────────────────────────────────────────────────────

pub fn show_dialog(app: &mut GuiApp, ctx: &egui::Context) {
    let Some(dialog) = app.dialog.take() else {
        return;
    };
    match dialog {
        Dialog::OpenFile { kind, path, error } => open_file_dialog(app, ctx, kind, path, error),
        Dialog::SaveFile { kind, path, error } => save_file_dialog(app, ctx, kind, path, error),
        Dialog::Rename { target, text } => rename_dialog(app, ctx, target, text),
        Dialog::Prompt { kind, text } => prompt_dialog(app, ctx, kind, text),
        Dialog::Theme(state) => theme_dialog(app, ctx, *state),
    }
}

/// A centred modal window shell shared by every dialog.
fn modal<R>(ctx: &egui::Context, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, add)
        .unwrap()
        .inner
        .unwrap()
}

fn open_file_dialog(
    app: &mut GuiApp,
    ctx: &egui::Context,
    kind: OpenKind,
    mut path: String,
    mut error: Option<String>,
) {
    let title = match kind {
        OpenKind::Collection => app.strings.gui_open_collection_title,
        OpenKind::Environment => app.strings.gui_open_environment_title,
        OpenKind::Workspace => app.strings.gui_open_workspace_title,
    };
    let lbl_path = app.strings.gui_file_path;
    let lbl_open = app.strings.gui_open;
    let lbl_cancel = app.strings.gui_cancel;
    let err_col = app.theme.err;
    let hint = if kind == OpenKind::Workspace {
        "/path/to/folder"
    } else {
        "/path/to/file"
    };
    let (keep, submit) = modal(ctx, title, |ui| {
        ui.label(lbl_path);
        let resp = ui.add(
            egui::TextEdit::singleline(&mut path)
                .desired_width(420.0)
                .hint_text(hint),
        );
        if let Some(err) = &error {
            ui.colored_label(err_col, err);
        }
        let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let mut keep = true;
        let mut go = submit;
        ui.horizontal(|ui| {
            if ui.button(lbl_open).clicked() {
                go = true;
            }
            if ui.button(lbl_cancel).clicked() {
                keep = false;
            }
        });
        (keep, go)
    });

    if !keep {
        return; // cancelled
    }
    if submit {
        if kind == OpenKind::Workspace {
            let p = PathBuf::from(&path);
            if p.is_dir() {
                app.session.open_workspace(p);
                app.focus = super::Focus::List;
                app.workspace_report = None;
                return; // success closes the dialog
            }
            error = Some(app.strings.gui_not_a_folder.to_string());
            app.dialog = Some(Dialog::OpenFile { kind, path, error });
            return;
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let name = file_stem(&path);
                let ok = match kind {
                    OpenKind::Collection => {
                        app.session
                            .load_collection_text(name, &content, Some(PathBuf::from(&path)))
                    }
                    OpenKind::Environment => app
                        .session
                        .load_environment_text(name, &content, Some(PathBuf::from(&path)), None)
                        .is_some(),
                    // Handled above with an early return (a folder, not a file).
                    OpenKind::Workspace => unreachable!(),
                };
                if ok {
                    return; // success closes the dialog
                }
                error = Some(app.strings.gui_could_not_parse.to_string());
            }
            Err(e) => error = Some(format!("{} {e}", app.strings.gui_could_not_read)),
        }
    }
    app.dialog = Some(Dialog::OpenFile { kind, path, error });
}

fn save_file_dialog(
    app: &mut GuiApp,
    ctx: &egui::Context,
    kind: SaveKind,
    mut path: String,
    mut error: Option<String>,
) {
    let title = match kind {
        SaveKind::Collection => app.strings.gui_save_collection_title,
        SaveKind::Environment(_) => app.strings.gui_save_environment_title,
        SaveKind::Response => app.strings.gui_save_response_title,
    };
    let lbl_path = app.strings.gui_file_path;
    let lbl_save = app.strings.gui_save;
    let lbl_cancel = app.strings.gui_cancel;
    let err_col = app.theme.err;
    let (keep, submit) = modal(ctx, title, |ui| {
        ui.label(lbl_path);
        let resp = ui.add(
            egui::TextEdit::singleline(&mut path)
                .desired_width(420.0)
                .hint_text("/path/to/file"),
        );
        if let Some(err) = &error {
            ui.colored_label(err_col, err);
        }
        let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let mut keep = true;
        let mut go = submit;
        ui.horizontal(|ui| {
            if ui.button(lbl_save).clicked() {
                go = true;
            }
            if ui.button(lbl_cancel).clicked() {
                keep = false;
            }
        });
        (keep, go)
    });

    if !keep {
        return;
    }
    if submit {
        let content = match kind {
            SaveKind::Collection => Some(app.session.collections[app.active_ci()].to_hurl()),
            SaveKind::Environment(id) => app
                .session
                .global_envs
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.to_vars_text()),
            SaveKind::Response => Some(app.session.response.lock().unwrap().body.to_string()),
        };
        match content {
            Some(text) => match std::fs::write(&path, text) {
                Ok(()) => {
                    // Remember the path for collections/environments.
                    let pb = PathBuf::from(&path);
                    match kind {
                        SaveKind::Collection => {
                            let ci = app.active_ci();
                            app.session.collections[ci].path = Some(pb);
                        }
                        SaveKind::Environment(id) => {
                            if let Some(e) = app.session.global_envs.iter_mut().find(|e| e.id == id)
                            {
                                e.path = Some(pb);
                            }
                        }
                        SaveKind::Response => {}
                    }
                    app.session.save();
                    return;
                }
                Err(e) => error = Some(format!("{} {e}", app.strings.gui_could_not_write)),
            },
            None => error = Some(app.strings.gui_nothing_to_save.to_string()),
        }
    }
    app.dialog = Some(Dialog::SaveFile { kind, path, error });
}

fn rename_dialog(app: &mut GuiApp, ctx: &egui::Context, target: RenameTarget, mut text: String) {
    let title = app.strings.gui_rename;
    let lbl_name = app.strings.gui_name;
    let lbl_rename = app.strings.gui_rename;
    let lbl_cancel = app.strings.gui_cancel;
    let (keep, submit) = modal(ctx, title, |ui| {
        let resp = ui.add(
            egui::TextEdit::singleline(&mut text)
                .desired_width(320.0)
                .hint_text(lbl_name),
        );
        let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let mut keep = true;
        let mut go = submit;
        ui.horizontal(|ui| {
            if ui.button(lbl_rename).clicked() {
                go = true;
            }
            if ui.button(lbl_cancel).clicked() {
                keep = false;
            }
        });
        (keep, go)
    });
    if !keep {
        return;
    }
    if submit {
        if !text.trim().is_empty() {
            match target {
                RenameTarget::Request { ci, idx } => {
                    if let Some(col) = app.session.collections.get_mut(ci) {
                        if let Some(entry) = col.entries.get_mut(idx) {
                            entry.title = text.clone();
                            entry.modified = true;
                        }
                        col.invalidate_request_json();
                    }
                }
                RenameTarget::Tab { ci } => {
                    if let Some(col) = app.session.collections.get_mut(ci) {
                        col.name = text.clone();
                    }
                }
            }
            app.session.save();
        }
        return;
    }
    app.dialog = Some(Dialog::Rename { target, text });
}

fn prompt_dialog(app: &mut GuiApp, ctx: &egui::Context, kind: PromptKind, mut text: String) {
    let title = match kind {
        PromptKind::BaseUrl => app.strings.gui_base_url_title,
        PromptKind::NewEnvName => app.strings.gui_new_env_name_title,
        PromptKind::NewCollectionName => app.strings.gui_new_collection_name_title,
    };
    let lbl_ok = app.strings.gui_ok;
    let lbl_cancel = app.strings.gui_cancel;
    let (keep, submit) = modal(ctx, title, |ui| {
        let resp = ui.add(egui::TextEdit::singleline(&mut text).desired_width(360.0));
        let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let mut keep = true;
        let mut go = submit;
        ui.horizontal(|ui| {
            if ui.button(lbl_ok).clicked() {
                go = true;
            }
            if ui.button(lbl_cancel).clicked() {
                keep = false;
            }
        });
        (keep, go)
    });
    if !keep {
        return;
    }
    if submit {
        match kind {
            PromptKind::BaseUrl => {
                app.session.vars.base_url = text.clone();
                app.session.save();
            }
            PromptKind::NewEnvName => {
                let name = if text.trim().is_empty() {
                    app.strings.gui_default_env_name.to_string()
                } else {
                    text.clone()
                };
                app.session.add_environment(name);
            }
            PromptKind::NewCollectionName => {
                let name = if text.trim().is_empty() {
                    app.strings.gui_untitled.to_string()
                } else {
                    text.clone()
                };
                app.session.add_collection(name);
                app.session.save();
            }
        }
        return;
    }
    app.dialog = Some(Dialog::Prompt { kind, text });
}

fn theme_dialog(app: &mut GuiApp, ctx: &egui::Context, mut state: ThemeEditState) {
    enum Action {
        Keep,
        Cancel,
        Apply,
    }
    let strings = Strings::for_language(&app.session.language);
    let title = app.strings.gui_theme_editor_title;
    let lbl_name = app.strings.gui_name;
    let lbl_apply = app.strings.gui_apply;
    let lbl_cancel = app.strings.gui_cancel;
    let action = modal(ctx, title, |ui| {
        ui.horizontal(|ui| {
            ui.label(lbl_name);
            ui.text_edit_singleline(&mut state.spec.name);
        });
        ui.separator();
        egui::Grid::new("theme_colors")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                for i in 0..THEME_COLOR_COUNT {
                    let label = color_label(&strings, i);
                    ui.label(label);
                    let mut rgb = state.spec.color(i);
                    if ui.color_edit_button_srgb(&mut rgb).changed() {
                        state.spec.set_color(i, rgb);
                    }
                    ui.end_row();
                }
            });
        ui.separator();
        let mut action = Action::Keep;
        ui.horizontal(|ui| {
            if ui.button(lbl_apply).clicked() {
                action = Action::Apply;
            }
            if ui.button(lbl_cancel).clicked() {
                action = Action::Cancel;
            }
        });
        action
    });

    match action {
        Action::Cancel => {}
        Action::Apply => {
            // Replace an existing custom theme of the same original name, else add.
            if let Some(existing) = app
                .session
                .custom_themes
                .iter_mut()
                .find(|t| t.name == state.original_name)
            {
                *existing = state.spec.clone();
            } else {
                app.session.custom_themes.push(state.spec.clone());
            }
            app.session.active_theme = Some(state.spec.name.clone());
            app.session.save();
        }
        Action::Keep => {
            app.dialog = Some(Dialog::Theme(Box::new(state)));
        }
    }
}

fn file_stem(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled".to_string())
}

/// The i18n label for the `i`th editable theme colour (mirrors the terminal
/// UI's `theme_editor::color_label`, reading the same `Strings` fields).
fn color_label(s: &Strings, i: usize) -> &'static str {
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
