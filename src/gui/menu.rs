//! The top menu bar and the modal dialogs (open/save, rename, prompt) plus the
//! theme editor. Everything the terminal UI reaches through its `:`-menu and
//! wizards, driven through the shared [`crate::session::Session`].

use std::path::Path;

use eframe::egui::{self, Align2, RichText};

use crate::i18n::{Language, Strings};
use crate::request::RequestView;
use crate::session::PickerKind;
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

/// The File menu. Grouped into submenus by *verb* (New / Open / Save) rather
/// than one flat list, because the local and Git variants of each had grown to
/// a dozen sibling entries where "open" and "save" items were interleaved.
///
/// There is deliberately no separate "Import Postman" entry: Open ▸ Collection
/// sniffs Postman JSON and Hurl alike, so a second command pointing at exactly
/// the same loader only made the menu longer.
fn file_menu(app: &mut GuiApp, ui: &mut egui::Ui) {
    ui.menu_button(app.strings.gui_menu_file, |ui| {
        if ui.button(app.strings.gui_new_collection_ellipsis).clicked() {
            app.dialog = Some(Dialog::Prompt {
                kind: PromptKind::NewCollectionName,
                text: String::new(),
            });
            ui.close();
        }
        ui.separator();

        let mut close_menu = false;
        ui.menu_button(app.strings.gui_menu_open, |ui| {
            if ui.button(app.strings.gui_menu_item_collection).clicked() {
                open_via_picker(app, OpenKind::Collection);
                close_menu = true;
                ui.close();
            }
            if ui.button(app.strings.gui_menu_item_environment).clicked() {
                open_via_picker(app, OpenKind::Environment);
                close_menu = true;
                ui.close();
            }
            if ui.button(app.strings.gui_menu_item_workspace).clicked() {
                open_via_picker(app, OpenKind::Workspace);
                close_menu = true;
                ui.close();
            }
        });
        ui.menu_button(app.strings.gui_menu_open_git, |ui| {
            if ui
                .button(app.strings.gui_menu_item_collection_or_env)
                .clicked()
            {
                app.remote.open_load();
                close_menu = true;
                ui.close();
            }
            if ui.button(app.strings.gui_menu_item_workspace).clicked() {
                app.remote.open_load_workspace();
                close_menu = true;
                ui.close();
            }
        });
        ui.separator();

        ui.menu_button(app.strings.gui_menu_save, |ui| {
            if ui.button(app.strings.gui_menu_item_collection).clicked() {
                save_via_picker(app, SaveKind::Collection);
                close_menu = true;
                ui.close();
            }
            if ui.button(app.strings.gui_menu_item_response).clicked() {
                save_via_picker(app, SaveKind::Response);
                close_menu = true;
                ui.close();
            }
        });
        ui.menu_button(app.strings.gui_menu_save_git, |ui| {
            if ui.button(app.strings.gui_menu_item_collection).clicked() {
                app.remote.open_save_collection(app.active_ci());
                close_menu = true;
                ui.close();
            }
        });
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
            app.request_close_tab(app.active_ci());
            ui.close();
        }
        if ui.button(app.strings.gui_quit).clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // Closing a submenu leaves the parent File menu open; dismiss it too so
        // picking an item doesn't strand a half-open menu over the app.
        if close_menu {
            ui.close();
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
        Dialog::Rename { target, text } => rename_dialog(app, ctx, target, text),
        Dialog::Prompt { kind, text } => prompt_dialog(app, ctx, kind, text),
        Dialog::Theme(state) => theme_dialog(app, ctx, *state),
        Dialog::CloseGitWorkspace { ci, root } => close_git_workspace_dialog(app, ctx, ci, root),
        Dialog::UnsavedQuit { count, tabs } => unsaved_quit_dialog(app, ctx, count, tabs),
        Dialog::UnsavedCloseTab { ci, name, count } => {
            unsaved_close_tab_dialog(app, ctx, ci, name, count)
        }
        Dialog::WorkspaceReload { ci, reload } => workspace_reload_dialog(app, ctx, ci, *reload),
    }
}

/// Last chance before quitting throws away request edits that were never
/// written to a file.
///
/// A Workspace tab's requests are deliberately not persisted between runs (see
/// `persistence`), and edits parked for a file the user has switched away from
/// live only in memory, so "quit" really is the moment they disappear — hence a
/// modal rather than a status line. Cancelling re-arms nothing: the close was
/// already refused, so there is simply nothing left to do.
fn unsaved_quit_dialog(app: &mut GuiApp, ctx: &egui::Context, count: usize, tabs: String) {
    let title = app.strings.gui_unsaved_quit_title;
    let (quit, cancel, question) = (
        app.strings.gui_quit_anyway,
        app.strings.gui_cancel,
        app.strings
            .gui_unsaved_quit_q
            .replace("{n}", &count.to_string())
            .replace("{t}", &tabs),
    );
    let save_all = app.strings.gui_save_all_and_quit;
    let mut decided = false;
    let mut save_then_quit = false;
    modal(ctx, title, |ui| {
        ui.label(question);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            // Offered first: it is the only answer that keeps the work, so it
            // should be the one the eye lands on before "Quit anyway".
            if ui.button(save_all).clicked() {
                save_then_quit = true;
                decided = true;
            }
            if ui.button(quit).clicked() {
                app.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                decided = true;
            }
            if ui.button(cancel).clicked() {
                decided = true;
            }
        });
    });
    if save_then_quit {
        // A save that fails must not take the app down with it -- the dialog
        // comes back reporting the file that refused, so the work is still
        // there to be dealt with.
        match app.save_all_unsaved_edits() {
            Ok(_) => {
                app.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => {
                app.session.status = Some(crate::i18n::Status::Error(e));
                app.dialog = Some(Dialog::UnsavedQuit { count, tabs });
            }
        }
        return;
    }
    if !decided {
        app.dialog = Some(Dialog::UnsavedQuit { count, tabs });
    }
}

