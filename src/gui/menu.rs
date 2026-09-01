//! The top menu bar and the modal dialogs (open/save, rename, prompt) plus the
//! theme editor. Everything the terminal UI reaches through its `:`-menu and
//! wizards, driven through the shared [`crate::session::Session`].

use std::path::Path;

use eframe::egui;

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
    /// Whether a lone `Alt` has put the bar into "waiting for a letter" mode.
    ///
    /// The underlines no longer depend on this — they are always drawn — so
    /// this is now only read by the tests that pin the arming state machine.
    #[cfg(test)]
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

/// A menu title with its mnemonic underlined.
///
/// Drawn always, not only once `Alt` has armed the bar. Hiding the underlines
/// until Alt is tapped is a Windows convention that assumes you already know
/// the mnemonics are there — and here it left the menus looking as though they
/// had none, so nobody would think to press Alt to find out. The underline is
/// the only thing that advertises the feature, so it has to be visible before
/// the feature is used.
///
/// Falls back to the plain title when the mnemonic doesn't occur in the
/// translated title at all — a translator is free to pick a letter that isn't in
/// the word (Danish "Indstillinger" happens to start with its own), and an
/// underline drawn under the wrong character would be worse than none.
fn menu_title(ui: &egui::Ui, title: &str, mnemonic: char) -> egui::WidgetText {
    // Matched case-insensitively over the *original* title, so `pos` is a byte
    // index into the string actually being sliced below. Searching an
    // uppercased copy instead would drift the moment a language's uppercase
    // form has a different byte length from its lowercase one (German "ß" is
    // the classic; Turkish dotted "i" is the near miss), silently underlining
    // the wrong character.
    let Some((pos, matched)) = title
        .char_indices()
        .find(|(_, c)| c.to_uppercase().eq(mnemonic.to_uppercase()))
    else {
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
    let end = pos + matched.len_utf8();
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
        edit_menu(app, ui);
        settings_menu(app, ui);
        view_menu(app, ui);
        help_menu(app, ui);
        // No Send button here. There used to be one pinned to the right of this
        // bar, doing exactly what the Send beside the URL does (`run_active`) —
        // but it was drawn unconditionally, so in the report editor or a
        // workspace view it fired at whatever request happened to be selected
        // off-screen. One Send, next to the request it sends.
    });
}

/// The Help menu: a home for the keyboard-shortcuts overlay so F1 is not the
/// only way to find it. Discoverability was the whole complaint — a shortcut no
/// menu points at is a shortcut nobody knows exists — so the overlay earns a
/// visible entry, with its F1 accelerator shown the same way Save shows Ctrl+S.
fn help_menu(app: &mut GuiApp, ui: &mut egui::Ui) {
    let (title, mnemonic) = (app.strings.gui_menu_help, app.strings.gui_menu_help_key);
    top_menu(app, ui, title, mnemonic, |app, ui| {
        if ui
            .button(format!(
                "{}\t{}",
                app.strings.gui_shortcuts_title, app.strings.gui_shortcut_help
            ))
            .clicked()
        {
            app.dialog = Some(Dialog::Shortcuts);
            ui.close();
        }
    });
}

/// Draw one top-level menu button, registering its mnemonic so `Alt`+letter can
/// open it and underlining that letter.
fn top_menu<R>(
    app: &mut GuiApp,
    ui: &mut egui::Ui,
    title: &str,
    mnemonic: &str,
    content: impl FnOnce(&mut GuiApp, &mut egui::Ui) -> R,
) {
    let Some(m) = mnemonic_char(mnemonic) else {
        return;
    };
    let text = menu_title(ui, title, m);
    let resp = ui.menu_button(text, |ui| content(app, ui));
    app.alt_menus.register(m, resp.response.id);
}

/// The Edit menu. Currently just the one entry — undoing a request delete —
/// but that action needed a home somewhere more discoverable than "press `u`
/// in the terminal UI", and there was no existing menu that fit rather than
/// stretched to accommodate it (File is about whole files; Settings and View
/// are configuration, not an in-session action). A one-item menu is a fair
/// trade for that: the alternative was wedging it into the Requests panel's
/// header, which only the graphical front-end has and which was still less
/// discoverable than the standard place every editor puts "Undo".
fn edit_menu(app: &mut GuiApp, ui: &mut egui::Ui) {
    let (title, mnemonic) = (app.strings.gui_menu_edit, app.strings.gui_menu_edit_key);
    top_menu(app, ui, title, mnemonic, |app, ui| {
        let ci = app.active_ci();
        // Disabled rather than hidden when there's nothing to restore, so the
        // menu's shape doesn't shift under a user who opens it out of habit —
        // and so the one item in it is still there to explain what the
        // shortcut does even when it currently can't.
        let has_deleted = !app.session.collections[ci].deleted_entries.is_empty();
        if ui
            .add_enabled(
                has_deleted,
                egui::Button::new(format!(
                    "{}\t{}",
                    app.strings.gui_undo_delete_request, app.strings.gui_shortcut_undo_delete
                )),
            )
            .clicked()
        {
            app.undo_delete_request();
            ui.close();
        }
    });
}

