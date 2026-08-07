//! The GUI application: [`GuiApp`] wraps the shared [`Session`] with egui view
//! state and lays out the Postman-style panels every frame.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use eframe::egui::{self, Key, Modifiers};

use crate::i18n::Strings;
use crate::persistence::GuiView;
use crate::request::RequestView;
use crate::session::Session;

use super::report_editor::ReportOrigin;
use super::theme::GuiTheme;
use super::{
    Focus, editor, environments, menu, remote, report_editor, reports, requests, response,
};

/// Which section of the request editor (centre-top) is shown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditorSection {
    All,
    Params,
    Headers,
    Body,
    Auth,
    Cookies,
    Options,
    Asserts,
    Captures,
    Code,
}

/// Which section of the response viewer (centre-bottom) is shown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ResponseSection {
    Body,
    Headers,
    Asserts,
}

/// A modal dialog currently shown over the main UI.
pub enum Dialog {
    /// Rename a request or collection tab.
    Rename { target: RenameTarget, text: String },
    /// The theme editor.
    Theme(Box<super::menu::ThemeEditState>),
    /// Simple text prompt (base URL, new env name, …).
    Prompt { kind: PromptKind, text: String },
    /// Closing a Workspace tab whose folder PaperBoy downloaded from git into
    /// a throwaway directory: keep the folder (so the tab can be reopened
    /// later) or delete it now? Never shown for a folder the user picked
    /// themselves — the app must never delete one of those.
    CloseGitWorkspace { ci: usize, root: std::path::PathBuf },
    /// Quitting while some request edits exist only in memory. Confirming
    /// closes the window for real; cancelling leaves everything as it was.
    UnsavedQuit { count: usize, tabs: String },
    /// Closing a tab that is holding request edits with nowhere on disk to go.
    UnsavedCloseTab {
        ci: usize,
        name: String,
        count: usize,
    },
    /// A restored Workspace tab's downloaded folder has vanished since the
    /// last session (typically `/tmp` swept between restarts). Offers to
    /// redownload it, pinned to the exact commit it recorded.
    WorkspaceReload {
        ci: usize,
        reload: Box<crate::persistence::PendingWorkspaceReload>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OpenKind {
    Collection,
    Environment,
    /// Open a folder as a Workspace (a filesystem tree of collections /
    /// environments / reports), rather than a single file.
    Workspace,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SaveKind {
    Collection,
    /// Save the Global Environment with this id to a `.vars` file.
    Environment(u64),
    Response,
    /// Export the open report editor's last run results (format by extension).
    ReportResults,
}

#[derive(Clone)]
pub enum RenameTarget {
    Request { ci: usize, idx: usize },
    Tab { ci: usize },
}

// Not `Copy`: `NewWorkspaceFolder` carries the folder the new one goes inside.
#[derive(Clone, PartialEq, Eq)]
pub enum PromptKind {
    BaseUrl,
    NewEnvName,
    NewCollectionName,
    /// Name for a new subfolder in a Workspace tab's tree, with the tab and the
    /// folder it goes inside. Asked for in-app rather than through the
    /// platform's dialog, as the file kinds are: a save dialog is built around
    /// choosing a *file* name, and the folder pickers offer existing folders,
    /// so neither asks the question "what should this new folder be called?"
    /// as directly as a text box does.
    NewWorkspaceFolder {
        ci: usize,
        dir: std::path::PathBuf,
    },
}

/// Editable-code-view state for the request editor's Code section. Holds the
/// live text buffer the user edits (Hurl or resolved JSON) plus the identity of
/// the `(collection, entry, showing-Hurl)` it currently reflects. The buffer is
/// refreshed from the entry when you switch entry/representation or return to
/// the Code tab, but is otherwise the source of truth while you edit it (so
/// keystrokes are never clobbered by a re-render of the canonical text).
#[derive(Default)]
pub struct CodeEdit {
    pub buf: String,
    /// `(collection index, entry index, showing Hurl)` the buffer reflects, or
    /// `None` when the Code tab isn't the active section.
    pub key: Option<(usize, usize, bool)>,
    /// The last parse error, shown beneath the editor; cleared on a good parse.
    pub error: Option<String>,
}

pub struct GuiApp {
    pub session: Session,
    pub focus: Focus,
    pub editor_section: EditorSection,
    pub response_section: ResponseSection,
    /// When true, the Response Body view shortens long string literals to a
    /// `"head...tail"` overview (see [`crate::shared_utils::compact_long_strings`]).
    /// Display-only: the Copy button always yields the full body.
    pub response_compact: bool,
    pub dialog: Option<Dialog>,
    /// Recomputed each frame from the active theme spec.
    pub theme: GuiTheme,
    /// Recomputed each frame from the active language.
    pub strings: Strings,
    /// Show the raw request as Hurl (vs. the resolved JSON preview) in the Code
    /// section. Mirrors the terminal UI's `RequestView` toggle.
    pub show_hurl: bool,
    /// Editable-code-view buffer state for the request editor's Code section.
    pub code_edit: CodeEdit,
    /// An environment the Environments panel should expand and scroll to on the
    /// next frame — set when one is opened from the workspace tree, where the
    /// row that was clicked is nowhere near the panel that ends up holding it.
    /// Cleared once the panel has had its chance to honour it.
    pub reveal_env: Option<u64>,
    /// Report row selected in the reports panel, if the reports view is open.
    pub show_reports: bool,
    /// The open PaperTrail report editor (Scratch-style blocks + source view),
    /// if any. Opened from the reports list or a Workspace tree `.trail` file;
    /// takes over the centre pane while present. See [`report_editor`].
    pub report_editor: Option<report_editor::ReportEditor>,
    /// Git remote load/save UI state (self-contained in `remote.rs`).
    pub remote: super::remote::RemoteUi,
    /// An in-flight Workspace redownload (see [`Dialog::WorkspaceReload`]):
    /// the tab it will rebind, the file that was selected before (relative to
    /// the old, dead root) and the worker's result channel.
    pub workspace_redownload: Option<(usize, Option<String>, Receiver<Result<PathBuf, String>>)>,
    /// The PaperBoy logo texture, lazily uploaded on the first frame and shown
    /// in the status bar. `None` until loaded (or if decoding ever fails).
    pub logo: Option<egui::TextureHandle>,
    /// Set when the user has moved a splitter or resized the window and the new
    /// geometry has not been written to disk yet. Saving is deferred to the end
    /// of the gesture (see [`GuiApp::record_layout`]) so a single drag doesn't
    /// rewrite `state.json` once per frame.
    layout_dirty: bool,
    /// Set once the user has confirmed a quit that would discard unsaved
    /// request edits, so the close request that follows isn't intercepted a
    /// second time (which would make the window impossible to close).
    pub(super) allow_close: bool,
}

/// The raw PNG bytes of the application logo, embedded at compile time so the
/// binary is self-contained (no runtime asset path to resolve). Used for both
/// the window/taskbar icon and the status-bar badge.
pub(super) const LOGO_PNG: &[u8] = include_bytes!("../../assets/paperboy_logo.png");

/// Decode the embedded logo into an `egui::IconData` for the window/taskbar
/// icon. Returns `None` if decoding fails (we then fall back to the platform
/// default rather than refusing to launch).
pub fn load_app_icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(LOGO_PNG).ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    })
}

