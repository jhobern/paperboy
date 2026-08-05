//! The GUI application: [`GuiApp`] wraps the shared [`Session`] with egui view
//! state and lays out the Postman-style panels every frame.

use std::time::Duration;

use eframe::egui::{self, Key, Modifiers};

use crate::i18n::Strings;
use crate::session::Session;

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
    /// Open a file (collection / environment / report). Holds the current path
    /// text and what kind of file is being opened.
    OpenFile {
        kind: OpenKind,
        path: String,
        error: Option<String>,
    },
    /// Save the active collection / environment / response to a path.
    SaveFile {
        kind: SaveKind,
        path: String,
        error: Option<String>,
    },
    /// Rename a request or collection tab.
    Rename { target: RenameTarget, text: String },
    /// The theme editor.
    Theme(Box<super::menu::ThemeEditState>),
    /// Simple text prompt (base URL, new env name, …).
    Prompt { kind: PromptKind, text: String },
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    BaseUrl,
    NewEnvName,
    NewCollectionName,
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
    /// Report row selected in the reports panel, if the reports view is open.
    pub show_reports: bool,
    /// The open PaperTrail report editor (Scratch-style blocks + source view),
    /// if any. Opened from the reports list or a Workspace tree `.trail` file;
    /// takes over the centre pane while present. See [`report_editor`].
    pub report_editor: Option<report_editor::ReportEditor>,
    /// Git remote load/save UI state (self-contained in `remote.rs`).
    pub remote: super::remote::RemoteUi,
    /// The PaperBoy logo texture, lazily uploaded on the first frame and shown
    /// in the status bar. `None` until loaded (or if decoding ever fails).
    pub logo: Option<egui::TextureHandle>,
}

/// The raw PNG bytes of the application logo, embedded at compile time so the
/// binary is self-contained (no runtime asset path to resolve). Used for both
/// the window/taskbar icon and the status-bar badge.
const LOGO_PNG: &[u8] = include_bytes!("../../assets/paperboy_logo.png");

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
        let strings = Strings::for_language(&session.language);
        let theme = GuiTheme::from_spec(&session.active_theme_spec());
        Self {
            session,
            focus: Focus::List,
            editor_section: EditorSection::All,
            response_section: ResponseSection::Body,
            response_compact: false,
            dialog: None,
            theme,
            strings,
            show_hurl: false,
            show_reports: false,
            report_editor: None,
            remote: super::remote::RemoteUi::default(),
            logo: None,
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
            self.session.close_tab(self.active_ci());
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
            let names: Vec<(usize, String, bool, bool)> = self
                .session
                .collections
                .iter()
                .enumerate()
                .map(|(i, c)| (i, c.name.clone(), c.git_origin.is_some(), c.is_workspace()))
                .collect();
            for (i, name, from_git, is_ws) in names {
                let selected = i == active;
                let label = if is_ws {
                    format!("{} {name}", super::icons::FOLDER)
                } else if from_git {
                    format!("{} {name}", super::icons::GIT)
                } else {
                    name.clone()
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
                    self.session.close_tab(i);
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
            self.session.close_tab(i);
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
                let env = self
                    .session
                    .active_env_id
                    .and_then(|id| self.session.global_envs.iter().find(|e| e.id == id))
                    .map(|e| e.name.clone())
                    .unwrap_or_else(|| self.strings.gui_none_dash.to_string());
                ui.colored_label(
                    self.theme.dim,
                    format!("{} {}", self.strings.gui_env_label, env),
                );
            });
        });
    }
}

impl eframe::App for GuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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

        // egui 0.35: the app is handed a root `Ui` and every region is an
        // `egui::Panel` nested into it (outermost added first, CentralPanel
        // last). `.resizable(true)` panels get native drag-to-resize handles —
        // the GUI's replacement for the terminal UI's `<`/`>` and `+`/`-` keys.
        egui::Panel::top("menu_bar").show(ui, |ui| menu::menu_bar(self, ui));
        egui::Panel::top("tab_strip").show(ui, |ui| self.tab_strip(ui));
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));

        // Left column: Requests (top) + Global Environments (bottom).
        let left_default = (self.session.list_width as f32 * 8.0).clamp(220.0, 460.0);
        egui::Panel::left("left_col")
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
                let env_h = (avail * self.session.response_pct as f32 / 100.0)
                    .clamp(120.0, (avail - 120.0).max(120.0));
                // Permissive vertical limit: keep at least ~80px of the
                // Requests panel above visible (looser than the side-to-side
                // 180px minimum) so the bottom panel can't fully cover the top.
                let env_max = (avail - 80.0).max(80.0);
                egui::Panel::bottom("env_panel")
                    .resizable(true)
                    .default_size(env_h)
                    .min_size(80.0)
                    .max_size(env_max)
                    .show(ui, |ui| {
                        self.panel_frame(ui, Focus::GlobalEnv, |app, ui| {
                            environments::ui(app, ui);
                        });
                    });
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
            let resp_h = (avail * self.session.response_pct as f32 / 100.0)
                .clamp(140.0, (avail - 140.0).max(140.0));
            // Keep at least ~80px of the editor above visible (permissive
            // vertical cap, looser than the horizontal 180px minimum).
            let resp_max = (avail - 80.0).max(80.0);
            egui::Panel::bottom("response_panel")
                .resizable(true)
                .default_size(resp_h)
                .min_size(80.0)
                .max_size(resp_max)
                .show(ui, |ui| {
                    self.panel_frame(ui, Focus::Response, |app, ui| {
                        response::ui(app, ui);
                    });
                });
            egui::CentralPanel::default().show(ui, |ui| {
                self.panel_frame(ui, Focus::Main, |app, ui| {
                    editor::ui(app, ui);
                });
            });
        });

        menu::show_dialog(self, &ctx);
        remote::show(self, &ctx);
    }

    fn on_exit(&mut self) {
        self.session.save();
    }
}