/// The File menu. Grouped into submenus by *verb* (New / Open / Save) rather
/// than one flat list, because the local and Git variants of each had grown to
/// a dozen sibling entries where "open" and "save" items were interleaved.
///
/// The File menu, shaped the way the terminal's is: **what** first, **where**
/// second. "Open ▸ Workspace ▸ From a Postman account…" says what that import
/// produces, where a bare "From Postman…" sitting among the open commands left
/// the user to guess; and the same submenu is where "Local folder…" and "From
/// Git…" live, so the sources for a thing are listed together instead of being
/// spread over three top-level entries.
///
/// Import is the exception, and sits at the top level: it is the word someone
/// arriving from Postman scans for, and reaching it by way of "Open ▸
/// Workspace" asks them to know what PaperBoy will make of their export before
/// they have made it.
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

        // Submenus close only themselves, leaving the File menu hanging open
        // over the app; every leaf sets this so the whole menu goes with it.
        let mut close_menu = false;

        // Import is its own top-level entry rather than a leaf three levels
        // down under Open ▸ Workspace: "import" is the word someone arriving
        // from Postman looks for, and they will not find it by first deciding
        // that what they want is a workspace. Its two routes are named after
        // what the user has — a file they exported, or an account to connect
        // to — because that, not the transport, is what they can answer.
        ui.menu_button(app.strings.gui_menu_import, |ui| {
            if ui
                .button(app.strings.gui_menu_import_file)
                .on_hover_text(app.strings.help_menu_import_file)
                .clicked()
            {
                open_via_picker(app, OpenKind::PostmanExport);
                close_menu = true;
                ui.close();
            }
            if ui
                .button(app.strings.gui_menu_import_account)
                .on_hover_text(app.strings.help_menu_import_account)
                .clicked()
            {
                app.postman.open();
                close_menu = true;
                ui.close();
            }
        });
        ui.separator();
        ui.menu_button(app.strings.gui_menu_open, |ui| {
            ui.menu_button(app.strings.file_kind_collection, |ui| {
                if ui.button(app.strings.gui_menu_from_file).clicked() {
                    open_via_picker(app, OpenKind::Collection);
                    close_menu = true;
                    ui.close();
                }
                if ui.button(app.strings.gui_menu_from_git).clicked() {
                    app.remote.open_load();
                    close_menu = true;
                    ui.close();
                }
            });
            ui.menu_button(app.strings.file_kind_environment, |ui| {
                if ui.button(app.strings.gui_menu_from_file).clicked() {
                    open_via_picker(app, OpenKind::Environment);
                    close_menu = true;
                    ui.close();
                }
                // The same flow as a collection: which of the two arrived is
                // read off the file that was picked, not asked up front.
                if ui.button(app.strings.gui_menu_from_git).clicked() {
                    app.remote.open_load();
                    close_menu = true;
                    ui.close();
                }
            });
            ui.menu_button(app.strings.file_kind_report, |ui| {
                if ui.button(app.strings.gui_menu_from_file).clicked() {
                    open_via_picker(app, OpenKind::Report);
                    close_menu = true;
                    ui.close();
                }
                if ui.button(app.strings.gui_menu_from_git).clicked() {
                    app.remote.open_load_report();
                    close_menu = true;
                    ui.close();
                }
            });
            ui.menu_button(app.strings.file_kind_workspace, |ui| {
                if ui.button(app.strings.gui_menu_from_folder).clicked() {
                    open_via_picker(app, OpenKind::Workspace);
                    close_menu = true;
                    ui.close();
                }
                if ui.button(app.strings.gui_menu_from_git).clicked() {
                    app.remote.open_load_workspace();
                    close_menu = true;
                    ui.close();
                }
                // A Postman import produces a whole folder of collections and
                // environments — a workspace — so this is where it belongs.
                if ui.button(app.strings.gui_menu_from_postman).clicked() {
                    app.postman.open();
                    close_menu = true;
                    ui.close();
                }
            });
        });
        ui.separator();

        // Save writes straight to the file the thing came from; Save As always
        // asks. Keeping them apart is the point: the menu used to offer only
        // the asking kind, so saving a report you had just edited meant walking
        // through a file dialog to name the file it already had.
        let save_hint = if save_active_has_path(app) {
            app.strings.help_menu_save
        } else {
            app.strings.help_menu_save_unsaved
        };
        if ui
            .button(format!(
                "{}\t{}",
                app.strings.gui_menu_save, app.strings.gui_shortcut_save
            ))
            .on_hover_text(save_hint)
            .clicked()
        {
            save_active(app);
            close_menu = true;
            ui.close();
        }
        ui.menu_button(
            format!(
                "{}\t{}",
                app.strings.gui_menu_save_as, app.strings.gui_shortcut_save_as
            ),
            |ui| {
                ui.menu_button(app.strings.file_kind_collection, |ui| {
                    if ui.button(app.strings.gui_menu_to_file).clicked() {
                        save_via_picker(app, SaveKind::Collection);
                        close_menu = true;
                        ui.close();
                    }
                    if ui.button(app.strings.gui_menu_to_git).clicked() {
                        app.remote.open_save_collection(app.active_ci());
                        close_menu = true;
                        ui.close();
                    }
                });
                // Only offered where they can work: a report save needs a report in
                // the editor, and a workspace push needs a tab that came from git.
                let has_report = app.report_editor.is_some();
                ui.menu_button(app.strings.file_kind_report, |ui| {
                    if ui
                        .add_enabled(has_report, egui::Button::new(app.strings.gui_menu_to_file))
                        .clicked()
                    {
                        save_via_picker(app, SaveKind::Report);
                        close_menu = true;
                        ui.close();
                    }
                    if ui
                        .add_enabled(has_report, egui::Button::new(app.strings.gui_menu_to_git))
                        .clicked()
                    {
                        app.remote.open_save_report();
                        close_menu = true;
                        ui.close();
                    }
                });
                let ci = app.active_ci();
                let is_ws = app
                    .session
                    .collections
                    .get(ci)
                    .is_some_and(|c| c.workspace_git_origin.is_some());
                ui.menu_button(app.strings.file_kind_workspace, |ui| {
                    if ui
                        .add_enabled(is_ws, egui::Button::new(app.strings.gui_menu_to_git))
                        .clicked()
                    {
                        app.remote.open_save_workspace(ci);
                        close_menu = true;
                        ui.close();
                    }
                });
                ui.menu_button(app.strings.gui_menu_kind_response, |ui| {
                    if ui.button(app.strings.gui_menu_to_file).clicked() {
                        save_via_picker(app, SaveKind::Response);
                        close_menu = true;
                        ui.close();
                    }
                });
            },
        );
        ui.separator();

        if ui.button(app.strings.gui_set_base_url).clicked() {
            app.dialog = Some(Dialog::Prompt {
                kind: PromptKind::BaseUrl,
                text: app.session.vars.base_url.clone(),
            });
            ui.close();
        }
        ui.separator();

        if ui
            .button(format!(
                "{}\t{}",
                app.strings.gui_close_tab, app.strings.gui_shortcut_close_tab_key
            ))
            .clicked()
        {
            app.request_close_tab(app.active_ci());
            ui.close();
        }
        if ui.button(app.strings.gui_quit).clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }

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
        ui.colored_label(app.theme.dim, app.strings.gui_preferences);
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
                &mut app.session.confirm_on_delete_request,
                app.strings.gui_confirm_delete_request,
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
        Dialog::ExtractParameter {
            ci,
            entry,
            target,
            value,
            range,
            name,
        } => extract_parameter_dialog(app, ctx, ci, entry, target, value, range, name),
        Dialog::Prompt { kind, text } => prompt_dialog(app, ctx, kind, text),
        Dialog::Theme(state) => theme_dialog(app, ctx, *state),
        Dialog::CloseGitWorkspace { ci, root } => close_git_workspace_dialog(app, ctx, ci, root),
        Dialog::UnsavedQuit { count, tabs } => unsaved_quit_dialog(app, ctx, count, tabs),
        Dialog::UnsavedCloseTab { ci, name, count } => {
            unsaved_close_tab_dialog(app, ctx, ci, name, count)
        }
        Dialog::WorkspaceReload { ci, reload } => workspace_reload_dialog(app, ctx, ci, *reload),
        Dialog::ExportResults { path } => export_results_dialog(app, ctx, path),
        Dialog::RevertToSaved {
            ci,
            path,
            entry,
            name,
        } => revert_to_saved_dialog(app, ctx, ci, path, entry, name),
        Dialog::ConfirmDeleteRequest { ci, idx, name } => {
            confirm_delete_request_dialog(app, ctx, ci, idx, name)
        }
        Dialog::DeleteWorkspaceItem {
            ci,
            path,
            is_dir,
            name,
            file_count,
            unsaved,
        } => confirm_delete_workspace_item_dialog(
            app, ctx, ci, path, is_dir, name, file_count, unsaved,
        ),
        Dialog::ConfirmRunAll { ci, total, non_get } => {
            confirm_run_all_dialog(app, ctx, ci, total, non_get)
        }
        Dialog::Shortcuts => shortcuts_dialog(app, ctx),
    }
}