/// The same warning for one tab. Confirming hands over to the ordinary close
/// path, so a downloaded git Workspace still gets its keep-or-delete question.
fn unsaved_close_tab_dialog(
    app: &mut GuiApp,
    ctx: &egui::Context,
    ci: usize,
    name: String,
    count: usize,
) {
    let title = app.strings.gui_unsaved_quit_title;
    let (close, cancel, question) = (
        app.strings.gui_close_anyway,
        app.strings.gui_cancel,
        app.strings
            .gui_unsaved_close_tab_q
            .replace("{n}", &count.to_string())
            .replace("{t}", &name),
    );
    let mut decided = false;
    modal(ctx, title, |ui| {
        ui.label(question);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(close).clicked() {
                app.close_tab_now(ci);
                decided = true;
            }
            if ui.button(cancel).clicked() {
                decided = true;
            }
        });
    });
    if !decided {
        app.dialog = Some(Dialog::UnsavedCloseTab { ci, name, count });
    } else {
        app.session.save();
    }
}

/// Keep-or-delete prompt for a Workspace folder PaperBoy downloaded itself.
/// Cancel re-arms the dialog so an accidental click outside can't close the tab.
fn close_git_workspace_dialog(
    app: &mut GuiApp,
    ctx: &egui::Context,
    ci: usize,
    root: std::path::PathBuf,
) {
    let title = app.strings.gui_close_git_workspace_title;
    let (keep, delete, cancel, question) = (
        app.strings.close_git_workspace_keep,
        app.strings.close_git_workspace_delete,
        app.strings.close_git_workspace_cancel,
        app.strings
            .close_git_workspace_q
            .replace("{p}", &root.display().to_string()),
    );
    let mut decided = false;
    modal(ctx, title, |ui| {
        ui.label(question);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(keep).clicked() {
                app.session.close_tab(ci);
                decided = true;
            }
            if ui.button(delete).clicked() {
                app.session.close_tab_deleting_workspace(ci);
                decided = true;
            }
            if ui.button(cancel).clicked() {
                decided = true;
            }
        });
    });
    if !decided {
        app.dialog = Some(Dialog::CloseGitWorkspace { ci, root });
    } else {
        app.session.save();
    }
}