/// Decode the embedded logo into an egui image ready to upload as a texture.
fn logo_color_image() -> Option<egui::ColorImage> {
    let img = image::load_from_memory(LOGO_PNG).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        img.as_raw(),
    ))
}

impl GuiApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Register the Phosphor icon font so the tree/button icons render (see
        // `gui::icons`). egui's bundled fonts don't cover them, so without this
        // every icon shows as an empty "tofu" box.
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        cc.egui_ctx.set_fonts(fonts);

        // Larger default text so the client reads comfortably as a desktop app.
        // egui 0.35 has no `Context::style`/`set_style`; scale every variant's
        // text styles in place.
        cc.egui_ctx.all_styles_mut(|style| {
            for (_, font) in style.text_styles.iter_mut() {
                font.size *= 1.08;
            }
        });

        let session = Session::restored();
        let session_view_is_hurl = session.default_request_view == RequestView::Hurl;
        let strings = Strings::for_language(&session.language);
        let theme = GuiTheme::from_spec(&session.active_theme_spec());
        let mut app = Self {
            session,
            focus: Focus::List,
            editor_section: EditorSection::All,
            response_section: ResponseSection::Body,
            response_compact: false,
            reveal_env: None,
            dialog: None,
            theme,
            strings,
            show_hurl: session_view_is_hurl,
            code_edit: CodeEdit::default(),
            show_reports: false,
            report_editor: None,
            remote: super::remote::RemoteUi::default(),
            workspace_redownload: None,
            logo: None,
            layout_dirty: false,
            allow_close: false,
        };
        app.restore_view();
        app.restore_workspace_selection();
        app
    }

    /// A `GuiApp` around a ready-made session, for tests that need to draw a
    /// panel headlessly. `new` can't serve: it wants an `eframe` creation
    /// context and it restores the *real* user's session from disk.
    #[cfg(test)]
    pub(crate) fn for_test(session: Session) -> Self {
        let strings = Strings::for_language(&session.language);
        let theme = GuiTheme::from_spec(&session.active_theme_spec());
        Self {
            session,
            focus: Focus::List,
            editor_section: EditorSection::All,
            response_section: ResponseSection::Body,
            response_compact: false,
            reveal_env: None,
            dialog: None,
            theme,
            strings,
            show_hurl: false,
            code_edit: CodeEdit::default(),
            show_reports: false,
            report_editor: None,
            remote: super::remote::RemoteUi::default(),
            workspace_redownload: None,
            logo: None,
            layout_dirty: false,
            allow_close: false,
        }
    }

    /// Reopen the active Workspace tab on whatever node was last selected in
    /// its tree.
    ///
    /// A workspace tab's *collection* file is already restored by
    /// [`crate::persistence::PersistedTab::into_collection`] (it re-reads the
    /// file from disk), but a `.trail` report or a `.vars` environment opened
    /// from the tree left no trace at all — the tab came back with an empty
    /// right-hand pane and no hint of what had been open. `workspace_selected`
    /// closes that gap; the path was already checked to still exist when the
    /// state was loaded.
    fn restore_workspace_selection(&mut self) {
        let ci = self.active_ci();
        let Some(selected) = self
            .session
            .collections
            .get(ci)
            .filter(|c| c.workspace_root.is_some())
            .and_then(|c| c.workspace_selected.clone())
        else {
            return;
        };
        if crate::workspace::is_report_file(&selected) {
            // A report *tab* being restored by `restore_view` outranks this:
            // that is what the centre column was actually showing.
            if self.report_editor.is_some() {
                return;
            }
            match crate::report::Report::load_local(&selected) {
                Ok(report) => {
                    self.open_report_editor(ReportOrigin::Workspace, report);
                    self.show_reports = false;
                    self.focus = Focus::Main;
                }
                // A report that has since become unreadable is not worth
                // interrupting the launch over — the tree still shows it.
                Err(_) => {}
            }
        } else if crate::workspace::is_env_file(&selected) {
            self.session.open_workspace_environment(&selected);
        }
    }

    /// Reopen the centre column on whatever it was showing last time.
    ///
    /// Only session report tabs are restored here: a report opened from a
    /// Workspace `.trail` file has no index in the session's report list, so it
    /// is restored from the workspace tree instead (see
    /// [`Self::restore_workspace_selection`]). A stale index falls back to the
    /// reports list.
    fn restore_view(&mut self) {
        match self.session.gui.view {
            GuiView::Requests => {}
            GuiView::Reports => self.show_reports = true,
            GuiView::Report(i) => {
                self.show_reports = true;
                let Some(report) = self.session.reports.get(i).cloned() else {
                    return;
                };
                self.open_report_editor(ReportOrigin::Session(i), report.into_report());
                if self.session.gui.report_source_view
                    && let Some(ed) = &mut self.report_editor
                {
                    ed.view = report_editor::EditorView::Source;
                }
            }
        }
    }

    /// Open `report` in the block editor, restoring the panel sizes the user
    /// last dragged it to.
    ///
    /// Every report opens through here — restored at startup, picked from the
    /// reports list, created fresh, or opened from a Workspace tree — because a
    /// resized palette or diagnostics panel that only came back on *one* of
    /// those paths reads as the setting not being saved at all.
    pub fn open_report_editor(&mut self, origin: ReportOrigin, report: crate::report::Report) {
        let mut ed = report_editor::ReportEditor::new(origin, report);
        if let Some(h) = self.session.gui.report_diag_height {
            ed.diag_h = h;
        }
        if let Some(w) = self.session.gui.report_palette_width {
            ed.palette_w = w;
        }
        self.report_editor = Some(ed);
    }

    /// Fold one freshly-measured panel size into the saved layout, flagging the
    /// layout dirty when it actually moved. Sub-pixel jitter (egui lays panels
    /// out in floats, so a "still" splitter wobbles fractionally) is ignored,
    /// otherwise every frame would look like a resize.
    fn record_size(dirty: &mut bool, slot: &mut Option<f32>, measured: f32) {
        if slot.is_none_or(|old| (old - measured).abs() > 0.5) {
            *slot = Some(measured);
            *dirty = true;
        }
    }

    /// Capture the window size and the current centre-column view, then write
    /// the whole layout out once the user has finished dragging.
    ///
    /// Deferring the save until no mouse button is down means one splitter drag
    /// costs a single `state.json` write instead of one per frame, while still
    /// landing on disk the moment the gesture ends rather than only at exit
    /// (which a crash or a `kill` would skip).
    fn record_layout(&mut self, ctx: &egui::Context, root_size: egui::Vec2) {
        let mut dirty = self.layout_dirty;

        // `inner_rect` is the window's true frame, but no Wayland compositor
        // reports one back to the client, so fall back to the size of the root
        // `Ui` — for the root viewport that spans the window's inner area, in
        // the same logical points `with_inner_size` expects.
        let size = ctx
            .input(|i| i.viewport().inner_rect)
            .map_or(root_size, |r| r.size());
        {
            let (w, h) = (size.x, size.y);
            // Some compositors report a zero-sized viewport while the window is
            // minimised; persisting that would reopen an invisible window.
            if w >= super::MIN_WINDOW.0 && h >= super::MIN_WINDOW.1 {
                let moved = self
                    .session
                    .gui
                    .window
                    .is_none_or(|(ow, oh)| (ow - w).abs() > 0.5 || (oh - h).abs() > 0.5);
                if moved {
                    self.session.gui.window = Some((w, h));
                    dirty = true;
                }
            }
        }

        let view = match (&self.report_editor, self.show_reports) {
            (Some(ed), _) => match ed.origin {
                ReportOrigin::Session(i) => GuiView::Report(i),
                // A workspace report is shown *inside* its workspace tab, so
                // the view to come back to is that tab; which report it was
                // showing is restored from the tree's own selection.
                ReportOrigin::Workspace => GuiView::Requests,
            },
            (None, true) => GuiView::Reports,
            (None, false) => GuiView::Requests,
        };
        if self.session.gui.view != view {
            self.session.gui.view = view;
            dirty = true;
        }

        if let Some(ed) = &self.report_editor {
            let source = ed.view == report_editor::EditorView::Source;
            if self.session.gui.report_source_view != source {
                self.session.gui.report_source_view = source;
                dirty = true;
            }
            Self::record_size(
                &mut dirty,
                &mut self.session.gui.report_diag_height,
                ed.diag_h,
            );
            Self::record_size(
                &mut dirty,
                &mut self.session.gui.report_palette_width,
                ed.palette_w,
            );
        }

        self.layout_dirty = dirty;
        if dirty && !ctx.input(|i| i.pointer.any_down()) {
            self.session.save();
            self.layout_dirty = false;
        }
    }

    /// The active collection tab index, clamped into range.
    pub fn active_ci(&self) -> usize {
        self.session
            .active_tab
            .min(self.session.collections.len().saturating_sub(1))
    }

    /// Run the selected request of the active collection.
    pub fn run_active(&mut self) {
        let ci = self.active_ci();
        self.session.run_entry(ci);
        self.session.save();
    }

    /// Close tab `ci`, first asking what to do with its folder when that folder
    /// is a git download PaperBoy made itself. Every close path in the GUI goes
    /// through here so a downloaded workspace can never be dropped silently,
    /// leaving an orphaned temp folder behind with no way back to it.
    pub fn request_close_tab(&mut self, ci: usize) {
        // Edits first: a downloaded-workspace tab can be both, and losing
        // unsaved work is the more serious of the two.
        let count = self
            .session
            .collections
            .get(ci)
            .map_or(0, |c| c.unsaved_edit_count());
        if count > 0 {
            self.dialog = Some(Dialog::UnsavedCloseTab {
                ci,
                name: self.session.collections[ci].name.clone(),
                count,
            });
            return;
        }
        self.close_tab_now(ci);
    }

    /// Close tab `ci`, having already settled what to do about any unsaved
    /// edits. Still asks about a git-downloaded Workspace folder.
    pub fn close_tab_now(&mut self, ci: usize) {
        let downloaded = self
            .session
            .collections
            .get(ci)
            .filter(|c| c.workspace_downloaded_from_git)
            .and_then(|c| c.workspace_root.clone());
        match downloaded {
            Some(root) if ci != 0 => {
                self.dialog = Some(Dialog::CloseGitWorkspace { ci, root });
            }
            _ => self.session.close_tab(ci),
        }
    }

    /// Ask about the next Workspace tab whose downloaded folder went missing,
    /// one at a time so several affected tabs don't stack up modal dialogs.
    /// Called every frame; a no-op once the queue is drained.
    pub fn poll_workspace_reload_prompts(&mut self) {
        if self.dialog.is_some() || self.workspace_redownload.is_some() {
            return;
        }
        if let Some((ci, reload)) = self.session.pending_workspace_reloads.pop_front() {
            self.dialog = Some(Dialog::WorkspaceReload {
                ci,
                reload: Box::new(reload),
            });
        }
    }

    /// Start redownloading tab `ci`'s Workspace, pinned to the exact commit it
    /// recorded. Never prompts for a token (tokens are deliberately never
    /// persisted), so a private repo fails here with an auth error and the user
    /// must reload it through "Load workspace from Git…" instead.
    pub fn start_workspace_redownload(
        &mut self,
        ci: usize,
        reload: crate::persistence::PendingWorkspaceReload,
    ) {
        let rx = crate::tui::remote::spawn_workspace_redownload(reload.origin);
        self.workspace_redownload = Some((ci, reload.relative_selected_path, rx));
    }

    /// Drive an in-flight Workspace redownload (called every frame). On success
    /// the tab is rebound to the fresh folder and the previously-open file
    /// re-selected; a failure — most often the recorded commit no longer being
    /// reachable (force-push, rebase, deleted branch/tag) — is reported and the
    /// tab simply stays empty.
    pub fn poll_workspace_redownload(&mut self) {
        let Some((ci, relative, rx)) = self.workspace_redownload.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(root)) => self
                .session
                .rebind_redownloaded_workspace(ci, root, relative),
            Ok(Err(e)) => self.session.status = Some(crate::i18n::Status::WorkspaceReloadFailed(e)),
            Err(TryRecvError::Empty) => {
                // Still running — put it back for the next frame.
                self.workspace_redownload = Some((ci, relative, rx));
            }
            Err(TryRecvError::Disconnected) => {}
        }
    }

    /// A stroke used to outline the focused panel (accent) vs. others (dim).
    ///
    /// The width is **constant** across states — only the colour changes — so
    /// focusing a panel never changes its frame's footprint and therefore never
    /// nudges the panel's contents by a pixel. (An `egui` `Frame` counts its
    /// stroke width as part of its size, so varying the width would shift the
    /// body inward on focus.)
    pub fn focus_stroke(&self, panel: Focus) -> egui::Stroke {
        let color = if self.focus == panel {
            self.theme.accent
        } else {
            self.theme.raised()
        };
        egui::Stroke::new(1.6, color)
    }

    /// Wrap a panel body in a titled, focus-aware frame and register a click on
    /// it as focusing that panel.
    pub fn panel_frame<R>(
        &mut self,
        ui: &mut egui::Ui,
        panel: Focus,
        add_contents: impl FnOnce(&mut GuiApp, &mut egui::Ui) -> R,
    ) -> R {
        let stroke = self.focus_stroke(panel);
        // Register a background click-sense over the whole panel *before* its
        // contents, so it sits behind the interior widgets: a click on empty
        // space focuses the panel, but a click that lands on a list row, button
        // or field goes to that widget instead (egui routes a click to the
        // top-most — i.e. last-registered — widget under the pointer, so the
        // background must be registered first).
        let bg_id = ui.id().with(("panel_bg", panel));
        let bg = ui.interact(ui.max_rect(), bg_id, egui::Sense::click());
        let frame = egui::Frame::new()
            .stroke(stroke)
            .fill(self.theme.panel)
            .inner_margin(6.0)
            .corner_radius(4.0);
        let resp = frame.show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            add_contents(self, ui)
        });
        if bg.clicked() {
            self.focus = panel;
        }
        resp.inner
    }

    fn handle_global_keys(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() {
            return; // let the modal own the keyboard
        }
        // Tab / Shift+Tab cycle the focused *panel*, exactly like the terminal
        // UI. We pull Tab key-presses straight out of the event queue rather
        // than using `consume_key`: its `Modifiers::NONE` pattern also matches
        // Shift+Tab (egui's `matches_logically` only rejects *missing* pattern
        // modifiers, not *extra* ones), so a plain-Tab check would swallow
        // Shift+Tab and both would cycle forwards.
        let dir = ctx.input_mut(|i| {
            let mut dir: Option<bool> = None;
            i.events.retain(|e| match e {
                egui::Event::Key {
                    key: Key::Tab,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    dir = Some(!modifiers.shift); // Shift+Tab → backwards
                    false // consume it
                }
                _ => true,
            });
            dir
        });
        // egui records its *own* Tab/Shift+Tab focus-traversal direction in
        // `Memory::begin_pass`, which runs *before* this handler — so draining
        // the events above isn't enough to stop it walking focus across every
        // interactive widget (the tab bar, buttons, fields, …). Cancel that
        // direction every frame so Tab only ever moves our panel focus, never
        // egui's widget focus.
        ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        if let Some(forward) = dir {
            self.focus = self.focus.cycle(forward);
        }
        // Ctrl+Enter or F5 sends the current request (parity with the TUI's F5).
        let send = ctx.input_mut(|i| {
            i.consume_key(Modifiers::COMMAND, Key::Enter) || i.consume_key(Modifiers::NONE, Key::F5)
        });
        if send {
            self.run_active();
        }
        // Ctrl+W closes the active tab.
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::W)) {
            self.request_close_tab(self.active_ci());
        }
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let focused = self.focus == Focus::Tabs;
        let lbl_rename = self.strings.gui_rename_ellipsis;
        let lbl_close = self.strings.gui_close_tab;
        let mut open_rename: Option<(usize, String)> = None;
        let mut close_tab: Option<usize> = None;
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            let active = self.active_ci();
            let names: Vec<(usize, String, bool, bool, bool)> = self
                .session
                .collections
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    (
                        i,
                        c.name.clone(),
                        c.git_origin.is_some(),
                        c.is_workspace(),
                        // A Workspace tab may also be holding parked edits for
                        // files it isn't currently showing, so ask the
                        // collection rather than just scanning `entries`.
                        c.has_unsaved_edits() || !c.workspace_pending.is_empty(),
                    )
                })
                .collect();
            for (i, name, from_git, is_ws, edited) in names {
                let selected = i == active;
                let label = if is_ws {
                    format!("{} {name}", super::icons::FOLDER)
                } else if from_git {
                    format!("{} {name}", super::icons::GIT)
                } else {
                    name.clone()
                };
                let label = if edited {
                    format!("{label} {}", super::icons::EDITED)
                } else {
                    label
                };
                let mut text = egui::RichText::new(label);
                if selected {
                    text = text.strong().color(self.theme.text);
                } else {
                    text = text.color(self.theme.dim);
                }
                let resp = super::widgets::selectable(ui, selected, text);
                if resp.clicked() {
                    self.session.activate_tab(i);
                    self.focus = Focus::Tabs;
                    // Close a Workspace-opened report editor when leaving its
                    // tab; a standalone (session) report stays open.
                    if self
                        .report_editor
                        .as_ref()
                        .is_some_and(|e| e.is_workspace())
                    {
                        self.report_editor = None;
                    }
                    self.session.save();
                }
                // Middle-click closes a tab (not the built-in Request tab).
                if i != 0 && resp.middle_clicked() {
                    self.request_close_tab(i);
                }
                // Right-click: rename the collection, or close it (parity with
                // the TUI's rename-collection and close-tab actions).
                resp.context_menu(|ui| {
                    if ui.button(lbl_rename).clicked() {
                        open_rename = Some((i, name.clone()));
                        ui.close();
                    }
                    if i != 0 && ui.button(lbl_close).clicked() {
                        close_tab = Some(i);
                        ui.close();
                    }
                });
                if selected && focused {
                    resp.highlight();
                }
            }
            if ui
                .button("+")
                .on_hover_text(self.strings.gui_new_collection)
                .clicked()
            {
                self.session.add_collection(self.strings.gui_untitled);
                self.session.save();
            }
        });
        if let Some((ci, text)) = open_rename {
            self.dialog = Some(Dialog::Rename {
                target: RenameTarget::Tab { ci },
                text,
            });
        }
        if let Some(i) = close_tab {
            self.request_close_tab(i);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // The PaperBoy logo badge, lazily uploaded on first use. Drawn at
            // the text's own height so it sits inline with the status message.
            let logo = self.logo.get_or_insert_with(|| {
                let img = logo_color_image().unwrap_or_else(|| {
                    egui::ColorImage::new([1, 1], vec![egui::Color32::TRANSPARENT])
                });
                ui.ctx()
                    .load_texture("paperboy_logo", img, egui::TextureOptions::LINEAR)
            });
            let h = ui.text_style_height(&egui::TextStyle::Body);
            ui.add(egui::Image::new((logo.id(), egui::vec2(h, h))));
            ui.add_space(4.0);
            let msg = self
                .session
                .status
                .as_ref()
                .map(|s| s.text(&self.strings))
                .unwrap_or_default();
            ui.colored_label(self.theme.dim, msg);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let spec = self.session.active_theme_spec();
                ui.colored_label(
                    self.theme.dim,
                    format!("{} {}", self.strings.gui_theme_status_label, spec.name),
                );
                ui.separator();
                // The always-visible answer to "which environment am I about
                // to send this with?". The name is drawn in the "ok" colour and
                // bold — the rest of the status bar is uniformly dim, so an
                // active environment is the one thing here that stands out.
                let env = self
                    .session
                    .active_env_id
                    .and_then(|id| self.session.global_envs.iter().find(|e| e.id == id));
                match env {
                    Some(env) => ui.label(
                        egui::RichText::new(format!("{} {}", super::icons::PASS, env.name))
                            .color(self.theme.ok)
                            .strong(),
                    ),
                    None => ui.colored_label(self.theme.dim, self.strings.gui_none_dash),
                };
                ui.colored_label(self.theme.dim, self.strings.gui_env_label);
            });
        });
    }
}