/// Confirm deleting a request. Gated on `confirm_on_delete_request`; the delete
/// records an undo step, so cancelling re-arms the dialog (a click outside must
/// not be able to lose the request either) while confirming removes it and
/// leaves it recoverable via Undo Delete Request / Ctrl+Z.
fn confirm_delete_request_dialog(
    app: &mut GuiApp,
    ctx: &egui::Context,
    ci: usize,
    idx: usize,
    name: String,
) {
    let title = app.strings.gui_delete_request_title;
    let (go, cancel, question) = (
        app.strings.gui_delete,
        app.strings.gui_cancel,
        app.strings.confirm_delete_request_q.replace("{r}", &name),
    );
    let mut decided = false;
    let dismissed = modal(ctx, title, |ui| {
        ui.colored_label(app.theme.text, question);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(go).clicked() {
                app.delete_request_now(ci, idx);
                decided = true;
            }
            if ui.button(cancel).clicked() {
                decided = true;
            }
        });
    })
    .dismissed;
    decided |= dismissed;
    if !decided {
        app.dialog = Some(Dialog::ConfirmDeleteRequest { ci, idx, name });
    }
}

/// Confirm deleting a workspace file or folder from disk.
///
/// Unlike the request delete above, this is *not* gated on the
/// `confirm_on_delete_request` preference and can never be turned off: that
/// preference guards an undoable in-memory delete, whereas this removes a file
/// — or a whole folder's worth of them — from disk with no undo, so it must
/// always ask. The prompt says what is about to go: a folder's file count so
/// the size of the loss is visible, and a warning when there are unsaved edits
/// under the item that the delete would take with it. Cancelling (or dismissing)
/// re-arms the dialog so a stray click outside can't delete by default;
/// confirming performs the delete and its fix-up.
#[allow(clippy::too_many_arguments)]
fn confirm_delete_workspace_item_dialog(
    app: &mut GuiApp,
    ctx: &egui::Context,
    ci: usize,
    path: std::path::PathBuf,
    is_dir: bool,
    name: String,
    file_count: usize,
    unsaved: bool,
) {
    let title = app.strings.gui_ws_delete_title;
    let go = app.strings.gui_delete;
    let cancel = app.strings.gui_cancel;
    let question = if is_dir {
        app.strings
            .confirm_delete_ws_folder_q
            .replace("{name}", &name)
            .replace("{n}", &file_count.to_string())
    } else {
        app.strings
            .confirm_delete_ws_file_q
            .replace("{name}", &name)
    };
    let unsaved_note = unsaved.then_some(app.strings.confirm_delete_ws_unsaved);
    let mut decided = false;
    let dismissed = modal(ctx, title, |ui| {
        ui.colored_label(app.theme.text, question);
        if let Some(note) = unsaved_note {
            ui.add_space(4.0);
            ui.colored_label(app.theme.err, note);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(go).clicked() {
                super::requests::delete_workspace_item(app, ci, &path);
                decided = true;
            }
            if ui.button(cancel).clicked() {
                decided = true;
            }
        });
    })
    .dismissed;
    decided |= dismissed;
    if !decided {
        app.dialog = Some(Dialog::DeleteWorkspaceItem {
            ci,
            path,
            is_dir,
            name,
            file_count,
            unsaved,
        });
    }
}

/// Confirm a "Run All" that would fire non-GET requests. The count of how many
/// will run — and how many of those are not GET — is the whole point: it turns
/// "run everything" from a leap into an informed choice, so a collection full
/// of writes isn't one stray click from executing. Cancelling re-arms nothing
/// (there is nothing to lose by dismissing); confirming runs the collection.
fn confirm_run_all_dialog(
    app: &mut GuiApp,
    ctx: &egui::Context,
    ci: usize,
    total: usize,
    non_get: usize,
) {
    let title = app.strings.gui_run_all_confirm_title;
    let (go, cancel, question) = (
        app.strings.gui_run_all,
        app.strings.gui_cancel,
        app.strings
            .confirm_run_all_q
            .replace("{n}", &total.to_string())
            .replace("{m}", &non_get.to_string()),
    );
    let mut answered: Option<bool> = None;
    let dismissed = modal(ctx, title, |ui| {
        ui.colored_label(app.theme.text, question);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(go).clicked() {
                answered = Some(true);
            }
            if ui.button(cancel).clicked() {
                answered = Some(false);
            }
        });
    })
    .dismissed;
    if dismissed {
        answered = Some(false);
    }
    match answered {
        Some(true) => {
            app.session.run_all_entries(ci);
        }
        Some(false) => {}
        // No answer yet (egui drew nothing, or the user hasn't clicked): keep
        // the dialog armed rather than deciding for them.
        None => app.dialog = Some(Dialog::ConfirmRunAll { ci, total, non_get }),
    }
}

