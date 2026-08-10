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

// ── Alt-key menu mnemonics ──────────────────────────────────────────────────

/// Keyboard access to the top-level menus, following the convention every
/// desktop toolkit teaches: `Alt` on its own arms the menu bar and reveals each
/// menu's mnemonic letter, and the letter then opens that menu. `Alt+F` as a
/// single chord does the same thing in one go, because that is what most people
/// who know the pattern actually press.
///
/// The underlines only appear while armed. Showing them permanently puts three
/// pieces of keyboard trivia on screen for the whole session for the benefit of
/// the one moment someone wants them, which is exactly the sort of chrome the
/// rest of this restyle removed.
#[derive(Default)]
pub struct AltMenus {
    /// Alt has been pressed and released alone: the bar is armed and a letter
    /// will open a menu.
    armed: bool,
    /// Alt is currently held and nothing else has been pressed with it yet, so
    /// releasing it now counts as "Alt on its own" rather than as a chord.
    alt_alone: bool,
    alt_was_down: bool,
    /// Each menu's mnemonic and the widget id of its button, collected as the
    /// bar is drawn. egui ids are deterministic frame to frame, so a letter
    /// pressed now opens the menu registered on the previous frame.
    ids: Vec<(char, egui::Id)>,
}

impl AltMenus {
    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Note where a menu button ended up, so a mnemonic can open it.
    fn register(&mut self, mnemonic: char, id: egui::Id) {
        self.ids.retain(|(c, _)| *c != mnemonic);
        self.ids.push((mnemonic, id));
    }

    fn id_for(&self, c: char) -> Option<egui::Id> {
        self.ids
            .iter()
            .find(|(m, _)| m.eq_ignore_ascii_case(&c))
            .map(|(_, id)| *id)
    }
}

/// The first character of a mnemonic string, upper-cased. The i18n table holds
/// these as one-character strings so a translator can pick a letter that suits
/// their language's own words.
fn mnemonic_char(s: &str) -> Option<char> {
    s.chars().next().map(|c| c.to_ascii_uppercase())
}

/// Update the armed state from this frame's input, and open a menu if a
/// mnemonic was pressed.
///
/// Run *before* the buttons are drawn, so opening a menu takes effect on the
/// same frame the key arrives.
fn handle_menu_mnemonics(app: &mut GuiApp, ctx: &egui::Context) {
    // A text field having focus means the letters are being typed into it, not
    // aimed at the menu bar; arming would swallow them.
    if ctx.memory(|m| m.focused().is_some()) {
        app.alt_menus.armed = false;
        app.alt_menus.alt_alone = false;
        return;
    }

    struct Frame {
        alt_down: bool,
        escape: bool,
        // (letter, was Alt held) for each key pressed this frame.
        pressed: Vec<(char, bool)>,
    }
    let f = ctx.input(|i| Frame {
        alt_down: i.modifiers.alt,
        escape: i.key_pressed(egui::Key::Escape),
        pressed: i
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => mnemonic_char(key.name()).map(|c| (c, modifiers.alt)),
                _ => None,
            })
            .collect(),
    });

    // egui reports modifier keys through `modifiers` rather than as key events,
    // so "was Alt pressed on its own" has to be read from the edges of that flag
    // plus whether anything else arrived while it was held.
    if f.alt_down && !app.alt_menus.alt_was_down {
        app.alt_menus.alt_alone = true;
    }
    if !f.pressed.is_empty() {
        app.alt_menus.alt_alone = false;
    }

    let mut opened = false;
    for (c, with_alt) in &f.pressed {
        // `Alt+F` as a chord, or `F` on its own once armed.
        if (*with_alt || app.alt_menus.armed)
            && let Some(id) = app.alt_menus.id_for(*c)
        {
            egui::Popup::open_id(ctx, id.with("popup"));
            opened = true;
            break;
        }
    }

    if opened || f.escape {
        app.alt_menus.armed = false;
        app.alt_menus.alt_alone = false;
    } else if !f.alt_down && app.alt_menus.alt_was_down && app.alt_menus.alt_alone {
        // Alt pressed and released with nothing in between: toggle, so a second
        // Alt puts the bar away again.
        app.alt_menus.armed = !app.alt_menus.armed;
        app.alt_menus.alt_alone = false;
    }
    app.alt_menus.alt_was_down = f.alt_down;
}