impl eframe::App for GuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    fn on_exit(&mut self) {
        self.session.save();
    }
}

impl GuiApp {
    /// One whole frame of application UI.
    ///
    /// Split out of the `eframe::App` impl so tests can drive a complete frame:
    /// `eframe::Frame` can't be built outside `eframe`, but a bare
    /// `egui::Context` needs nothing at all, and whole-frame rendering is the
    /// only way to catch problems that live *between* panels (widget id
    /// clashes, for one).
    pub(super) fn draw(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        // Refresh theme + strings from the session (cheap; picks up live edits).
        let spec = self.session.active_theme_spec();
        self.theme = GuiTheme::from_spec(&spec);
        self.theme.apply(&ctx);
        self.strings = Strings::for_language(&self.session.language);

        // Drain background work (secret resolution, captures, Run All) and keep
        // animating while anything is in flight.
        let busy = self.session.poll();
        if busy {
            ctx.request_repaint_after(Duration::from_millis(80));
        }

        self.handle_global_keys(&ctx);
        self.intercept_close(&ctx);

        // egui 0.35: the app is handed a root `Ui` and every region is an
        // `egui::Panel` nested into it (outermost added first, CentralPanel
        // last). `.resizable(true)` panels get native drag-to-resize handles —
        // the GUI's replacement for the terminal UI's `<`/`>` and `+`/`-` keys.
        // Panels remember their dragged size in egui's own memory, which is not
        // persisted, so read the sizes back out of the responses each frame and
        // keep them in `state.json` ourselves.
        let mut measured_env = None;
        let mut measured_response = None;
        let root_size = ui.max_rect().size();

        egui::Panel::top("menu_bar").show(ui, |ui| menu::menu_bar(self, ui));
        egui::Panel::top("tab_strip").show(ui, |ui| self.tab_strip(ui));
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));

        // Left column: Requests (top) + Global Environments (bottom). A width
        // the user dragged wins; otherwise fall back to a pixel width derived
        // from the terminal UI's column count so the two front-ends open on a
        // comparable layout the first time.
        let left_default = self
            .session
            .gui
            .left_width
            .unwrap_or_else(|| (self.session.list_width as f32 * 8.0).clamp(220.0, 460.0));
        let left = egui::Panel::left("left_col")
            .resizable(true)
            .default_size(left_default)
            // 200px keeps the environment editor's fixed-width variable grid
            // within the panel: request/folder/env names truncate and the
            // action buttons wrap, but the grid (key field + value + remove)
            // has a hard minimum around 185px. Bounding the panel there means
            // no content ever exceeds it, so dragging the splitter narrower
            // can't leave the unpainted "black strip".
            .min_size(200.0)
            .max_size(560.0)
            .show(ui, |ui| {
                let avail = ui.available_height();
                let env_h = self.session.gui.env_height.unwrap_or_else(|| {
                    (avail * self.session.response_pct as f32 / 100.0)
                        .clamp(120.0, (avail - 120.0).max(120.0))
                });
                // Permissive vertical limit: keep at least ~80px of the
                // Requests panel above visible (looser than the side-to-side
                // 180px minimum) so the bottom panel can't fully cover the top.
                let env_max = (avail - 80.0).max(80.0);
                let env = egui::Panel::bottom("env_panel")
                    .resizable(true)
                    .default_size(env_h)
                    .min_size(80.0)
                    .max_size(env_max)
                    .show(ui, |ui| {
                        self.panel_frame(ui, Focus::GlobalEnv, |app, ui| {
                            environments::ui(app, ui);
                        });
                    });
                measured_env = Some(env.response.rect.height());
                egui::CentralPanel::default().show(ui, |ui| {
                    self.panel_frame(ui, Focus::List, |app, ui| {
                        requests::ui(app, ui);
                    });
                });
            });

        // Centre: request editor (top) + response (bottom), the reports view,
        // or the open PaperTrail report editor (blocks / source).
        egui::CentralPanel::default().show(ui, |ui| {
            if self.report_editor.is_some() {
                self.panel_frame(ui, Focus::Main, |app, ui| report_editor::ui(app, ui));
                return;
            }
            if self.show_reports {
                self.panel_frame(ui, Focus::Main, |app, ui| reports::ui(app, ui));
                return;
            }
            let avail = ui.available_height();
            let resp_h = self.session.gui.response_height.unwrap_or_else(|| {
                (avail * self.session.response_pct as f32 / 100.0)
                    .clamp(140.0, (avail - 140.0).max(140.0))
            });
            // Keep at least ~80px of the editor above visible (permissive
            // vertical cap, looser than the horizontal 180px minimum).
            let resp_max = (avail - 80.0).max(80.0);
            let resp = egui::Panel::bottom("response_panel")
                .resizable(true)
                .default_size(resp_h)
                .min_size(80.0)
                .max_size(resp_max)
                .show(ui, |ui| {
                    self.panel_frame(ui, Focus::Response, |app, ui| {
                        response::ui(app, ui);
                    });
                });
            measured_response = Some(resp.response.rect.height());
            egui::CentralPanel::default().show(ui, |ui| {
                self.panel_frame(ui, Focus::Main, |app, ui| {
                    editor::ui(app, ui);
                });
            });
        });

        let mut dirty = self.layout_dirty;
        Self::record_size(
            &mut dirty,
            &mut self.session.gui.left_width,
            left.response.rect.width(),
        );
        if let Some(h) = measured_env {
            Self::record_size(&mut dirty, &mut self.session.gui.env_height, h);
        }
        if let Some(h) = measured_response {
            Self::record_size(&mut dirty, &mut self.session.gui.response_height, h);
        }
        self.layout_dirty = dirty;
        self.record_layout(&ctx, root_size);

        menu::show_dialog(self, &ctx);
        remote::show(self, &ctx);
        self.poll_workspace_redownload();
        self.poll_workspace_reload_prompts();
        if self.workspace_redownload.is_some() {
            // The download runs on a worker thread, so nothing would otherwise
            // wake the UI when it finishes.
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        report_id_clashes(&ctx);
    }

    /// Refuse a window close that would silently discard request edits, and put
    /// the warning up instead.
    ///
    /// The window manager's close is a *request*: `CancelClose` withdraws it for
    /// this frame, which is the only chance to ask — `on_exit` runs too late to
    /// stop anything. `allow_close` lets the confirmed close straight through
    /// rather than looping on the same question forever.
    fn intercept_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.allow_close {
            return;
        }
        // Only what a quit would actually destroy: a plain tab's edits are
        // saved with the session and are still there (still flagged) next start,
        // so warning about them cried wolf every single time.
        let count: usize = self
            .session
            .collections
            .iter()
            .map(|c| c.edits_lost_on_exit())
            .sum();
        if count == 0 {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        // Don't stack this on top of whatever else is open; the close was
        // already refused, so the user can simply try again.
        if self.dialog.is_none() {
            let tabs = self
                .session
                .collections
                .iter()
                .filter(|c| c.edits_lost_on_exit() > 0)
                .map(|c| c.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            self.dialog = Some(Dialog::UnsavedQuit { count, tabs });
        }
    }

    /// Write out every edit that quitting would otherwise destroy, and report
    /// how many files were written. Backs "Save all changes" on the quit
    /// dialog.
    ///
    /// The set of files is [`Collection::edits_lost_on_exit`]'s, not
    /// [`Collection::unsaved_edit_count`]'s: an ordinary tab's edits survive a
    /// quit inside the session state, so writing them out to a `.hurl` on the
    /// way past would be making a decision -- where the file goes, and that the
    /// edit is finished -- that the user never asked this button to make.
    ///
    /// A failure stops at the offending file rather than pressing on, so the
    /// caller can name it. Files already written stay written and are no longer
    /// flagged, so answering the dialog again retries only what is left.
    pub(super) fn save_all_unsaved_edits(&mut self) -> Result<usize, String> {
        let mut written = 0usize;
        for c in &mut self.session.collections {
            written += c.save_workspace_edits()?;
        }
        self.session.save();
        self.session.status = Some(crate::i18n::Status::SavedFiles(written));
        Ok(written)
    }
}