/// The F1 keyboard-shortcuts overlay: every GUI shortcut, grouped, with a short
/// description. Built from [`super::shortcut_help_sections`] so it can never
/// drift from the keys the app actually binds, and dismissed with Escape or the
/// close button like every other modal.
fn shortcuts_dialog(app: &mut GuiApp, ctx: &egui::Context) {
    let title = app.strings.gui_shortcuts_title;
    let sections = super::shortcut_help_sections(&app.strings);
    let theme = app.theme;
    let close_lbl = app.strings.gui_close;
    let mut close = false;
    // Sized to its content rather than resizable. This is a reference card, not
    // a workspace: there is nothing in it worth dragging bigger, and being
    // resizable was what let it grow. `egui` remembers a resizable window's
    // size across runs, so leaving it resizable would also have left anyone who
    // had already seen the oversized version still looking at it.
    let list_max_h = (ctx.input(|i| i.content_rect()).height() * 0.65).max(200.0);
    let dismissed = super::widgets::dialog(ctx, title, Some(420.0), |ui| {
        egui::ScrollArea::vertical()
            // Shrink to the content vertically, but not horizontally: the grid
            // wants the full dialog width so its two columns line up down the
            // whole overlay, while its height should be the height of the list.
            // `false` on this axis made the scroll area claim every pixel it
            // could be given, so the dialog filled the screen with most of it
            // blank and ran its heading and Close button off both ends at once.
            .auto_shrink([false, true])
            // ...and a ceiling, so a list longer than the screen scrolls inside
            // the dialog instead of pushing Close out of the bottom of it.
            .max_height(list_max_h)
            .show(ui, |ui| {
                for (i, section) in sections.iter().enumerate() {
                    if i > 0 {
                        ui.add_space(8.0);
                    }
                    ui.colored_label(theme.accent, section.title);
                    ui.add_space(2.0);
                    egui::Grid::new(("shortcuts_grid", i))
                        .num_columns(2)
                        .spacing([18.0, 4.0])
                        .show(ui, |ui| {
                            for (keys, desc) in &section.rows {
                                ui.colored_label(theme.text, egui::RichText::new(*keys).strong());
                                ui.colored_label(theme.dim, *desc);
                                ui.end_row();
                            }
                        });
                }
            });
        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(close_lbl).clicked() {
                close = true;
            }
        });
    })
    .dismissed;
    // Not re-armed on close: the overlay is a reference the user is done with,
    // not a question waiting on an answer.
    if !(close || dismissed) {
        app.dialog = Some(Dialog::Shortcuts);
    }
}

/// Confirm discarding a request's (or a whole workspace file's) in-memory edits
/// in favour of what is on disk. Cancelling re-arms the dialog, like every
/// other destructive confirmation here, so a click outside can't lose an edit.
fn revert_to_saved_dialog(
    app: &mut GuiApp,
    ctx: &egui::Context,
    ci: usize,
    path: std::path::PathBuf,
    entry: Option<usize>,
    name: String,
) {
    let title = app.strings.gui_revert_title;
    let (go, cancel, question) = (
        app.strings.gui_revert_go,
        app.strings.gui_cancel,
        match entry {
            Some(_) => app.strings.confirm_revert_request_q.replace("{r}", &name),
            None => app.strings.confirm_revert_file_q.replace("{f}", &name),
        },
    );
    let mut decided = false;
    let dismissed = modal(ctx, title, |ui| {
        ui.colored_label(app.theme.text, question);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(go).clicked() {
                match entry {
                    Some(ei) => {
                        app.session.collections[ci].revert_request(ei);
                    }
                    None => {
                        let _ = app.session.collections[ci].revert_workspace_file(&path);
                    }
                }
                decided = true;
            }
            if ui.button(cancel).clicked() {
                decided = true;
            }
        });
    })
    .dismissed;
    decided |= dismissed;
    if !decided {
        app.dialog = Some(Dialog::RevertToSaved {
            ci,
            path,
            entry,
            name,
        });
    } else {
        app.session.save();
    }
}

/// Pick the file *and* the format a report's results are exported as.
///
/// The format lives beside the name because it *is* the name: every writer is
/// chosen by extension (see [`crate::report::writer::writer_for_extension`]),
/// so choosing Excel has to make the file end `.xlsx` or the choice is a lie.
/// The native picker's filter dropdown could not do that — it filters what the
/// dialog lists and nothing more — so it sat in the far corner appearing to do
/// nothing. Browse… still hands off to the native picker for people who want to
/// go looking for a folder.
fn export_results_dialog(app: &mut GuiApp, ctx: &egui::Context, mut path: String) {
    let title = app.strings.gui_save_results_title;
    let (lbl_export, lbl_cancel, lbl_browse, lbl_format) = (
        app.strings.gui_report_export_go,
        app.strings.gui_cancel,
        app.strings.gui_browse,
        app.strings.gui_report_export_format,
    );
    let mut act = ExportChoice::Keep;
    let theme_dim = app.theme.dim;
    let answered = modal(ctx, title, |ui| {
        ui.horizontal(|ui| {
            ui.colored_label(theme_dim, lbl_format);
            let current = std::path::Path::new(&path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            egui::ComboBox::from_id_salt("pb_export_format")
                .selected_text(format_label(&current))
                .show_ui(ui, |ui| {
                    for ext in crate::report::writer::OUTPUT_EXTENSIONS {
                        if ui
                            .selectable_label(ext == current, format_label(ext))
                            .clicked()
                            && ext != current
                        {
                            path = retarget_extension(&path, ext);
                        }
                    }
                });
        });
        let resp = ui.add(
            egui::TextEdit::singleline(&mut path)
                .desired_width(420.0)
                .hint_text(".csv / .json / .html / .xlsx"),
        );
        let entered = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.horizontal(|ui| {
            if ui.button(lbl_browse).clicked() {
                act = ExportChoice::Browse;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(lbl_cancel).clicked() {
                    act = ExportChoice::Cancel;
                }
                if ui.button(lbl_export).clicked() || entered {
                    act = ExportChoice::Write;
                }
            });
        });
    });
    // A frame egui never drew is not an answer: keep the dialog armed.
    if answered.inner.is_none() {
        act = ExportChoice::Keep;
    }
    if answered.dismissed {
        act = ExportChoice::Cancel;
    }
    match act {
        ExportChoice::Keep => app.dialog = Some(Dialog::ExportResults { path }),
        ExportChoice::Cancel => {}
        ExportChoice::Browse => save_via_picker(app, SaveKind::ReportResults),
        ExportChoice::Write => {
            match export_report_results(app, &path) {
                Ok(()) => {
                    app.session.status = Some(crate::i18n::Status::Saved);
                }
                // A failed export leaves the dialog open with the name still in
                // it: the fix is nearly always a word in that name.
                Err(e) => {
                    app.session.status = Some(crate::i18n::Status::Error(e));
                    app.dialog = Some(Dialog::ExportResults { path });
                }
            }
        }
    }
}