/// A menu title with its mnemonic underlined while the bar is armed.
///
/// Falls back to the plain title when the mnemonic doesn't occur in the
/// translated title at all — a translator is free to pick a letter that isn't in
/// the word (Danish "Indstillinger" happens to start with its own), and an
/// underline drawn under the wrong character would be worse than none.
fn menu_title(ui: &egui::Ui, title: &str, mnemonic: char, armed: bool) -> egui::WidgetText {
    if !armed {
        return title.into();
    }
    let Some(pos) = title.to_uppercase().find(mnemonic) else {
        return title.into();
    };
    let font = egui::TextStyle::Button.resolve(ui.style());
    let color = ui.visuals().widgets.inactive.fg_stroke.color;
    let mut job = egui::text::LayoutJob::default();
    let mut push = |s: &str, underline: bool| {
        if s.is_empty() {
            return;
        }
        job.append(
            s,
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color,
                underline: if underline {
                    egui::Stroke::new(1.0, color)
                } else {
                    egui::Stroke::NONE
                },
                ..Default::default()
            },
        );
    };
    let end = pos + title[pos..].chars().next().map_or(0, |c| c.len_utf8());
    push(&title[..pos], false);
    push(&title[pos..end], true);
    push(&title[end..], false);
    job.into()
}

// ── Menu bar ────────────────────────────────────────────────────────────────

pub fn menu_bar(app: &mut GuiApp, ui: &mut egui::Ui) {
    handle_menu_mnemonics(app, ui.ctx());
    egui::MenuBar::new().ui(ui, |ui| {
        file_menu(app, ui);
        settings_menu(app, ui);
        view_menu(app, ui);
        // No Send button here. There used to be one pinned to the right of this
        // bar, doing exactly what the Send beside the URL does (`run_active`) —
        // but it was drawn unconditionally, so in the report editor or a
        // workspace view it fired at whatever request happened to be selected
        // off-screen. One Send, next to the request it sends.
    });
}

/// Draw one top-level menu button, registering its mnemonic so `Alt`+letter can
/// open it and underlining that letter while the bar is armed.
fn top_menu<R>(
    app: &mut GuiApp,
    ui: &mut egui::Ui,
    title: &str,
    mnemonic: &str,
    content: impl FnOnce(&mut GuiApp, &mut egui::Ui) -> R,
) {
    let armed = app.alt_menus.is_armed();
    let Some(m) = mnemonic_char(mnemonic) else {
        return;
    };
    let text = menu_title(ui, title, m, armed);
    let resp = ui.menu_button(text, |ui| content(app, ui));
    app.alt_menus.register(m, resp.response.id);
}