/// Offer to redownload a restored Workspace whose folder has vanished. Declining
/// leaves the tab in place but empty — the recorded origin stays on it, so the
/// same offer comes back next launch.
fn workspace_reload_dialog(
    app: &mut GuiApp,
    ctx: &egui::Context,
    ci: usize,
    reload: crate::persistence::PendingWorkspaceReload,
) {
    let title = app.strings.gui_workspace_reload_title;
    let ref_label = if reload.origin.ref_kind == crate::git_remote::RefKind::Branch {
        app.strings.git_branches
    } else {
        app.strings.git_tags
    };
    let question = app
        .strings
        .workspace_reload_confirm_q
        .replace("{name}", &reload.tab_name)
        .replace(
            "{ref}",
            &format!("[{ref_label}] {}", reload.origin.ref_name),
        )
        .replace("{url}", &reload.origin.repo_url);
    let (yes, no, hint) = (
        app.strings.gui_workspace_reload_yes,
        app.strings.gui_workspace_reload_no,
        app.strings.workspace_reload_save_hint,
    );
    let mut answer: Option<bool> = None;
    modal(ctx, title, |ui| {
        ui.label(question);
        ui.add_space(4.0);
        ui.label(hint);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(yes).clicked() {
                answer = Some(true);
            }
            if ui.button(no).clicked() {
                answer = Some(false);
            }
        });
    });
    match answer {
        Some(true) => app.start_workspace_redownload(ci, reload),
        Some(false) => {}
        // Re-arm until the user answers; the reload can't be silently skipped.
        None => {
            app.dialog = Some(Dialog::WorkspaceReload {
                ci,
                reload: Box::new(reload),
            });
        }
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

/// Open a collection / environment / workspace through a native OS picker.
/// Replaces the old type-a-path modal — a menu click pops the native chooser,
/// and a successful pick loads immediately; failures show a native error alert.
pub fn open_via_picker(app: &mut GuiApp, kind: OpenKind) {
    let title = match kind {
        OpenKind::Collection => app.strings.gui_open_collection_title,
        OpenKind::Environment => app.strings.gui_open_environment_title,
        OpenKind::Workspace => app.strings.gui_open_workspace_title,
    };
    let picker_kind = match kind {
        OpenKind::Environment => PickerKind::Environment,
        _ => PickerKind::Other,
    };
    let dir = app.session.picker_dir(picker_kind).map(|p| p.to_path_buf());
    let picked = match kind {
        OpenKind::Workspace => super::filepick::pick_folder(title, dir.as_deref()),
        OpenKind::Collection => super::filepick::pick_file(
            title,
            dir.as_deref(),
            &[
                (app.strings.gui_filter_collections, &["hurl", "json"]),
                (app.strings.gui_filter_all, &["*"]),
            ],
        ),
        OpenKind::Environment => super::filepick::pick_file(
            title,
            dir.as_deref(),
            &[
                (app.strings.gui_filter_environments, &["vars", "env"]),
                (app.strings.gui_filter_all, &["*"]),
            ],
        ),
    };
    let Some(path) = picked else {
        return; // cancelled
    };
    // Remembered even when the load fails: the user still browsed there, and
    // sending the next picker back to square one would be the bigger annoyance.
    app.session.remember_picker_dir(picker_kind, &path);
    if let Err(msg) = apply_open(app, kind, &path) {
        super::filepick::error_alert(title, &msg);
    }
    app.session.save();
}

/// Load the chosen path as the given kind, returning a user-facing error string
/// on failure (bad folder / unreadable / unparseable). The success side effects
/// (loading into the session, refocusing) mirror the old dialog's submit path.
fn apply_open(app: &mut GuiApp, kind: OpenKind, path: &Path) -> Result<(), String> {
    if kind == OpenKind::Workspace {
        if path.is_dir() {
            app.session.open_workspace(path.to_path_buf());
            app.focus = super::Focus::List;
            app.report_editor = None;
            return Ok(());
        }
        return Err(app.strings.gui_not_a_folder.to_string());
    }
    let path_str = path.to_string_lossy().into_owned();
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("{} {e}", app.strings.gui_could_not_read))?;
    let name = file_stem(&path_str);
    let ok = match kind {
        OpenKind::Collection => {
            app.session
                .load_collection_text(name, &content, Some(path.to_path_buf()))
        }
        OpenKind::Environment => app
            .session
            .load_environment_text(name, &content, Some(path.to_path_buf()), None)
            .is_some(),
        OpenKind::Workspace => unreachable!(),
    };
    if ok {
        Ok(())
    } else {
        Err(app.strings.gui_could_not_parse.to_string())
    }
}