/// Print any widget-id clash `egui` flagged this frame, when
/// `PAPERBOY_ID_CLASH=1` is set.
///
/// `egui` reports a clash by stroking a red rectangle around the offender and
/// writing a `🔥 …` note beside it — it neither logs nor returns anything, so a
/// user who sees the red flash has no way to say *which* widget it was. Reading
/// the debug layer back out turns that flash into a line on stderr naming the
/// widget, which is the only practical way to chase a clash that only shows up
/// in someone else's session.
///
/// Only compiled in debug builds, because that is the only place `egui` runs
/// the check at all (`Options::warn_on_id_clash` is `cfg!(debug_assertions)`).
#[cfg(debug_assertions)]
fn report_id_clashes(ctx: &egui::Context) {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("PAPERBOY_ID_CLASH").is_some()) {
        return;
    }
    fn walk(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
        match shape {
            egui::epaint::Shape::Text(t) if t.galley.text().contains('\u{1f525}') => {
                out.push(t.galley.text().to_string());
            }
            egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    ctx.graphics(|g| {
        if let Some(list) = g.get(egui::LayerId::debug()) {
            for c in list.all_entries() {
                walk(&c.shape, &mut found);
            }
        }
    });
    for f in found {
        eprintln!("[paperboy] egui id clash: {f}");
    }
}

#[cfg(not(debug_assertions))]
fn report_id_clashes(_ctx: &egui::Context) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn edited_collection(name: &str) -> crate::collection::Collection {
        let mut e = crate::hurl::HurlEntry::default();
        e.title = "req".into();
        e.method = "GET".into();
        e.url = "https://example.com".into();
        e.modified = true;
        crate::collection::Collection::new(name.to_string(), vec![e])
    }

    /// The same, but bound to a Workspace folder — the one kind of tab whose
    /// edits a quit really does destroy, since its entries are re-read from
    /// disk on restore rather than restored from the session state.
    fn edited_workspace_collection(name: &str) -> crate::collection::Collection {
        let mut c = edited_collection(name);
        c.workspace_root = Some(std::path::PathBuf::from("/tmp/paperboy-test-ws"));
        c
    }

    /// "Save all changes" has to leave nothing behind for the dialog to object
    /// to a second time -- otherwise the button would appear to do nothing.
    #[test]
    fn saving_all_changes_writes_the_workspace_file_and_clears_the_quit_warning() {
        let dir = std::env::temp_dir().join(format!(
            "paperboy_gui_save_all_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("health.hurl");
        std::fs::write(&file, "GET https://example.com/health\n").unwrap();

        // save_all_unsaved_edits() persists the session, which would otherwise
        // land on the developer's own state.json.
        super::requests::tests::redirect_saved_state();

        let mut session = Session::default();
        session.collections.clear();
        let mut col = edited_workspace_collection("ws");
        col.workspace_root = Some(dir.clone());
        col.path = Some(file.clone());
        col.entries[0].url = "https://example.com/health/v2".into();
        session.collections.push(col);
        let mut app = GuiApp::for_test(session);

        assert_eq!(
            app.session.collections[0].edits_lost_on_exit(),
            1,
            "the fixture starts with exactly the edit the dialog would warn about"
        );

        let written = app
            .save_all_unsaved_edits()
            .expect("the temporary file is writable");
        assert_eq!(written, 1, "the one edited file was written");
        let on_disk = std::fs::read_to_string(&file).unwrap();
        assert!(
            on_disk.contains("https://example.com/health/v2"),
            "the edit reached the file rather than just being marked saved: {on_disk}"
        );
        assert_eq!(
            app.session.collections[0].edits_lost_on_exit(),
            0,
            "so a second close request would go straight through"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Closing a tab that is holding edits with nowhere on disk to go asks
    /// first — the tab is still there afterwards.
    #[test]
    fn closing_a_tab_with_unsaved_edits_asks_before_throwing_them_away() {
        let mut session = Session::default();
        session.collections.clear();
        // Tab 0 is the built-in Request tab and is never closable, so the
        // fixture puts both tabs under test after it.
        session.collections.push(crate::collection::Collection::new(
            "home".to_string(),
            Vec::new(),
        ));
        session.collections.push(edited_collection("dirty"));
        session.collections.push(crate::collection::Collection::new(
            "clean".to_string(),
            Vec::new(),
        ));
        let mut app = GuiApp::for_test(session);

        app.request_close_tab(1);
        assert!(
            matches!(
                &app.dialog,
                Some(Dialog::UnsavedCloseTab {
                    ci: 1,
                    count: 1,
                    ..
                })
            ),
            "the warning should name the tab and how much is at stake"
        );
        assert_eq!(
            app.session.collections.len(),
            3,
            "nothing may be closed until the question is answered"
        );

        // A tab with nothing unsaved closes straight away, no question asked.
        app.dialog = None;
        app.request_close_tab(2);
        assert!(app.dialog.is_none(), "a clean tab must not be nagged about");
        assert_eq!(app.session.collections.len(), 2);
    }

    /// The window manager's close is only a *request*: PaperBoy has to refuse
    /// it and ask, because `on_exit` runs far too late to stop anything.
    #[test]
    fn quitting_with_unsaved_edits_is_refused_until_it_is_confirmed() {
        fn close_request() -> egui::RawInput {
            let mut input = egui::RawInput::default();
            input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .events
                .push(egui::ViewportEvent::Close);
            input
        }
        fn cancelled(out: &egui::FullOutput) -> bool {
            out.viewport_output
                .values()
                .any(|v| v.commands.contains(&egui::ViewportCommand::CancelClose))
        }

        let mut session = Session::default();
        session.collections.clear();
        session
            .collections
            .push(edited_workspace_collection("dirty"));
        let mut app = GuiApp::for_test(session);

        let ctx = egui::Context::default();
        let out = ctx.run_ui(close_request(), |ui| app.intercept_close(ui.ctx()));
        assert!(cancelled(&out), "the close must be withdrawn, not honoured");
        assert!(
            matches!(&app.dialog, Some(Dialog::UnsavedQuit { count: 1, .. })),
            "and the user asked about the edit that would be lost"
        );

        // Confirming sets `allow_close`; the close that follows must go through,
        // or the window could never be closed at all.
        app.allow_close = true;
        app.dialog = None;
        let out = ctx.run_ui(close_request(), |ui| app.intercept_close(ui.ctx()));
        assert!(!cancelled(&out), "a confirmed quit must not be intercepted");
        assert!(app.dialog.is_none(), "and must not ask a second time");
    }

    /// Nothing unsaved, nothing to say — the window closes without a word.
    #[test]
    fn quitting_with_everything_saved_is_not_interrupted() {
        let mut session = Session::default();
        session.collections.clear();
        session.collections.push(crate::collection::Collection::new(
            "clean".to_string(),
            Vec::new(),
        ));
        let mut app = GuiApp::for_test(session);
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
        let out = ctx.run_ui(input, |ui| app.intercept_close(ui.ctx()));
        assert!(
            !out.viewport_output
                .values()
                .any(|v| v.commands.contains(&egui::ViewportCommand::CancelClose)),
            "a clean session must never have its quit interrupted"
        );
        assert!(app.dialog.is_none());
    }
}