/// The File menu. Grouped into submenus by *verb* (New / Open / Save) rather
/// than one flat list, because the local and Git variants of each had grown to
/// a dozen sibling entries where "open" and "save" items were interleaved.
///
/// There is deliberately no separate "Import Postman" entry: Open ▸ Collection
/// sniffs Postman JSON and Hurl alike, so a second command pointing at exactly
/// the same loader only made the menu longer.
fn file_menu(app: &mut GuiApp, ui: &mut egui::Ui) {
    let (title, mnemonic) = (app.strings.gui_menu_file, app.strings.gui_menu_file_key);
    top_menu(app, ui, title, mnemonic, |app, ui| {
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
            if ui.button(app.strings.file_kind_report).clicked() {
                open_via_picker(app, OpenKind::Report);
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
        // A Postman import produces a whole folder of collections and
        // environments, so it belongs beside the other Workspace sources.
        if ui.button(app.strings.postman_menu_item).clicked() {
            app.postman.open();
            close_menu = true;
            ui.close();
        }
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
            // Only saveable while a report is open in the editor -- the same
            // condition the "Save to git > Report" entry uses.
            if ui
                .add_enabled(
                    app.report_editor.is_some(),
                    egui::Button::new(app.strings.file_kind_report),
                )
                .clicked()
            {
                save_via_picker(app, SaveKind::Report);
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
            // Only offered where it can work: a workspace push needs a tab that
            // came from git, and a report push needs a report to be open.
            let ci = app.active_ci();
            let is_ws = app
                .session
                .collections
                .get(ci)
                .is_some_and(|c| c.workspace_git_origin.is_some());
            if ui
                .add_enabled(
                    is_ws,
                    egui::Button::new(app.strings.gui_menu_item_workspace),
                )
                .clicked()
            {
                app.remote.open_save_workspace(ci);
                close_menu = true;
                ui.close();
            }
            if ui
                .add_enabled(
                    app.report_editor.is_some(),
                    egui::Button::new(app.strings.file_kind_report),
                )
                .clicked()
            {
                app.remote.open_save_report();
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
    let (title, mnemonic) = (
        app.strings.gui_menu_settings,
        app.strings.gui_menu_settings_key,
    );
    top_menu(app, ui, title, mnemonic, |app, ui| {
        ui.menu_button(app.strings.gui_language, |ui| {
            for (lang, label) in [
                (Language::English, app.strings.lang_english),
                (Language::French, app.strings.lang_french),
                (Language::Danish, app.strings.lang_danish),
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
    let (title, mnemonic) = (app.strings.gui_menu_view, app.strings.gui_menu_view_key);
    top_menu(app, ui, title, mnemonic, |app, ui| {
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
    let _ = modal(ctx, title, |ui| {
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
    let _ = modal(ctx, title, |ui| {
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
    let _ = modal(ctx, title, |ui| {
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
    let _ = modal(ctx, title, |ui| {
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
///
/// Returns `None` when egui declined to show the window at all (it is opened
/// non-collapsible, so in practice this only happens if the viewport is in a
/// state where nothing can be drawn). Callers treat that as "no answer this
/// frame" and leave the dialog armed rather than deciding for the user — a
/// render path must never panic, and an unanswered dialog simply reappears.
fn modal<R>(ctx: &egui::Context, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, add)
        .and_then(|r| r.inner)
}

/// What an open dialog is for, once it answers. See [`PickAction`].
fn open_title(app: &GuiApp, kind: OpenKind) -> &'static str {
    match kind {
        OpenKind::Collection => app.strings.gui_open_collection_title,
        OpenKind::Environment => app.strings.gui_open_environment_title,
        OpenKind::Workspace => app.strings.gui_open_workspace_title,
        OpenKind::Report => app.strings.gui_open_report_title,
    }
}

fn open_picker_kind(kind: OpenKind) -> PickerKind {
    match kind {
        OpenKind::Environment => PickerKind::Environment,
        _ => PickerKind::Other,
    }
}

/// Open a collection / environment / workspace through a native OS picker.
/// Replaces the old type-a-path modal — a menu click pops the native chooser,
/// and a successful pick loads immediately; failures show a native error alert.
///
/// The dialog runs on a worker thread and is collected by
/// [`poll_pending_pick`] some frames later, so this returns straight away.
pub fn open_via_picker(app: &mut GuiApp, kind: OpenKind) {
    let title = open_title(app, kind);
    let dir = app
        .session
        .picker_dir(open_picker_kind(kind))
        .map(|p| p.to_path_buf());
    let filters = |f: &[super::filepick::Filter]| super::filepick::owned_filters(f);
    let pick_kind = match kind {
        OpenKind::Workspace => super::filepick::PickKind::Folder,
        OpenKind::Collection => super::filepick::PickKind::File {
            filters: filters(&[
                (app.strings.gui_filter_collections, &["hurl", "json"]),
                (app.strings.gui_filter_all, &["*"]),
            ]),
        },
        OpenKind::Environment => super::filepick::PickKind::File {
            filters: filters(&[
                (
                    app.strings.gui_filter_environments,
                    &["vars", "env", "json"],
                ),
                (app.strings.gui_filter_all, &["*"]),
            ]),
        },
        OpenKind::Report => super::filepick::PickKind::File {
            filters: filters(&[
                (app.strings.gui_filter_reports, &["trail"]),
                (app.strings.gui_filter_all, &["*"]),
            ]),
        },
    };
    app.request_pick(pick_kind, title, dir.as_deref(), PickAction::Open(kind));
}

/// What to do with a path once its dialog answers.
///
/// The click that opened the dialog knows what it was for; the frame that
/// collects the path, many frames later, does not — so the intent travels with
/// the request.
pub enum PickAction {
    Open(OpenKind),
    Save(SaveKind),
    /// A `root:` / `baseline:` / `collection:` path in the report editor's
    /// settings panel.
    ReportHeaderFile {
        key: &'static str,
        occurrence: usize,
    },
    /// The folder (or file) a `FOR … IN FILES/FOLDERS` loop walks.
    ReportLoopDir {
        path: Vec<usize>,
        file: bool,
    },
    /// Where a Postman import should put the workspace it builds.
    PostmanDest,
    /// The folder a `FILES`/`FOLDERS` node wizard is being pointed at.
    ReportWizardDir,
    /// Naming a new collection / report / environment in a workspace.
    NewWorkspaceItem {
        ci: usize,
        kind: crate::workspace::NewItemKind,
    },
    /// A `File`/`Base64File` value in a request's Form or Multipart body.
    FormFieldFile {
        ci: usize,
        entry: usize,
        field: usize,
    },
    /// Where to keep a workspace just downloaded from a git remote.
    GitWorkspaceDir,
}

/// Collect a finished file dialog, if one has finished, and act on it. Called
/// once per frame from [`super::app::GuiApp::draw`].
pub fn poll_pending_pick(app: &mut GuiApp) {
    let Some(pending) = app.pending_pick.as_mut() else {
        return;
    };
    let Some((action, picked)) = pending.take() else {
        return; // still open — the usual case
    };
    app.pending_pick = None;
    // The report editor's own pickers write into the open editor rather than
    // loading a file, so they neither seed the shared picker directory nor have
    // an error to report.
    match action {
        PickAction::ReportHeaderFile { key, occurrence } => {
            super::report_editor::apply_picked_header_file(app, key, occurrence, picked);
            return;
        }
        PickAction::ReportLoopDir { path, file } => {
            super::report_editor::apply_picked_loop_dir(app, &path, file, picked);
            return;
        }
        PickAction::PostmanDest => {
            super::postman::apply_picked_dest(app, picked);
            return;
        }
        PickAction::ReportWizardDir => {
            super::report_wizard::apply_picked_dir(app, picked);
            return;
        }
        PickAction::NewWorkspaceItem { ci, kind } => {
            super::requests::apply_new_workspace_item(app, ci, kind, picked);
            return;
        }
        PickAction::FormFieldFile { ci, entry, field } => {
            super::editor::apply_picked_form_file(app, ci, entry, field, picked);
            return;
        }
        PickAction::GitWorkspaceDir => {
            super::remote::apply_picked_workspace_dir(app, picked);
            return;
        }
        _ => {}
    }
    let (title, picker_kind) = match action {
        PickAction::Open(kind) => (open_title(app, kind), open_picker_kind(kind)),
        PickAction::Save(kind) => (save_title(app, kind), save_picker_kind(kind)),
        _ => unreachable!("handled above"),
    };
    let Some(path) = picked else {
        return; // cancelled
    };
    // Remembered even when the load fails: the user still browsed there, and
    // sending the next picker back to square one would be the bigger annoyance.
    app.session.remember_picker_dir(picker_kind, &path);
    let outcome = match action {
        PickAction::Open(kind) => apply_open(app, kind, &path),
        PickAction::Save(kind) => apply_save(app, kind, &path),
        _ => unreachable!("handled above"),
    };
    if let Err(msg) = outcome {
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
            app.close_report_editor();
            return Ok(());
        }
        return Err(app.strings.gui_not_a_folder.to_string());
    }
    let path_str = path.to_string_lossy().into_owned();
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("{} {e}", app.strings.gui_could_not_read))?;
    let name = file_stem(&path_str);
    // A report opens into the report editor rather than a tab. It also joins
    // the session's report list, so it is listed beside the others, survives a
    // restart, and its edits are saved back to the file it came from.
    if kind == OpenKind::Report {
        let report = crate::report::Report::load_local(path)?;
        let existing = app
            .session
            .reports
            .iter()
            .position(|r| r.path.as_deref() == Some(&path_str));
        let idx = match existing {
            Some(i) => i,
            None => {
                app.session
                    .reports
                    .push(crate::persistence::PersistedReport {
                        name: report.name.clone(),
                        text: report.text.clone(),
                        path: Some(path_str.clone()),
                        git_origin: None,
                        workspace_root: None,
                        embedded_active: true,
                    });
                app.session.reports.len() - 1
            }
        };
        app.open_report_editor(
            crate::gui::report_editor::ReportOrigin::Session(idx),
            report,
        );
        app.focus = super::Focus::Main;
        return Ok(());
    }
    let ok = match kind {
        OpenKind::Collection => {
            app.session
                .load_collection_text(name, &content, Some(path.to_path_buf()))
        }
        OpenKind::Environment => app
            .session
            .load_environment_text(name, &content, Some(path.to_path_buf()), None)
            .is_some(),
        OpenKind::Workspace | OpenKind::Report => unreachable!(),
    };
    if ok {
        Ok(())
    } else {
        Err(app.strings.gui_could_not_parse.to_string())
    }
}

/// The results-export dialog's format filters, with `ext`'s own format moved to
/// the front.
///
/// The dialog applies its first filter by default, so leading with the format
/// the report's `# output:` directive declares means exporting a report that
/// says `# output: xlsx` writes a spreadsheet without the user touching the
/// dropdown — while the other three stay one click away. This is what gives the
/// GUI the terminal UI's behaviour, whose export picker is seeded the same way.
/// An unrecognised extension leaves the order alone, so CSV leads as before.
fn report_result_filters(ext: &str) -> Vec<super::filepick::Filter<'static>> {
    const ALL: [super::filepick::Filter<'static>; 4] = [
        ("CSV", &["csv"]),
        ("JSON", &["json"]),
        ("HTML", &["html"]),
        ("Excel", &["xlsx"]),
    ];
    let want = ext.to_ascii_lowercase();
    let mut filters = ALL.to_vec();
    if let Some(i) = filters
        .iter()
        .position(|(_, exts)| exts.contains(&want.as_str()))
    {
        let leading = filters.remove(i);
        filters.insert(0, leading);
    }
    filters
}

/// Save the active collection / environment / response / report results through
/// a native OS save picker.
fn save_title(app: &GuiApp, kind: SaveKind) -> &'static str {
    match kind {
        SaveKind::Collection => app.strings.gui_save_collection_title,
        SaveKind::Environment(_) => app.strings.gui_save_environment_title,
        SaveKind::Response => app.strings.gui_save_response_title,
        SaveKind::ReportResults => app.strings.gui_save_results_title,
        SaveKind::ReportBaseline => app.strings.gui_save_baseline_title,
        SaveKind::Report => app.strings.gui_save_report_title,
    }
}

fn save_picker_kind(kind: SaveKind) -> PickerKind {
    match kind {
        SaveKind::Environment(_) => PickerKind::Environment,
        _ => PickerKind::Other,
    }
}

pub fn save_via_picker(app: &mut GuiApp, kind: SaveKind) {
    let title = save_title(app, kind);
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
        // A results export defaults to the format the report's `# output:`
        // directive declares (CSV when it declares none), beside the report --
        // the same name the terminal UI suggests, `{time}` token included. The
        // dialog can still be pointed at any of the other formats.
        SaveKind::ReportResults => app
            .report_editor
            .as_ref()
            .map(|e| {
                crate::report::writer::export_path(
                    &e.report,
                    &crate::report::writer::report_output_extension(&e.report),
                )
            })
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        // A baseline snapshot is named the same way, so a report's results file
        // and its snapshot sit side by side under one stem.
        SaveKind::ReportBaseline => app
            .report_editor
            .as_ref()
            .map(|e| crate::report::writer::export_path(&e.report, "baseline"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        // A report saves as its own `.trail` source, seeded from the file it was
        // loaded from so "Save report" over an opened file re-offers that file.
        SaveKind::Report => app
            .report_editor
            .as_ref()
            .and_then(|e| e.report.path.clone())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                app.report_editor
                    .as_ref()
                    .map(|e| format!("{}.trail", e.report.name))
                    .unwrap_or_default()
            }),
        SaveKind::Response => String::new(),
    };
    let picker_kind = save_picker_kind(kind);
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
            SaveKind::ReportBaseline => "report.baseline".into(),
            SaveKind::Report => "report.trail".into(),
        });
    let result_filters = report_result_filters(
        std::path::Path::new(&current)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default(),
    );
    let filters: &[super::filepick::Filter] = match kind {
        SaveKind::Collection => &[("Hurl", &["hurl"]), ("All files", &["*"])],
        SaveKind::Environment(_) => &[("Vars", &["vars"]), ("All files", &["*"])],
        SaveKind::ReportResults => &result_filters,
        SaveKind::ReportBaseline => &[("Baseline", &["baseline"]), ("All files", &["*"])],
        SaveKind::Report => &[("PaperTrail", &["trail"]), ("All files", &["*"])],
        SaveKind::Response => &[("All files", &["*"])],
    };
    app.request_pick(
        super::filepick::PickKind::Save {
            default_name,
            filters: super::filepick::owned_filters(filters),
        },
        title,
        dir.as_deref(),
        PickAction::Save(kind),
    );
}

/// Write the given kind to `path`, returning a user-facing error on failure.
fn apply_save(app: &mut GuiApp, kind: SaveKind, path: &Path) -> Result<(), String> {
    let path_str = path.to_string_lossy();
    // Report results export writes format-specific bytes (chosen by extension).
    if matches!(kind, SaveKind::ReportResults) {
        return export_report_results(app, &path_str);
    }
    if matches!(kind, SaveKind::ReportBaseline) {
        return save_report_baseline(app, path);
    }
    if matches!(kind, SaveKind::Report) {
        super::report_editor::save_report_to(app, path)?;
        app.session.save();
        return Ok(());
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
        SaveKind::ReportResults | SaveKind::ReportBaseline | SaveKind::Report => None,
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
        SaveKind::Response
        | SaveKind::ReportResults
        | SaveKind::ReportBaseline
        | SaveKind::Report => {}
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
    // A run in flight and a run never started both leave `result` empty, but
    // they are different problems: one is "wait", the other is "press Run".
    // "Nothing to save" told the reader neither.
    let result = ed.result.as_ref().ok_or_else(|| {
        if ed.is_running() {
            app.strings.report_export_still_running.to_string()
        } else {
            app.strings.report_export_no_result.to_string()
        }
    })?;
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

/// Write the open report editor's last run to `path` as a `.baseline` JSON
/// snapshot — PaperTrail's "Source B", which a later run diffs against once its
/// `# baseline:` directive or a `BASELINE(FILE(…))` role points at the file.
///
/// Like a results export, this marks the run exported: the result is on disk
/// now, so a rerun needn't warn about discarding it.
fn save_report_baseline(app: &mut GuiApp, path: &Path) -> Result<(), String> {
    let result = app
        .report_editor
        .as_ref()
        .and_then(|e| e.result.as_ref())
        .ok_or_else(|| app.strings.report_baseline_no_result.to_string())?;
    crate::report::Baseline::from_result(result)
        .save(path)
        .map_err(|e| format!("{} {e}", app.strings.gui_could_not_write))?;
    if let Some(ed) = app.report_editor.as_mut() {
        ed.results_exported = true;
    }
    app.session.status = Some(crate::i18n::Status::ReportBaselineSaved(
        path.display().to_string(),
    ));
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
    })
    // A frame egui never drew is not an answer: keep the dialog open.
    .unwrap_or((true, false));
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
    })
    // A frame egui never drew is not an answer: keep the dialog open.
    .unwrap_or((true, false));
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
    })
    // A frame egui never drew is not an answer: keep the dialog open.
    .unwrap_or(Action::Keep);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;
    use crate::session::Session;

    fn app() -> GuiApp {
        GuiApp::for_test(Session::default())
    }

    /// Saving a baseline writes a snapshot the report engine can load straight
    /// back — the whole point of the button, and the thing a compile check
    /// can't tell you.
    #[test]
    fn saving_a_baseline_writes_a_snapshot_that_loads_again() {
        use crate::report::model::{ReportResult, ReportRow};

        let mut app = app();
        let mut ed = crate::gui::report_editor::ReportEditor::new(
            crate::gui::report_editor::ReportOrigin::Session(0),
            crate::report::Report::scratch("r"),
        );
        let mut result = ReportResult::default();
        result.rows.push(ReportRow {
            cells: [("Time".to_string(), "100".to_string())]
                .into_iter()
                .collect(),
            key: vec!["a".to_string()],
            ..Default::default()
        });
        ed.result = Some(result);
        app.report_editor = Some(ed);

        let dir = std::env::temp_dir().join(format!("pb_gui_baseline_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.baseline");
        let saved = save_report_baseline(&mut app, &path);
        let loaded = crate::report::Baseline::load(&path);
        std::fs::remove_dir_all(&dir).ok();

        assert!(saved.is_ok(), "saving should succeed: {saved:?}");
        let loaded = loaded.expect("the snapshot must load back");
        assert_eq!(loaded.rows.len(), 1);
        assert_eq!(
            loaded.rows[0].cells.get("Time").map(String::as_str),
            Some("100")
        );
        // The run is on disk now, so a rerun needn't warn about losing it.
        assert!(app.report_editor.as_ref().unwrap().results_exported);
    }

    /// Opening a report from disk must put it in the editor *and* in the
    /// session's report list, so it is listed beside the others and survives a
    /// restart -- the thing that makes it a real tab rather than a scratch view.
    #[test]
    fn opening_a_report_file_loads_it_into_the_editor_and_the_session() {
        let mut app = app();
        let dir = std::env::temp_dir().join(format!("pb_gui_open_rep_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nightly.trail");
        std::fs::write(&path, "# name: nightly\nREPORT smoke\n").unwrap();

        let res = apply_open(&mut app, OpenKind::Report, &path);
        // Opening the same file twice must reuse its tab rather than stack up
        // duplicates that would then disagree about the file's contents.
        let again = apply_open(&mut app, OpenKind::Report, &path);
        std::fs::remove_dir_all(&dir).ok();

        assert!(res.is_ok() && again.is_ok(), "{res:?} {again:?}");
        let ed = app.report_editor.as_ref().expect("editor must be open");
        assert_eq!(ed.report.name, "nightly");
        assert_eq!(app.session.reports.len(), 1);
        assert_eq!(
            app.session.reports[0].path.as_deref(),
            Some(path.to_string_lossy().as_ref())
        );
    }

    /// Saving a report writes its source and adopts the path, so the next
    /// Ctrl+S goes straight to the same file instead of asking again.
    #[test]
    fn saving_a_report_writes_its_source_and_adopts_the_path() {
        let mut app = app();
        let mut report = crate::report::Report::scratch("r");
        report.text = "# name: r\nREPORT smoke\n".into();
        app.report_editor = Some(crate::gui::report_editor::ReportEditor::new(
            crate::gui::report_editor::ReportOrigin::Session(0),
            report,
        ));
        app.session
            .reports
            .push(crate::persistence::PersistedReport {
                name: "r".into(),
                text: String::new(),
                path: None,
                git_origin: None,
                workspace_root: None,
                embedded_active: true,
            });

        let dir = std::env::temp_dir().join(format!("pb_gui_save_rep_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.trail");
        let saved = apply_save(&mut app, SaveKind::Report, &path);
        let text = std::fs::read_to_string(&path);
        std::fs::remove_dir_all(&dir).ok();

        assert!(saved.is_ok(), "saving should succeed: {saved:?}");
        assert_eq!(text.unwrap(), "# name: r\nREPORT smoke\n");
        let ed = app.report_editor.as_ref().unwrap();
        assert_eq!(ed.report.path.as_deref(), Some(path.as_path()));
        assert!(!ed.report.dirty);
        assert_eq!(
            app.session.reports[0].path.as_deref(),
            Some(path.to_string_lossy().as_ref())
        );
    }

    /// With nothing to snapshot the button reports why rather than writing an
    /// empty file that a later run would silently diff against.
    #[test]
    fn saving_a_baseline_without_a_run_is_refused() {
        let mut app = app();
        app.report_editor = Some(crate::gui::report_editor::ReportEditor::new(
            crate::gui::report_editor::ReportOrigin::Session(0),
            crate::report::Report::scratch("r"),
        ));
        let path = std::env::temp_dir().join("pb_gui_baseline_never_written.baseline");
        let err = save_report_baseline(&mut app, &path).expect_err("no result, no snapshot");
        assert_eq!(err, app.strings.report_baseline_no_result);
        assert!(!path.exists(), "nothing should have been written");
    }

    /// The declared output format leads the export dialog's filters, so the
    /// default choice is the one the report asked for.
    #[test]
    fn the_reports_own_output_format_leads_the_export_filters() {
        for (ext, expected) in [("xlsx", "Excel"), ("json", "JSON"), ("html", "HTML")] {
            let filters = report_result_filters(ext);
            assert_eq!(filters[0].0, expected, "{ext} should lead");
            assert_eq!(filters.len(), 4, "every format stays available");
        }
        // Case is irrelevant: `# output: XLSX` is the same directive.
        assert_eq!(report_result_filters("XLSX")[0].0, "Excel");
    }

    /// With no (or an unwritable) format declared, the list keeps its usual
    /// order, so CSV remains the default it has always been.
    #[test]
    fn an_unknown_export_format_leaves_csv_leading() {
        for ext in ["", "csv", "pdf"] {
            let filters = report_result_filters(ext);
            assert_eq!(filters[0].0, "CSV", "{ext:?} should leave CSV leading");
            assert_eq!(
                filters.iter().map(|f| f.0).collect::<Vec<_>>(),
                vec!["CSV", "JSON", "HTML", "Excel"]
            );
        }
    }

    /// Draw the menu bar once with the given input, so mnemonics are registered
    /// and the key handling runs exactly as it does in the real app.
    fn frame(app: &mut GuiApp, ctx: &egui::Context, input: egui::RawInput) {
        let mut input = input;
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(900.0, 600.0),
        ));
        let _ = ctx.run_ui(input, |ui| menu_bar(app, ui));
    }

    fn alt_held() -> egui::RawInput {
        egui::RawInput {
            modifiers: egui::Modifiers::ALT,
            ..Default::default()
        }
    }

    #[test]
    fn menu_mnemonics_are_unique_within_each_language() {
        // A duplicate would make one of the two menus unreachable from the
        // keyboard, and silently: the first match wins.
        for lang in [Language::English, Language::French, Language::Danish] {
            let s = crate::i18n::Strings::for_language(&lang);
            let keys = [
                s.gui_menu_file_key,
                s.gui_menu_view_key,
                s.gui_menu_settings_key,
            ];
            for k in keys {
                assert_eq!(
                    k.chars().count(),
                    1,
                    "{lang:?} mnemonic {k:?} is not a single character"
                );
            }
            let mut seen: Vec<char> = keys.iter().filter_map(|k| mnemonic_char(k)).collect();
            seen.sort_unstable();
            let before = seen.len();
            seen.dedup();
            assert_eq!(before, seen.len(), "duplicate mnemonic in {lang:?}");
        }
    }

    #[test]
    fn pressing_alt_on_its_own_arms_the_menu_bar_and_a_second_alt_puts_it_away() {
        let ctx = egui::Context::default();
        let mut a = app();
        frame(&mut a, &ctx, alt_held());
        assert!(!a.alt_menus.is_armed(), "arming waits for the release");
        frame(&mut a, &ctx, egui::RawInput::default());
        assert!(a.alt_menus.is_armed());

        frame(&mut a, &ctx, alt_held());
        frame(&mut a, &ctx, egui::RawInput::default());
        assert!(!a.alt_menus.is_armed());
    }

    #[test]
    fn alt_used_as_a_chord_does_not_leave_the_bar_armed() {
        // Alt+F opens File outright; it must not also leave the bar waiting for
        // another letter once Alt comes back up.
        let ctx = egui::Context::default();
        let mut a = app();
        frame(&mut a, &ctx, egui::RawInput::default());

        let mut input = alt_held();
        input.events.push(egui::Event::Key {
            key: egui::Key::F,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::ALT,
        });
        frame(&mut a, &ctx, input);
        frame(&mut a, &ctx, egui::RawInput::default());
        assert!(!a.alt_menus.is_armed());
    }

    #[test]
    fn a_mnemonic_opens_its_menu_both_as_a_chord_and_once_armed() {
        for chord in [true, false] {
            let ctx = egui::Context::default();
            let mut a = app();
            // One frame to register the buttons: egui ids are only known once
            // the widgets have been laid out.
            frame(&mut a, &ctx, egui::RawInput::default());
            let file = a.alt_menus.id_for('F').expect("File menu registered");

            let mut input = if chord {
                alt_held()
            } else {
                frame(&mut a, &ctx, alt_held());
                frame(&mut a, &ctx, egui::RawInput::default());
                assert!(a.alt_menus.is_armed());
                egui::RawInput::default()
            };
            let modifiers = if chord {
                egui::Modifiers::ALT
            } else {
                egui::Modifiers::NONE
            };
            input.events.push(egui::Event::Key {
                key: egui::Key::F,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            });
            frame(&mut a, &ctx, input);

            assert!(
                egui::Popup::is_id_open(&ctx, file.with("popup")),
                "chord={chord}: File menu should be open"
            );
            assert!(!a.alt_menus.is_armed(), "chord={chord}: opening disarms");
        }
    }

    #[test]
    fn escape_cancels_an_armed_menu_bar() {
        let ctx = egui::Context::default();
        let mut a = app();
        frame(&mut a, &ctx, alt_held());
        frame(&mut a, &ctx, egui::RawInput::default());
        assert!(a.alt_menus.is_armed());

        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        frame(&mut a, &ctx, input);
        assert!(!a.alt_menus.is_armed());
    }

    #[test]
    fn the_mnemonic_is_underlined_only_while_the_bar_is_armed() {
        let ctx = egui::Context::default();
        let underlined = |armed: bool| {
            let mut found = Vec::new();
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                let text = menu_title(ui, "Settings", 'S', armed);
                found = match text {
                    egui::WidgetText::LayoutJob(job) => job
                        .sections
                        .iter()
                        .filter(|s| s.format.underline != egui::Stroke::NONE)
                        .map(|s| {
                            job.text[usize::from(s.byte_range.start)..usize::from(s.byte_range.end)]
                                .to_string()
                        })
                        .collect::<Vec<_>>(),
                    // A plain string carries no formatting at all, which is the
                    // unarmed case.
                    _ => Vec::new(),
                };
            });
            found
        };
        assert_eq!(underlined(false), Vec::<String>::new());
        assert_eq!(underlined(true), vec!["S".to_string()]);
    }
}
