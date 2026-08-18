//! Left-bottom panel: Global Environments — activate one for substitution,
//! link one to the active collection, edit variables, and load/save `.vars`
//! files. Resolved secret values are shown but never editable (their provider
//! reference is the source of truth).

use eframe::egui::{self, RichText};

use crate::environment::{EnvVar, ValueSource};
use crate::i18n::Strings;

use super::app::{Dialog, GuiApp, OpenKind, PromptKind, SaveKind};

fn source_label(source: ValueSource) -> Option<&'static str> {
    match source {
        ValueSource::Literal => None,
        ValueSource::ProcessEnv => Some("env"),
        ValueSource::Ssm => Some("ssm"),
        ValueSource::OnePassword => Some("1password"),
        ValueSource::Unknown => Some("?"),
    }
}

fn env_source_label(s: &Strings, source: crate::env_panel::EnvSource) -> &'static str {
    match source {
        crate::env_panel::EnvSource::Both => s.gui_env_source_both,
        crate::env_panel::EnvSource::Global => s.gui_env_source_global,
        crate::env_panel::EnvSource::Workspace => s.gui_env_source_workspace,
    }
}

fn has_workspace(app: &GuiApp) -> bool {
    app.session
        .collections
        .get(app.active_ci())
        .and_then(|c| c.workspace_root.as_deref())
        .is_some()
}

fn workspace_files(app: &GuiApp) -> Vec<std::path::PathBuf> {
    app.session
        .collections
        .get(app.active_ci())
        .map(|c| c.workspace_env_files())
        .unwrap_or_default()
}

fn effective_source(app: &GuiApp) -> crate::env_panel::EnvSource {
    if has_workspace(app) {
        app.session.env_source
    } else {
        // The source picker is hidden without a Workspace tab; forcing "Both"
        // here avoids reopening the app on a remembered Workspace-only choice
        // that would make ordinary global environments appear to vanish.
        crate::env_panel::EnvSource::Both
    }
}

/// The panel's rows: the open Workspace's environment files (loaded or not)
/// followed by every other loaded environment, narrowed by the filter box.
/// See [`crate::env_panel`], which both front-ends share so they list the same
/// things in the same order.
#[cfg(test)]
fn env_rows(app: &GuiApp) -> Vec<crate::env_panel::EnvRow> {
    env_rows_from(app, &workspace_files(app))
}

/// [`env_rows`] against a file list already in hand. The panel needs the same
/// list three times over — the rows, the unfiltered rows behind the "no
/// matches" message, and the empty state — and gathering it once is what keeps
/// those from being three passes over the workspace scan per frame.
fn env_rows_from(app: &GuiApp, files: &[std::path::PathBuf]) -> Vec<crate::env_panel::EnvRow> {
    crate::env_panel::rows(
        &app.session.global_envs,
        files,
        &app.env_query,
        effective_source(app),
    )
}