/// What [`export_results_dialog`] decided this frame.
enum ExportChoice {
    /// No answer yet — re-arm the dialog unchanged.
    Keep,
    Cancel,
    /// Hand over to the native save picker.
    Browse,
    Write,
}

/// The display name for an export format, from its extension.
fn format_label(ext: &str) -> &'static str {
    match ext {
        "json" => "JSON",
        "html" | "htm" => "HTML",
        "xlsx" => "Excel",
        "pdf" => "PDF",
        "csv" => "CSV",
        // An unknown extension is shown as itself rather than silently
        // corrected: the name in the box is the user's, and the Export button
        // is what tells them the format isn't one PaperBoy writes.
        _ => "—",
    }
}

/// Rewrite `path`'s extension to `ext`, keeping everything else.
///
/// A path with no extension gains one rather than being left alone — a name
/// typed as `results` still has to become `results.csv` for a writer to be
/// found for it.
fn retarget_extension(path: &str, ext: &str) -> String {
    let p = std::path::Path::new(path);
    if p.as_os_str().is_empty() {
        return format!("results.{ext}");
    }
    p.with_extension(ext).to_string_lossy().into_owned()
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
    let decided = modal(ctx, title, |ui| {
        ui.colored_label(app.theme.text, question);
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
    })
    .dismissed
        || decided;
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
    let dismissed = modal(ctx, title, |ui| {
        ui.colored_label(app.theme.text, question);
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
    })
    .dismissed;
    decided |= dismissed;
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
    let dismissed = modal(ctx, title, |ui| {
        ui.colored_label(app.theme.text, question);
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
    })
    .dismissed;
    decided |= dismissed;
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
    let dismissed = modal(ctx, title, |ui| {
        ui.colored_label(app.theme.text, question);
        ui.add_space(4.0);
        ui.colored_label(app.theme.dim, hint);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(yes).clicked() {
                answer = Some(true);
            }
            if ui.button(no).clicked() {
                answer = Some(false);
            }
        });
    })
    .dismissed;
    // Dismissing is declining: the offer is recorded on the tab, so it comes
    // back next launch rather than being lost.
    if dismissed {
        answer = Some(false);
    }
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
/// [`DialogFrame::inner`] is `None` when egui declined to show the window at
/// all. Callers treat that as "no answer this frame" and leave the dialog
/// armed rather than deciding for the user — a render path must never panic,
/// and an unanswered dialog simply reappears. `dismissed` is the ✕ or Escape,
/// which every dialog runs as its Cancel.
fn modal<R>(
    ctx: &egui::Context,
    title: &str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> super::widgets::DialogFrame<R> {
    super::widgets::dialog(ctx, title, None, add)
}

/// What an open dialog is for, once it answers. See [`PickAction`].
fn open_title(app: &GuiApp, kind: OpenKind) -> &'static str {
    match kind {
        OpenKind::Collection => app.strings.gui_open_collection_title,
        OpenKind::Environment => app.strings.gui_open_environment_title,
        OpenKind::Workspace => app.strings.gui_open_workspace_title,
        OpenKind::Report => app.strings.gui_open_report_title,
        OpenKind::PostmanExport => app.strings.postman_export_open_title,
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
        OpenKind::PostmanExport => super::filepick::PickKind::File {
            filters: filters(&[
                (app.strings.gui_filter_postman_export, &["json"]),
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
    /// A `FOLDER`/`FILE` parameter's path, chosen from the run settings.
    ReportParamPath {
        name: String,
    },
    /// The default value of a `FOLDER`/`FILE` `PARAM`, picked from its block.
    ReportParamDefault {
        path: Vec<usize>,
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
    // Every picker teaches the next one where the user just was. The report
    // editor's own dialogs used to be left out of this, so browsing to a corpus
    // folder was forgotten and the next Browse… opened back at the workspace
    // root — the one complaint this is here to fix. `Other` deliberately seeds
    // only the general last-browsed folder, leaving the environment/import
    // memories to the pickers those are actually about.
    if let Some(path) = picked.as_ref() {
        app.session
            .remember_picker_dir(crate::session::PickerKind::Other, path);
    }
    // The report editor's own pickers write into the open editor rather than
    // loading a file, so they have no error to report.
    match action {
        PickAction::ReportParamPath { name } => {
            if let Some(path) = picked.as_ref()
                && let Some(ed) = app.report_editor.as_mut()
            {
                ed.param_values
                    .insert(name, path.to_string_lossy().into_owned());
            }
            return;
        }
        PickAction::ReportHeaderFile { key, occurrence } => {
            super::report_editor::apply_picked_header_file(app, key, occurrence, picked);
            return;
        }
        PickAction::ReportParamDefault { path } => {
            super::report_editor::apply_picked_param_default(app, &path, picked);
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
    // An export is sorted by what it holds, not by what the user said it was:
    // Postman writes collections and environments to the same `.json`, and
    // making the user know which they picked is exactly the knowledge they
    // came here without.
    let kind = match kind {
        OpenKind::PostmanExport => match crate::postman::export_kind(&content) {
            Some(crate::postman::ExportKind::Collection) => OpenKind::Collection,
            Some(crate::postman::ExportKind::Environment) => OpenKind::Environment,
            None => return Err(app.strings.gui_not_postman_export.to_string()),
        },
        other => other,
    };
    let ok = match kind {
        OpenKind::Collection => {
            app.session
                .load_collection_text(name, &content, Some(path.to_path_buf()))
        }
        OpenKind::Environment => app
            .session
            .load_environment_text(name, &content, Some(path.to_path_buf()), None)
            .is_some(),
        OpenKind::Workspace | OpenKind::Report | OpenKind::PostmanExport => unreachable!(),
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
    const ALL: [super::filepick::Filter<'static>; 5] = [
        ("CSV", &["csv"]),
        ("JSON", &["json"]),
        ("HTML", &["html"]),
        ("Excel", &["xlsx"]),
        ("PDF", &["pdf"]),
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

/// What "Save" (the File menu entry and Ctrl+S) writes: whatever is in front of
/// the user.
///
/// The open report editor wins when there is one, since it is drawn over the
/// request view and is the thing being edited; otherwise it is the active
/// collection tab. Reported as a `SaveKind` so the fallback to a picker, when
/// the target has never been written anywhere, needs no second decision about
/// what is being saved.
pub fn active_save_kind(app: &GuiApp) -> SaveKind {
    if app.report_editor.is_some() {
        SaveKind::Report
    } else {
        SaveKind::Collection
    }
}

/// Save what is in front of the user, straight to the file it came from.
///
/// Falls back to the Save As picker when that thing has never been written
/// anywhere -- there is no path to save to, and refusing outright would leave
/// Ctrl+S doing nothing on exactly the documents most likely to need saving.
/// An item that *does* know its file is written without a dialog: a save
/// shortcut that always asks where is no shortcut.
pub fn save_active(app: &mut GuiApp) {
    match active_save_kind(app) {
        SaveKind::Report => match app.report_editor.as_mut() {
            Some(ed) if ed.report.path.is_some() => {
                super::report_editor::request_save(ed);
            }
            _ => save_via_picker(app, SaveKind::Report),
        },
        _ => {
            let ci = app.active_ci();
            let Some(path) = app.session.collections.get(ci).and_then(|c| c.path.clone()) else {
                save_via_picker(app, SaveKind::Collection);
                return;
            };
            let text = app.session.collections[ci].to_hurl();
            match std::fs::write(&path, text) {
                Ok(()) => {
                    app.session.collections[ci].mark_saved();
                    app.session.save();
                    app.session.status = Some(crate::i18n::Status::Saved);
                }
                Err(e) => {
                    app.session.status = Some(crate::i18n::Status::Error(format!(
                        "{} {e}",
                        app.strings.gui_could_not_write
                    )));
                }
            }
        }
    }
}

/// Whether [`save_active`] would write without asking -- i.e. whether the thing
/// in front of the user already has a file. Drives the Save entry's hint, so
/// the menu can say which of the two it is about to do.
pub fn save_active_has_path(app: &GuiApp) -> bool {
    match active_save_kind(app) {
        SaveKind::Report => app
            .report_editor
            .as_ref()
            .is_some_and(|e| e.report.path.is_some()),
        _ => app
            .session
            .collections
            .get(app.active_ci())
            .is_some_and(|c| c.path.is_some()),
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
        // Remembered so the toolbar can offer to open it: an HTML export is
        // written to be read in a browser, and hunting for the file you just
        // named is the only step between the two.
        ed.last_export = Some(path.to_string());
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
    let frame = modal(ctx, title, |ui| {
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
    // A frame egui never drew is not an answer: keep the dialog open;
    // dismissing it is the Cancel button.
    let dismissed = frame.dismissed;
    let (keep, submit) = frame.inner_or((true, false));
    let keep = keep && !dismissed;
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
                // A workspace file/folder is renamed on disk (and everything
                // holding its old path repointed), rather than by editing an
                // in-memory title — see `rename_workspace_item`.
                RenameTarget::WorkspaceItem { ci, path } => {
                    super::requests::rename_workspace_item(app, ci, &path, text.trim());
                }
            }
            app.session.save();
        }
        return;
    }
    app.dialog = Some(Dialog::Rename { target, text });
}

/// Name the parameter a request field is being extracted into.
///
/// The name is re-checked against the request every frame rather than once on
/// submit, so the refusal appears as the offending name is typed; and the OK
/// button is a no-op while it stands, so Enter can't smuggle a refused name
/// past the check.
#[allow(clippy::too_many_arguments)]
fn extract_parameter_dialog(
    app: &mut GuiApp,
    ctx: &egui::Context,
    ci: usize,
    entry: usize,
    target: super::editor::ExtractTarget,
    value: String,
    range: Option<std::ops::Range<usize>>,
    mut name: String,
) {
    let title = app.strings.extract_title;
    let lbl_value = app.strings.extract_value;
    let lbl_name = app.strings.extract_name_label;
    let lbl_ok = app.strings.gui_ok;
    let lbl_cancel = app.strings.gui_cancel;
    let dim = app.theme.dim;
    let err_color = app.theme.err;
    let declared = app
        .session
        .collections
        .get(ci)
        .and_then(|c| c.entries.get(entry))
        .map(|e| e.variable_defaults())
        .unwrap_or_default();
    let msg_invalid = app.strings.extract_name_invalid;
    let msg_conflict = app.strings.extract_name_conflict;
    let frame = modal(ctx, title, |ui| {
        ui.label(egui::RichText::new(crate::i18n::fill(lbl_value, &[&value])).color(dim));
        ui.add_space(4.0);
        ui.label(egui::RichText::new(lbl_name).color(dim));
        let resp = ui.add(
            egui::TextEdit::singleline(&mut name)
                .desired_width(320.0)
                .hint_text(lbl_name),
        );
        let error = crate::hurl::check_parameter_name(&name, &value, &declared);
        match &error {
            Some(crate::hurl::ParamNameError::Invalid) => {
                ui.label(egui::RichText::new(msg_invalid).color(err_color));
            }
            Some(crate::hurl::ParamNameError::Conflict(existing)) => {
                ui.label(
                    egui::RichText::new(crate::i18n::fill(msg_conflict, &[&name, existing]))
                        .color(err_color),
                );
            }
            None => {}
        }
        let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let mut keep = true;
        let mut go = submit;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(error.is_none(), egui::Button::new(lbl_ok))
                .clicked()
            {
                go = true;
            }
            if ui.button(lbl_cancel).clicked() {
                keep = false;
            }
        });
        (keep, go && error.is_none())
    });
    // A frame egui never drew is not an answer: keep the dialog open;
    // dismissing it is the Cancel button.
    let dismissed = frame.dismissed;
    let (keep, submit) = frame.inner_or((true, false));
    let keep = keep && !dismissed;
    if !keep {
        return;
    }
    if submit {
        super::editor::apply_extract_parameter(app, ci, entry, target, range, &value, &name);
        app.session.save();
        return;
    }
    app.dialog = Some(Dialog::ExtractParameter {
        ci,
        entry,
        target,
        value,
        range,
        name,
    });
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
    let frame = modal(ctx, title, |ui| {
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
    // A frame egui never drew is not an answer: keep the dialog open;
    // dismissing it is the Cancel button.
    let dismissed = frame.dismissed;
    let (keep, submit) = frame.inner_or((true, false));
    let keep = keep && !dismissed;
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
    let dim = app.theme.dim;
    let frame = modal(ctx, title, |ui| {
        ui.horizontal(|ui| {
            ui.colored_label(dim, lbl_name);
            ui.text_edit_singleline(&mut state.spec.name);
        });
        ui.separator();
        egui::Grid::new("theme_colors")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                for i in 0..THEME_COLOR_COUNT {
                    let label = color_label(&strings, i);
                    ui.colored_label(dim, label);
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
    // A frame egui never drew is not an answer: keep the dialog open;
    // dismissing it is the Cancel button.
    let action = if frame.dismissed {
        Action::Cancel
    } else {
        frame.inner_or(Action::Keep)
    };

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
use crate::theme::color_label;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;
    use crate::session::Session;

    fn app() -> GuiApp {
        GuiApp::for_test(Session::default())
    }

    /// Every picker leaves a trail: after the report editor's own parameter
    /// dialog lands somewhere, the next dialog that has no seed of its own
    /// opens there rather than back at square one.
    #[test]
    fn a_report_editors_picker_teaches_the_next_one_where_the_user_was() {
        let dir = std::env::temp_dir().join(format!("pb_pick_memory_{}", std::process::id()));
        let corpus = dir.join("Face");
        std::fs::create_dir_all(&corpus).unwrap();

        let mut app = app();
        assert!(
            app.session
                .picker_dir(crate::session::PickerKind::Other)
                .is_none(),
            "nothing browsed yet"
        );
        app.pending_pick = Some(super::super::filepick::resolved(
            PickAction::ReportParamPath {
                name: "CORPUS".to_string(),
            },
            Some(corpus.clone()),
        ));
        poll_pending_pick(&mut app);

        assert_eq!(
            app.session.picker_dir(crate::session::PickerKind::Other),
            Some(corpus.as_path()),
            "the folder just picked is where the next picker starts"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The desktop import route asks for "a file you exported from Postman"
    /// and works out for itself which of PaperBoy's two shelves it belongs on —
    /// Postman gives collections and environments the same `.json` extension,
    /// and knowing which is which is exactly what a newcomer has not learned
    /// yet.
    #[test]
    fn an_exported_postman_file_opens_as_whichever_kind_it_turns_out_to_be() {
        let dir = std::env::temp_dir().join(format!("pb_gui_import_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let collection = dir.join("api.json");
        std::fs::write(
            &collection,
            r#"{"info":{"name":"Api","schema":"x"},"item":[{"name":"Health","request":{"method":"GET","url":"http://127.0.0.1:8080/health"}}]}"#,
        )
        .unwrap();
        let environment = dir.join("staging.json");
        std::fs::write(
            &environment,
            r#"{"name":"Staging","values":[{"key":"BASE","value":"http://x","enabled":true}]}"#,
        )
        .unwrap();
        let other = dir.join("notes.json");
        std::fs::write(&other, r#"{"hello":"world"}"#).unwrap();

        let mut app = app();
        let tabs = app.session.collections.len();
        let envs = app.session.global_envs.len();

        assert!(apply_open(&mut app, OpenKind::PostmanExport, &collection).is_ok());
        assert_eq!(app.session.collections.len(), tabs + 1);
        assert_eq!(app.session.global_envs.len(), envs);

        assert!(apply_open(&mut app, OpenKind::PostmanExport, &environment).is_ok());
        assert_eq!(app.session.global_envs.len(), envs + 1);
        assert_eq!(app.session.collections.len(), tabs + 1);

        // And a `.json` that is neither says so in those terms, rather than
        // "could not parse that file".
        let err = apply_open(&mut app, OpenKind::PostmanExport, &other).unwrap_err();
        assert_eq!(err, app.strings.gui_not_postman_export);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An export is remembered so the toolbar can offer to open it: the file an
    /// HTML export writes is meant to be read in a browser, and the path is the
    /// only thing standing between the two.
    #[test]
    fn an_export_remembers_the_file_it_wrote_so_it_can_be_opened() {
        use crate::report::model::{ReportResult, ReportRow};

        let mut app = app();
        let mut ed = crate::gui::report_editor::ReportEditor::new(
            crate::gui::report_editor::ReportOrigin::Session(0),
            crate::report::Report::scratch("r"),
        );
        let mut result = ReportResult::default();
        result.column_order = vec!["Time".to_string()];
        result.rows.push(ReportRow {
            cells: [("Time".to_string(), "100".to_string())]
                .into_iter()
                .collect(),
            key: vec!["a".to_string()],
            ..Default::default()
        });
        ed.result = Some(result);
        assert!(
            ed.last_export.is_none(),
            "nothing exported, nothing to open"
        );
        app.report_editor = Some(ed);

        let dir = std::env::temp_dir().join(format!("pb_gui_export_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.csv");
        let done = export_report_results(&mut app, &path.to_string_lossy());
        std::fs::remove_dir_all(&dir).ok();

        assert!(done.is_ok(), "the export should succeed: {done:?}");
        assert_eq!(
            app.report_editor.as_ref().unwrap().last_export.as_deref(),
            Some(path.to_string_lossy().as_ref()),
            "and the file it wrote is the one Open would hand to the desktop"
        );
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

    /// Ctrl+S on a collection that already has a file writes it there and
    /// clears the unsaved marker -- no dialog, and nothing left to warn about.
    #[test]
    fn saving_a_collection_in_place_writes_it_and_clears_the_marker() {
        let mut app = app();
        let dir = std::env::temp_dir().join(format!("pb_save_active_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.hurl");
        std::fs::write(
            &path,
            "GET https://example.test
",
        )
        .unwrap();
        let ci = app.active_ci();
        app.session.collections[ci].path = Some(path.clone());

        assert!(
            save_active_has_path(&app),
            "a collection with a file saves without asking"
        );
        save_active(&mut app);
        // The fixture collection holds no requests, so what matters is that the
        // file was rewritten from it -- the seeded contents are gone.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert!(
            !on_disk.contains("example.test"),
            "the file was rewritten from the collection: {on_disk:?}"
        );
        assert!(
            matches!(app.session.status, Some(crate::i18n::Status::Saved)),
            "and it reported success rather than an error"
        );
        assert!(
            !app.session.collections[ci].has_unsaved_edits(),
            "and the collection no longer counts as edited"
        );
        assert!(
            app.pending_pick.is_none(),
            "saving in place must not open a file dialog"
        );
    }

    #[test]
    fn choosing_an_export_format_renames_the_file() {
        // The writer is chosen by extension, so a format dropdown that leaves
        // the name ending `.csv` has not chosen anything.
        assert_eq!(
            super::retarget_extension("/tmp/report results.csv", "xlsx"),
            "/tmp/report results.xlsx"
        );
        // A name with dots in it keeps them: only the last segment is the
        // format.
        assert_eq!(
            super::retarget_extension("/tmp/run_v4.3.csv", "html"),
            "/tmp/run_v4.3.html"
        );
        // A bare name gains an extension rather than staying unwritable.
        assert_eq!(super::retarget_extension("results", "json"), "results.json");
        assert_eq!(super::retarget_extension("", "csv"), "results.csv");
    }

    /// The report editor wins when one is open: it is drawn over the request
    /// view, so it is what "save" is about.
    #[test]
    fn the_open_report_is_what_save_saves() {
        let mut app = app();
        assert_eq!(active_save_kind(&app), SaveKind::Collection);
        app.report_editor = Some(crate::gui::report_editor::ReportEditor::new(
            crate::gui::report_editor::ReportOrigin::Session(0),
            crate::report::Report::scratch("r"),
        ));
        assert_eq!(active_save_kind(&app), SaveKind::Report);
        assert!(
            !save_active_has_path(&app),
            "a scratch report has no file, so Save has to ask where"
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
        for (ext, expected) in [
            ("xlsx", "Excel"),
            ("json", "JSON"),
            ("html", "HTML"),
            ("pdf", "PDF"),
        ] {
            let filters = report_result_filters(ext);
            assert_eq!(filters[0].0, expected, "{ext} should lead");
            assert_eq!(filters.len(), 5, "every format stays available");
        }
        // Case is irrelevant: `# output: XLSX` is the same directive.
        assert_eq!(report_result_filters("XLSX")[0].0, "Excel");
    }

    /// With no (or an unwritable) format declared, the list keeps its usual
    /// order, so CSV remains the default it has always been.
    #[test]
    fn an_unknown_export_format_leaves_csv_leading() {
        for ext in ["", "csv", "docx"] {
            let filters = report_result_filters(ext);
            assert_eq!(filters[0].0, "CSV", "{ext:?} should leave CSV leading");
            assert_eq!(
                filters.iter().map(|f| f.0).collect::<Vec<_>>(),
                vec!["CSV", "JSON", "HTML", "Excel", "PDF"]
            );
        }
    }

    /// Escape is the way out of any dialog. Without it a modal can only be
    /// left by finding the right button, which on a confirmation that re-arms
    /// itself (every destructive one here does) means there is no way out at
    /// all for someone who opened it by accident.
    /// The sheet a dialog draws over the app swallows clicks, so the keyboard
    /// has to stand down to match: Ctrl+S with a git wizard open would
    /// otherwise save whatever tab happens to be behind it.
    #[test]
    fn every_dialog_takes_the_keyboard_with_it_not_just_the_mouse() {
        let mut a = app();
        assert!(!a.dialog_is_open(), "nothing is open to begin with");

        a.dialog = Some(Dialog::Rename {
            target: RenameTarget::Tab { ci: 0 },
            text: String::new(),
        });
        assert!(a.dialog_is_open());
        a.dialog = None;

        a.remote.open_load();
        assert!(a.dialog_is_open(), "the git wizard counts too");
        a.remote = Default::default();

        a.postman.open();
        assert!(a.dialog_is_open(), "and the Postman importer");
    }

    #[test]
    fn escape_closes_a_dialog_the_way_its_cancel_button_would() {
        let ctx = egui::Context::default();
        let mut a = app();

        let armed = |a: &mut GuiApp| {
            a.dialog = Some(Dialog::Rename {
                target: RenameTarget::Tab { ci: 0 },
                text: "whatever".to_string(),
            });
        };
        let frame_with = |a: &mut GuiApp, input: egui::RawInput| {
            let mut input = input;
            input.screen_rect = Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(900.0, 600.0),
            ));
            let _ = ctx.run_ui(input, |ui| show_dialog(a, ui.ctx()));
        };

        // An ordinary frame leaves it open — the dialog re-arms itself.
        armed(&mut a);
        frame_with(&mut a, egui::RawInput::default());
        assert!(a.dialog.is_some(), "a quiet frame leaves the dialog open");

        let mut esc = egui::RawInput::default();
        esc.events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        frame_with(&mut a, esc);
        assert!(a.dialog.is_none(), "Escape closes it");
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
                s.gui_menu_edit_key,
                s.gui_menu_view_key,
                s.gui_menu_settings_key,
                s.gui_menu_help_key,
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
    fn the_mnemonic_is_underlined_without_waiting_for_alt() {
        let ctx = egui::Context::default();
        let underlined = |title: &str, m: char| {
            let mut found = Vec::new();
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                let text = menu_title(ui, title, m);
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
                    // A plain string carries no formatting at all, which is how
                    // the "mnemonic isn't in the title" fallback comes back.
                    _ => Vec::new(),
                };
            });
            found
        };
        // The underline is what advertises the mnemonic, so it is there from the
        // first frame rather than only after the user has already found Alt.
        assert_eq!(underlined("Settings", 'S'), vec!["S".to_string()]);
        // Matched case-insensitively, and under the letter as the title spells
        // it rather than an uppercased copy of it.
        assert_eq!(underlined("File", 'F'), vec!["F".to_string()]);
        assert_eq!(underlined("edit", 'E'), vec!["e".to_string()]);
        // A mnemonic that isn't in the translated title underlines nothing at
        // all, rather than guessing at a character.
        assert_eq!(underlined("Aide", 'H'), Vec::<String>::new());
    }

    #[test]
    fn a_multibyte_title_underlines_the_right_character() {
        // The byte index came from searching an uppercased copy of the title,
        // which is only the same string when uppercasing preserves every byte
        // length. The "\u{fb00}" ligature is three bytes and uppercases to the
        // two bytes "FF", so every index past it was off by one: searching the
        // copy finds 'E' at byte 5, which in the original is the hyphen.
        let ctx = egui::Context::default();
        let mut found = Vec::new();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let text = menu_title(ui, "A\u{fb00}x-End", 'E');
            if let egui::WidgetText::LayoutJob(job) = text {
                found = job
                    .sections
                    .iter()
                    .filter(|s| s.format.underline != egui::Stroke::NONE)
                    .map(|s| {
                        job.text[usize::from(s.byte_range.start)..usize::from(s.byte_range.end)]
                            .to_string()
                    })
                    .collect();
            }
        });
        assert_eq!(found, vec!["E".to_string()], "underlined the wrong glyph");
    }
}