/// Save the active collection / environment / response / report results through
/// a native OS save picker.
pub fn save_via_picker(app: &mut GuiApp, kind: SaveKind) {
    let title = match kind {
        SaveKind::Collection => app.strings.gui_save_collection_title,
        SaveKind::Environment(_) => app.strings.gui_save_environment_title,
        SaveKind::Response => app.strings.gui_save_response_title,
        SaveKind::ReportResults => app.strings.gui_save_results_title,
    };
    // Seed the dialog from any remembered path (collections/environments) and a
    // sensible default filename.
    let current = match kind {
        SaveKind::Collection => app.session.collections[app.active_ci()]
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        SaveKind::Environment(id) => app
            .session
            .global_envs
            .iter()
            .find(|e| e.id == id)
            .and_then(|e| e.path.as_ref())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        // Default a results export to `<report>.csv` beside the report (or the
        // current dir for a scratch report).
        SaveKind::ReportResults => app
            .report_editor
            .as_ref()
            .and_then(|e| e.report.path.as_ref())
            .map(|p| p.with_extension("csv").to_string_lossy().into_owned())
            .unwrap_or_default(),
        SaveKind::Response => String::new(),
    };
    let picker_kind = match kind {
        SaveKind::Environment(_) => PickerKind::Environment,
        _ => PickerKind::Other,
    };
    // The file's own folder wins (a re-save belongs where it already lives);
    // an unsaved item falls back to wherever the user last browsed.
    let dir = super::filepick::seed_dir(&current)
        .or_else(|| app.session.picker_dir(picker_kind).map(|p| p.to_path_buf()));
    let default_name = std::path::Path::new(&current)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| match kind {
            SaveKind::Collection => "collection.hurl".into(),
            SaveKind::Environment(_) => "environment.vars".into(),
            SaveKind::Response => "response.txt".into(),
            SaveKind::ReportResults => "results.csv".into(),
        });
    let filters: &[super::filepick::Filter] = match kind {
        SaveKind::Collection => &[("Hurl", &["hurl"]), ("All files", &["*"])],
        SaveKind::Environment(_) => &[("Vars", &["vars"]), ("All files", &["*"])],
        SaveKind::ReportResults => &[
            ("CSV", &["csv"]),
            ("JSON", &["json"]),
            ("HTML", &["html"]),
            ("Excel", &["xlsx"]),
        ],
        SaveKind::Response => &[("All files", &["*"])],
    };
    let Some(path) = super::filepick::save_file(title, dir.as_deref(), &default_name, filters)
    else {
        return; // cancelled
    };
    app.session.remember_picker_dir(picker_kind, &path);
    if let Err(msg) = apply_save(app, kind, &path) {
        super::filepick::error_alert(title, &msg);
    }
    app.session.save();
}

/// Write the given kind to `path`, returning a user-facing error on failure.
fn apply_save(app: &mut GuiApp, kind: SaveKind, path: &Path) -> Result<(), String> {
    let path_str = path.to_string_lossy();
    // Report results export writes format-specific bytes (chosen by extension).
    if matches!(kind, SaveKind::ReportResults) {
        return export_report_results(app, &path_str);
    }
    let content = match kind {
        SaveKind::Collection => Some(app.session.collections[app.active_ci()].to_hurl()),
        SaveKind::Environment(id) => app
            .session
            .global_envs
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.to_vars_text()),
        SaveKind::Response => Some(app.session.response.lock().unwrap().body.to_string()),
        SaveKind::ReportResults => None,
    };
    let text = content.ok_or_else(|| app.strings.gui_nothing_to_save.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{} {e}", app.strings.gui_could_not_write))?;
    // Remember the path for collections/environments.
    match kind {
        SaveKind::Collection => {
            let ci = app.active_ci();
            app.session.collections[ci].path = Some(path.to_path_buf());
            // The file on disk now matches what is in memory, so the "new" and
            // "edited" pencils must go — including any parked edits for it.
            app.session.collections[ci].mark_saved();
        }
        SaveKind::Environment(id) => {
            if let Some(e) = app.session.global_envs.iter_mut().find(|e| e.id == id) {
                e.path = Some(path.to_path_buf());
            }
        }
        SaveKind::Response | SaveKind::ReportResults => {}
    }
    app.session.save();
    Ok(())
}

/// Write the open report editor's last-run results to `path`, choosing the
/// output format from the file extension (csv / json / html / xlsx). Marks the
/// results exported so a rerun won't warn about discarding them.
fn export_report_results(app: &mut GuiApp, path: &str) -> Result<(), String> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let writer = crate::report::writer::writer_for_extension(ext)
        .ok_or_else(|| format!("{} {ext}", app.strings.gui_unsupported_format))?;
    let ed = app
        .report_editor
        .as_ref()
        .ok_or_else(|| app.strings.gui_nothing_to_save.to_string())?;
    let result = ed
        .result
        .as_ref()
        .ok_or_else(|| app.strings.gui_nothing_to_save.to_string())?;
    let header = ed
        .flow
        .as_ref()
        .map(|f| f.header.clone())
        .unwrap_or_default();
    let bytes = writer.write(result, &header)?;
    std::fs::write(path, bytes).map_err(|e| format!("{} {e}", app.strings.gui_could_not_write))?;
    if let Some(ed) = app.report_editor.as_mut() {
        ed.results_exported = true;
    }
    app.session.status = Some(crate::i18n::Status::ReportExported(path.to_string()));
    Ok(())
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
    let title = match &kind {
        PromptKind::BaseUrl => app.strings.gui_base_url_title,
        PromptKind::NewEnvName => app.strings.gui_new_env_name_title,
        PromptKind::NewCollectionName => app.strings.gui_new_collection_name_title,
        PromptKind::NewWorkspaceFolder { .. } => app.strings.gui_ws_new_folder_title,
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
        match &kind {
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
            PromptKind::NewWorkspaceFolder { ci, dir } => {
                let (ci, dir) = (*ci, dir.clone());
                super::requests::new_workspace_folder(app, ci, &dir, text.trim());
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