pub fn ui(app: &mut GuiApp, ui: &mut egui::Ui) {
    let theme = app.theme;
    let ci = app.active_ci();
    let (
        lbl_environments,
        lbl_load,
        tip_load,
        tip_new,
        lbl_no_envs,
        lbl_active,
        tip_active,
        lbl_linked,
        tip_linked,
        lbl_delete,
        lbl_save,
    ) = {
        let s = &app.strings;
        (
            s.gui_environments,
            s.gui_load_ellipsis,
            s.gui_load_vars_tooltip,
            s.gui_new_environment,
            s.gui_no_environments,
            s.gui_active,
            s.gui_active_tooltip,
            s.gui_linked,
            s.gui_linked_tooltip,
            s.gui_delete,
            s.gui_save_ellipsis,
        )
    };
    // The header right-click menu's labels, which say what the click will *do*
    // rather than what the row currently *is* (the body's buttons are toggles
    // showing state, so "Active" reads correctly there and wouldn't here).
    let (lbl_activate, lbl_deactivate, lbl_link, lbl_unlink) = {
        let s = &app.strings;
        (
            s.gui_env_menu_activate,
            s.gui_env_menu_deactivate,
            s.gui_env_menu_link,
            s.gui_env_menu_unlink,
        )
    };

    super::widgets::panel_header(ui, &theme, lbl_environments, |ui| {
        if ui.button(lbl_load).on_hover_text(tip_load).clicked() {
            super::menu::open_via_picker(app, OpenKind::Environment);
        }
        if ui
            .button(super::icons::PLUS)
            .on_hover_text(tip_new)
            .clicked()
        {
            app.dialog = Some(Dialog::Prompt {
                kind: PromptKind::NewEnvName,
                text: String::new(),
            });
        }
    });

    // Filter box. Always shown rather than hidden behind a toggle: a workspace
    // of a few hundred environments is unusable without it, and an empty box is
    // one line of panel for a permanently useful control.
    ui.horizontal(|ui| {
        super::widgets::flat_fields(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut app.env_query)
                    .hint_text(app.strings.gui_env_filter_hint)
                    .desired_width(f32::INFINITY),
            )
        });
    });
    if has_workspace(app) {
        ui.horizontal_wrapped(|ui| {
            for source in [
                crate::env_panel::EnvSource::Both,
                crate::env_panel::EnvSource::Global,
                crate::env_panel::EnvSource::Workspace,
            ] {
                if ui
                    .selectable_label(
                        app.session.env_source == source,
                        env_source_label(&app.strings, source),
                    )
                    .clicked()
                {
                    app.session.env_source = source;
                    app.session.save();
                }
            }
        });
    }
    ui.separator();

    let files = workspace_files(app);
    let rows = env_rows_from(app, &files);
    let rows_for_source =
        crate::env_panel::rows(&app.session.global_envs, &files, "", effective_source(app));
    let active = app.session.active_env_id;
    let linked = app.session.collections[ci].linked_env_id;

    // One-shot: consumed on the frame that shows the row, so a later manual
    // collapse isn't fought by a request that never expires.
    let reveal_target = app.reveal_env;
    let mut activate: Option<u64> = None;
    let mut link: Option<u64> = None;
    let mut delete: Option<u64> = None;
    let mut save: Option<u64> = None;
    let mut open_file: Option<std::path::PathBuf> = None;
    let mut changed = false;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Truncate long environment names so they can't report a content
            // width wider than the panel (see the note in `requests.rs` — an
            // over-wide panel leaves an unpainted strip while being dragged).
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            if rows.is_empty() {
                ui.add_space(6.0);
                // "Nothing loaded" and "the filter hid everything" are
                // different problems with different fixes.
                let empty = if app.session.global_envs.is_empty() && files.is_empty() {
                    lbl_no_envs
                } else if rows_for_source.is_empty() {
                    app.strings.gui_env_source_no_matches
                } else if app.env_query.trim().is_empty() {
                    lbl_no_envs
                } else {
                    app.strings.gui_env_filter_no_matches
                };
                ui.colored_label(theme.dim, empty);
            }
            for row in &rows {
                // Workspace files that have not been opened yet still use the
                // same collapsible row as loaded environments: clicking one is
                // the user's "expand" gesture, so it loads the file and the
                // next frame reveals the variables. Keeping the id path-based
                // means the row does not inherit state from a filtered-out
                // neighbour, and does not change shape when it becomes loaded.
                let Some(id) = row.env_id() else {
                    let Some(path) = row.file().map(|p| p.to_path_buf()) else {
                        continue;
                    };
                    let state_id = egui::Id::new(("env-path", path.clone()));
                    let state = egui::collapsing_header::CollapsingState::load_with_default_open(
                        ui.ctx(),
                        state_id,
                        false,
                    );
                    if state.is_open() {
                        open_file = Some(path.clone());
                    }
                    let text = RichText::new(format!("{} {}", super::icons::FOLDER, row.name))
                        .color(theme.dim);
                    let header = super::widgets::tree_header_marked(
                        ui,
                        ("env-path", path.clone()),
                        false,
                        false,
                        text,
                        None,
                        |_ui| {},
                    )
                    .on_hover_text(app.strings.gui_env_open_workspace_tooltip);
                    if header.clicked() {
                        open_file = Some(path);
                    }
                    continue;
                };
                let idx = match app.session.global_envs.iter().position(|e| e.id == id) {
                    Some(i) => i,
                    None => continue,
                };
                let is_active = active == Some(id);
                let is_linked = linked == Some(id);
                let name = app.session.global_envs[idx].name.clone();
                let from_git = app.session.global_envs[idx].git_origin.is_some();
                // The active environment is marked the way the terminal UI
                // marks it — a leading tick in the "ok" colour with the name to
                // match — plus a filled band behind the row, because a GUI list
                // is sparse enough that colour alone is easy to skim past.
                // A folder icon marks the environments that came from the open
                // workspace, so the panel's two sources stay distinguishable.
                let header = format!(
                    "{}{}{}",
                    if is_active {
                        format!("{} ", super::icons::PASS)
                    } else {
                        String::new()
                    },
                    if row.workspace {
                        format!("{} ", super::icons::FOLDER)
                    } else if from_git {
                        format!("{} ", super::icons::GIT)
                    } else {
                        String::new()
                    },
                    name,
                );

                let mut text = RichText::new(header);
                if is_active {
                    text = text.color(theme.ok).strong();
                } else {
                    text = text.color(theme.text);
                }
                // Opening a `.vars` file from the workspace tree reveals it
                // here: loading it alone would leave the user looking at a
                // collapsed row and no sign anything had happened.
                let reveal = reveal_target == Some(id);
                let id_salt = app.session.global_envs[idx]
                    .path
                    .as_ref()
                    .filter(|_| row.workspace)
                    .map(|path| ("env-path", path.clone()))
                    .unwrap_or_else(|| ("env", std::path::PathBuf::from(id.to_string())));
                let header = super::widgets::tree_header_marked(
                    ui,
                    id_salt,
                    false,
                    reveal,
                    text,
                    is_active.then_some(theme.ok),
                    |ui| {
                        // Wrap the action buttons so their fixed widths can't
                        // overflow (and report an over-wide content size) when
                        // the panel is dragged narrow — they flow onto a second
                        // line instead. This is also robust to longer
                        // translations of the button labels.
                        ui.horizontal_wrapped(|ui| {
                            if super::widgets::selectable(ui, is_active, lbl_active)
                                .on_hover_text(tip_active)
                                .clicked()
                            {
                                activate = Some(id);
                            }
                            if super::widgets::selectable(ui, is_linked, lbl_linked)
                                .on_hover_text(tip_linked)
                                .clicked()
                            {
                                link = Some(id);
                            }
                            if ui.button(lbl_save).clicked() {
                                save = Some(id);
                            }
                            if ui
                                .button(RichText::new(lbl_delete).color(theme.err))
                                .clicked()
                            {
                                delete = Some(id);
                            }
                        });
                        ui.add_space(4.0);
                        if var_editor(
                            ui,
                            &theme,
                            &app.strings,
                            &mut app.session.global_envs[idx].vars,
                        ) {
                            changed = true;
                        }
                    },
                );
                // Scroll the revealed row into view — the panel is short and a
                // workspace can hold more environments than fit in it.
                if reveal {
                    header.scroll_to_me(Some(egui::Align::Center));
                }
                // The row's buttons live *inside* the collapsing body, so
                // switching environments used to mean expanding a row, clicking
                // Active, and collapsing it again — three gestures for the one
                // thing this panel is most often opened to do. The header
                // answers directly: double-click activates, right-click offers
                // the same actions the body does without opening it.
                if header.double_clicked() {
                    activate = Some(id);
                }
                header.context_menu(|ui| {
                    let toggle = if is_active {
                        lbl_deactivate
                    } else {
                        lbl_activate
                    };
                    if ui.button(toggle).clicked() {
                        activate = Some(id);
                        ui.close();
                    }
                    let link_lbl = if is_linked { lbl_unlink } else { lbl_link };
                    if ui.button(link_lbl).on_hover_text(tip_linked).clicked() {
                        link = Some(id);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(lbl_save).clicked() {
                        save = Some(id);
                        ui.close();
                    }
                    if ui
                        .button(RichText::new(lbl_delete).color(theme.err))
                        .clicked()
                    {
                        delete = Some(id);
                        ui.close();
                    }
                });
            }
        });

    // Whether or not the row was found, the request is spent after one frame.
    app.reveal_env = None;

    if let Some(path) = open_file {
        // Opening reveals it, so the row visibly becomes a loaded environment
        // rather than just quietly changing colour somewhere in a long list.
        app.reveal_env = app.session.open_workspace_environment(&path);
        if app.reveal_env.is_none() {
            let id = egui::Id::new(("env-path", path));
            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                false,
            );
            state.set_open(false);
            state.store(ui.ctx());
        }
        app.session.save();
    }
    if let Some(id) = activate {
        app.session.set_active_env(Some(id));
    }
    if let Some(id) = link {
        app.session.set_linked_env(ci, Some(id));
    }
    if let Some(id) = delete {
        app.session.delete_environment(id);
    }
    if let Some(id) = save {
        super::menu::save_via_picker(app, SaveKind::Environment(id));
    }
    if changed {
        for col in &mut app.session.collections {
            col.invalidate_request_json();
        }
    }
}

/// Editable variable table for one environment. Literal values are editable in
/// place; secret-backed values show their provider and resolution state and are
/// not directly editable (the `.vars` reference is the source of truth).
fn var_editor(
    ui: &mut egui::Ui,
    theme: &super::theme::GuiTheme,
    s: &Strings,
    vars: &mut Vec<EnvVar>,
) -> bool {
    let mut changed = false;
    let mut remove: Option<usize> = None;
    // Give the key ~40% of the free width so it grows with the panel instead of
    // staying a fixed sliver next to the filling value (see `split_key_width`).
    let key_w = super::widgets::split_key_width(ui, 42.0);
    egui::Grid::new("env_vars")
        .num_columns(2)
        .spacing([8.0, 4.0])
        // Unstriped — see `widgets::kv_editor`.
        .min_col_width(0.0)
        .show(ui, |ui| {
            for i in 0..vars.len() {
                let source = vars[i].source;
                if super::widgets::sized_key(
                    ui,
                    key_w,
                    &mut vars[i].key,
                    s.gui_hint_key_upper,
                    theme.text,
                )
                .changed()
                {
                    changed = true;
                }
                // The value cell is the grid's last column so it stretches to
                // fill the panel; the remove ✕ is right-aligned inside it (see
                // the note in `widgets::kv_editor`).
                // `Align::Min` so the value lines up with its key rather than
                // sinking below it — see `widgets::kv_editor`.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if ui
                        .button(RichText::new(super::icons::CLOSE).color(theme.err))
                        .clicked()
                    {
                        remove = Some(i);
                    }
                    match source_label(source) {
                        None => {
                            // Literal: editable value.
                            // Environment values are tokens and URLs — the
                            // longest strings in the app — so they wrap.
                            if super::widgets::wrapping_field(
                                ui,
                                ui.available_width(),
                                &mut vars[i].value,
                                s.gui_hint_value,
                                theme.text,
                            )
                            .changed()
                            {
                                vars[i].raw = vars[i].value.clone();
                                vars[i].modified = true;
                                changed = true;
                            }
                        }
                        Some(provider) => {
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!("{{{{ {provider} }}}}"))
                                            .color(theme.subst),
                                    );
                                    if vars[i].loading {
                                        ui.spinner();
                                        ui.colored_label(theme.pending, s.gui_resolving);
                                    } else if vars[i].resolved {
                                        ui.colored_label(theme.dim, "••••••");
                                    } else {
                                        ui.colored_label(theme.err, s.gui_unresolved);
                                    }
                                },
                            );
                        }
                    }
                });
                ui.end_row();
            }
        });
    if let Some(i) = remove {
        vars.remove(i);
        changed = true;
    }
    if ui.button(s.gui_add_variable).clicked() {
        vars.push(EnvVar::user(String::new(), String::new()));
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every string the frame painted, so a row can be checked for by the name
    /// the user reads rather than by poking at internal state.
    fn painted_text(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
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

    /// A real screen rect matters: the list lives in a `ScrollArea`, which culls
    /// anything it believes is offscreen.
    fn draw(app: &mut GuiApp) -> Vec<String> {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(320.0, 600.0),
        ));
        let out = ctx.run_ui(input, |panel| super::ui(app, panel));
        painted_text(&out.shapes)
    }

    fn draw_with_workspace_file_expanded(app: &mut GuiApp, path: &std::path::Path) -> Vec<String> {
        let ctx = egui::Context::default();
        let id = egui::Id::new(("env-path", path.to_path_buf()));
        let mut state =
            egui::collapsing_header::CollapsingState::load_with_default_open(&ctx, id, false);
        state.set_open(true);
        state.store(&ctx);
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(320.0, 600.0),
        ));
        let out = ctx.run_ui(input, |panel| super::ui(app, panel));
        painted_text(&out.shapes)
    }

    fn app_with_workspace(tag: &str) -> (GuiApp, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "paperboy_gui_envs_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dev.vars"), "TOKEN=t\n").unwrap();
        std::fs::write(
            dir.join("Prod AU.json"),
            r#"{"environment":{"name":"Prod AU","values":[{"key":"url","value":"https://x"}]}}"#,
        )
        .unwrap();
        // A Postman *collection* shares the `.json` extension and must not be
        // mistaken for an environment.
        std::fs::write(
            dir.join("orders.json"),
            r#"{"info":{"name":"orders"},"item":[]}"#,
        )
        .unwrap();
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        (GuiApp::for_test(session), dir)
    }

    /// The panel shows the open workspace's environment files alongside the
    /// global ones, whether or not they have been opened yet.
    #[test]
    fn the_panel_merges_workspace_environment_files_with_the_global_ones() {
        let (mut app, dir) = app_with_workspace("merge");
        app.session
            .load_environment_text("hand-made".into(), "A=1\n", None, None);

        let names: Vec<String> = env_rows(&app).iter().map(|r| r.name.clone()).collect();
        assert_eq!(
            names,
            vec!["Prod AU", "dev", "hand-made"],
            "workspace files first, in tree order, then everything else"
        );
        assert!(
            env_rows(&app)[..2].iter().all(|r| r.workspace),
            "and the workspace ones are flagged, so they can be marked in the list"
        );

        let painted = draw(&mut app);
        for name in ["Prod AU", "dev", "hand-made"] {
            assert!(
                painted.iter().any(|t| t.contains(name)),
                "{name} should be drawn, painted: {painted:?}"
            );
        }
        assert!(
            !painted.iter().any(|t| t.contains("orders")),
            "a collection is not an environment"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The filter box narrows the list by name — the point of it being that a
    /// workspace can hold hundreds of environments.
    #[test]
    fn the_filter_box_narrows_the_list_to_matching_names() {
        let (mut app, dir) = app_with_workspace("filter");
        app.session
            .load_environment_text("hand-made".into(), "A=1\n", None, None);

        app.env_query = "PROD".into();
        assert_eq!(
            env_rows(&app)
                .iter()
                .map(|r| r.name.clone())
                .collect::<Vec<_>>(),
            vec!["Prod AU"],
            "case-insensitive substring match"
        );

        let painted = draw(&mut app);
        assert!(painted.iter().any(|t| t.contains("Prod AU")));
        assert!(
            !painted.iter().any(|t| t.contains("hand-made")),
            "the filtered-out rows are gone from the frame: {painted:?}"
        );

        app.env_query = "zzz".into();
        assert!(env_rows(&app).is_empty());
        let painted = draw(&mut app);
        assert!(
            painted
                .iter()
                .any(|t| t.contains(app.strings.gui_env_filter_no_matches)),
            "and an empty result says so rather than looking like an empty panel: {painted:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The GUI uses the shared source selector before drawing, so its helper
    /// narrows the same rows the visible panel does.
    #[test]
    fn the_source_selector_narrows_the_gui_environment_rows() {
        let (mut app, dir) = app_with_workspace("source");
        app.session
            .load_environment_text("hand-made".into(), "A=1\n", None, None);

        app.session.env_source = crate::env_panel::EnvSource::Workspace;
        assert_eq!(
            env_rows(&app)
                .iter()
                .map(|r| (r.name.as_str(), r.workspace))
                .collect::<Vec<_>>(),
            vec![("Prod AU", true), ("dev", true)]
        );

        app.session.env_source = crate::env_panel::EnvSource::Global;
        assert_eq!(
            env_rows(&app)
                .iter()
                .map(|r| (r.name.as_str(), r.workspace))
                .collect::<Vec<_>>(),
            vec![("hand-made", false)]
        );

        app.env_query = "made".into();
        assert_eq!(
            env_rows(&app)
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            vec!["hand-made"],
            "source and name filters compose"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Opening a listed workspace file turns its row into the environment it
    /// became, rather than adding a second row for the same file.
    #[test]
    fn opening_a_workspace_environment_replaces_its_row_instead_of_duplicating_it() {
        let (mut app, dir) = app_with_workspace("open");
        let path = dir.join("dev.vars");
        let id = app.session.open_workspace_environment(&path).unwrap();

        let rows = env_rows(&app);
        assert_eq!(rows.len(), 2, "still one row per file: {rows:?}");
        let dev = rows.iter().find(|r| r.name == "dev").unwrap();
        assert!(dev.workspace && dev.env_id() == Some(id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Workspace environment files that have not been opened yet still present
    /// as expandable rows, so the affordance does not appear only after use.
    #[test]
    fn unopened_workspace_environment_rows_are_drawn_with_a_caret() {
        let (mut app, dir) = app_with_workspace("unopened-caret");

        let painted = draw(&mut app);
        assert!(
            painted
                .iter()
                .any(|t| t == super::super::icons::CARET_RIGHT),
            "an unopened workspace environment should show a collapsed caret: {painted:?}"
        );
        assert!(
            painted.iter().any(|t| t.contains("Prod AU")),
            "the workspace environment row should be visible: {painted:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Expanding an unopened workspace environment is the load gesture: the row
    /// becomes a loaded environment instead of first changing into a different
    /// kind of row that needs a second click.
    #[test]
    fn expanding_an_unopened_workspace_environment_loads_the_file() {
        let (mut app, dir) = app_with_workspace("expand-loads");
        assert!(app.session.global_envs.is_empty());

        let _ = draw_with_workspace_file_expanded(&mut app, &dir.join("Prod AU.json"));

        assert_eq!(app.session.global_envs.len(), 1);
        let rows = env_rows(&app);
        let prod = rows.iter().find(|r| r.name == "Prod AU").unwrap();
        assert!(
            prod.workspace && prod.env_id().is_some(),
            "the clicked workspace row should now be loaded: {rows:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Loaded environment rows always show the disclosure affordance: right
    /// when collapsed, down when a reveal opens the row.
    #[test]
    fn loaded_environment_rows_keep_a_caret_before_and_after_expanding() {
        let mut app = GuiApp::for_test(crate::session::Session::default());
        let id = app
            .session
            .load_environment_text("dev".into(), "TOKEN=t\n", None, None)
            .unwrap();

        let painted = draw(&mut app);
        assert!(
            painted
                .iter()
                .any(|t| t == super::super::icons::CARET_RIGHT),
            "a never-opened row should still show a collapsed caret: {painted:?}"
        );

        app.reveal_env = Some(id);
        let painted = draw(&mut app);
        assert!(
            painted.iter().any(|t| t == super::super::icons::CARET_DOWN),
            "an opened row should show the expanded caret: {painted:?}"
        );
    }

    /// The position of the first painted text shape containing `needle`, so a
    /// test can click the row the user would click.
    fn text_pos(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<egui::Pos2> {
        fn walk(shape: &egui::epaint::Shape, needle: &str, out: &mut Option<egui::Pos2>) {
            match shape {
                egui::epaint::Shape::Text(t) if out.is_none() => {
                    if t.galley.text().contains(needle) {
                        *out = Some(t.pos + t.galley.size() / 2.0);
                    }
                }
                egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| walk(s, needle, out)),
                _ => {}
            }
        }
        let mut out = None;
        for c in shapes {
            walk(&c.shape, needle, &mut out);
        }
        out
    }

    /// Drive the panel across several frames on one `Context`, so pointer state
    /// (and any menu it opens) survives from frame to frame the way it does in
    /// a running app. Returns the text painted by the final frame.
    fn drive(
        app: &mut GuiApp,
        frames: &[Vec<egui::Event>],
    ) -> (Vec<String>, Vec<egui::epaint::ClippedShape>) {
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        let mut painted = Vec::new();
        let mut time = 0.0;
        for events in frames {
            let mut input = egui::RawInput::default();
            input.time = Some(time);
            time += 0.05;
            input.screen_rect = Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(320.0, 600.0),
            ));
            input.events = events.clone();
            let out = ctx.run_ui(input, |panel| super::ui(app, panel));
            painted = painted_text(&out.shapes);
            shapes = out.shapes;
        }
        (painted, shapes)
    }

    fn click_at(pos: egui::Pos2, button: egui::PointerButton, count: usize) -> Vec<egui::Event> {
        let mut ev = vec![egui::Event::PointerMoved(pos)];
        for _ in 0..count {
            ev.push(egui::Event::PointerButton {
                pos,
                button,
                pressed: true,
                modifiers: Default::default(),
            });
            ev.push(egui::Event::PointerButton {
                pos,
                button,
                pressed: false,
                modifiers: Default::default(),
            });
        }
        ev
    }

    /// Switching environments is the thing this panel is most often opened to
    /// do, and its buttons live inside the collapsing body — so doing it used
    /// to mean expanding a row, clicking Active, and collapsing it again. The
    /// header answers the gesture directly.
    #[test]
    fn double_clicking_a_row_activates_that_environment() {
        let (mut app, dir) = app_with_workspace("dblclick");
        app.session
            .load_environment_text("hand-made".into(), "A=1\n", None, None);
        assert_eq!(
            app.session.active_env_id, None,
            "nothing active to begin with"
        );

        let (_, shapes) = drive(&mut app, &[vec![]]);
        let pos = text_pos(&shapes, "hand-made").expect("the row is painted");
        drive(
            &mut app,
            &[vec![], click_at(pos, egui::PointerButton::Primary, 2)],
        );

        let id = app
            .session
            .global_envs
            .iter()
            .find(|e| e.name == "hand-made")
            .map(|e| e.id);
        assert_eq!(
            app.session.active_env_id, id,
            "the row the user double-clicked is the one that became active"
        );

        // And again turns it back off: the underlying action is a toggle, so
        // the gesture must not activate-only and leave no way back.
        drive(
            &mut app,
            &[vec![], click_at(pos, egui::PointerButton::Primary, 2)],
        );
        assert_eq!(app.session.active_env_id, None, "double-click toggles");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The right-click menu offers the row's actions without expanding it, and
    /// names them by what the click will *do* — an active row offers to
    /// deactivate, not to "Active".
    #[test]
    fn right_clicking_a_row_offers_its_actions_and_names_them_by_effect() {
        let (mut app, dir) = app_with_workspace("ctxmenu");
        app.session
            .load_environment_text("hand-made".into(), "A=1\n", None, None);

        let (_, shapes) = drive(&mut app, &[vec![]]);
        let pos = text_pos(&shapes, "hand-made").expect("the row is painted");

        let (painted, _) = drive(
            &mut app,
            &[
                vec![],
                click_at(pos, egui::PointerButton::Secondary, 1),
                vec![],
            ],
        );
        for label in ["Activate", "Link to this collection", "Save…", "Delete"] {
            assert!(
                painted.iter().any(|t| t == label),
                "the menu should offer {label:?}, painted: {painted:?}"
            );
        }
        assert!(
            !painted.iter().any(|t| t == "Deactivate"),
            "an inactive row offers to activate, not to deactivate: {painted:?}"
        );

        // Make it active, and the same menu inverts.
        let id = app
            .session
            .global_envs
            .iter()
            .find(|e| e.name == "hand-made")
            .map(|e| e.id);
        app.session.set_active_env(id);
        let (painted, _) = drive(
            &mut app,
            &[
                vec![],
                click_at(pos, egui::PointerButton::Secondary, 1),
                vec![],
            ],
        );
        assert!(
            painted.iter().any(|t| t == "Deactivate"),
            "an active row offers the way back: {painted:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
