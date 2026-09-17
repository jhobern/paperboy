//! Left-bottom panel: Global Environments — activate one for substitution,
//! link one to the active collection, edit variables, and load/save `.vars`
//! files. A *resolved* secret value is shown but never editable (its provider
//! reference is the source of truth); one that failed to resolve is editable,
//! so a value can be pasted in when the provider isn't available.

use eframe::egui::{self, RichText};

use crate::environment::{EnvVar, PendingSecret, ValueSource, spawn_resolution};
use crate::i18n::Strings;

use super::app::{Dialog, EnvPanelTab, GuiApp, OpenKind, PromptKind, SaveKind};

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
        crate::env_panel::EnvSource::Both => s.gui_env_source_all,
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

/// Reveal the active environment: widen whichever filters are hiding it (see
/// [`crate::env_panel::reveal_plan`], which the terminal UI's `g` key follows
/// too), then ask the list to expand and scroll to its row.
pub(super) fn goto_active(app: &mut GuiApp, id: u64) {
    let files = workspace_files(app);
    let Some(plan) = crate::env_panel::reveal_plan(
        &app.session.global_envs,
        &files,
        &app.env_query,
        effective_source(app),
        id,
    ) else {
        return;
    };
    if plan.clear_filter {
        app.env_query.clear();
    }
    if plan.widen_source {
        app.session.env_source = crate::env_panel::EnvSource::Both;
        app.session.save();
    }
    app.reveal_env = Some(id);
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
            s.gui_delete,
            s.gui_save_ellipsis,
        )
    };
    // The header right-click menu's labels, which say what the click will *do*
    // rather than what the row currently *is* (the body's buttons are toggles
    // showing state, so "Active" reads correctly there and wouldn't here).
    let (lbl_activate, lbl_deactivate) = {
        let s = &app.strings;
        (s.gui_env_menu_activate, s.gui_env_menu_deactivate)
    };

    super::widgets::panel_header(ui, &theme, lbl_environments, |ui| {
        // "Take me back to the active one." A workspace of a few hundred
        // environments buries it, and the row people most often want is the one
        // currently in effect. Shown only when something is active, so the
        // button never sits there with nowhere to go.
        if let Some(id) = app.session.collections[ci].env_id {
            if ui
                .button(super::icons::GOTO_ACTIVE)
                .on_hover_text(app.strings.gui_env_goto_active_tooltip)
                .clicked()
            {
                goto_active(app, id);
            }
        }
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

    // Two questions, two tabs: which environments there are, and what a request
    // will actually substitute. The second used to be a strip docked along the
    // bottom of the first, which spent panel height the list could not spare
    // and showed a short window on to a list that is often longer than it.
    ui.horizontal(|ui| {
        for tab in [EnvPanelTab::Environments, EnvPanelTab::Variables] {
            let (label, hint) = match tab {
                EnvPanelTab::Environments => (lbl_environments, app.strings.gui_env_tab_envs_hint),
                EnvPanelTab::Variables => {
                    (app.strings.vars_heading, app.strings.gui_env_tab_vars_hint)
                }
            };
            if super::widgets::selectable(ui, app.env_panel_tab == tab, label)
                .on_hover_text(hint)
                .clicked()
            {
                app.env_panel_tab = tab;
            }
        }
    });
    ui.separator();

    if app.env_panel_tab == EnvPanelTab::Variables {
        variables_tab(app, ui);
        return;
    }

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
    // The environment active on *this tab* — the only one its requests
    // substitute from.
    let active = app.session.collections[ci].env_id;

    // Which of the listed variables a capture is currently shadowing. Computed
    // once, up here, because the editor below borrows `global_envs` mutably and
    // could not reach `collections` to ask.
    let overridden: std::collections::HashSet<String> = {
        let col = &app.session.collections[ci];
        app.session
            .global_envs
            .iter()
            .flat_map(|e| e.vars.iter().map(|v| v.key.clone()))
            .filter(|k| crate::vars_view::env_var_overridden(col, k))
            .collect()
    };

    // One-shot: consumed on the frame that shows the row, so a later manual
    // collapse isn't fought by a request that never expires.
    let reveal_target = app.reveal_env;
    let mut activate: Option<u64> = None;
    let mut delete: Option<u64> = None;
    let mut save: Option<u64> = None;
    let mut resolve: Option<(u64, Vec<PendingSecret>)> = None;
    let mut open_file: Option<std::path::PathBuf> = None;
    let mut changed = false;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Truncate long environment names so they can't report a content
            // width wider than the panel (see the note in `requests.rs` — an
            // over-wide panel leaves an unpainted strip while being dragged).
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            // Same rhythm as the request and workspace trees: this is a list of
            // rows too, and it sat beneath them looking twice as airy.
            super::widgets::tree_rhythm(ui);
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
                        let (edited, pending) = var_editor(
                            ui,
                            &theme,
                            &app.strings,
                            &mut app.session.global_envs[idx].vars,
                            &overridden,
                        );
                        if edited {
                            changed = true;
                        }
                        // A pasted value that is itself an `op://`/`ssm:`
                        // reference needs resolving like any other, or it sits
                        // at "resolving..." forever.
                        if !pending.is_empty() {
                            resolve = Some((id, pending));
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
        app.session.set_tab_env(ci, Some(id));
    }
    if let Some(id) = delete {
        app.session.delete_environment(id);
    }
    if let Some(id) = save {
        super::menu::save_via_picker(app, SaveKind::Environment(id));
    }
    if let Some((id, pending)) = resolve {
        app.session.pending_env.push(spawn_resolution(id, pending));
    }
    if changed {
        for col in &mut app.session.collections {
            col.invalidate_request_json();
        }
    }
}

/// A group heading inside the Variables tab, with a rule running out to the
/// right of the label so the two groups read as sections rather than as two
/// bold rows adrift in a list of variables. The terminal UI's popup centres its
/// labels between two rules for the same reason; a rule on one side is enough
/// here, where the rows are a grid rather than full-width highlighted lines.
fn vars_group_header(ui: &mut egui::Ui, theme: &super::theme::GuiTheme, label: &str, hint: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(theme.text).strong())
            .on_hover_text(hint);
        // Claimed rather than painted over the free space, so the rule is part
        // of the layout and nothing can land on top of it later.
        let rest = ui.available_size_before_wrap().x;
        if rest > 0.0 {
            let (_, rect) = ui.allocate_space(egui::vec2(rest, 1.0));
            ui.painter().hline(
                rect.x_range(),
                rect.center().y,
                egui::Stroke::new(1.0, theme.line),
            );
        }
    });
}

/// One environment variable as the Variables tab shows it: flattened out of
/// [`EnvVar`] so the borrow of `app.session` can end before the tab's Reveal
/// toggle needs `app` mutably.
struct VarRow {
    key: String,
    dot: egui::Color32,
    value: String,
    value_color: egui::Color32,
    copy: String,
    overridden: bool,
}

/// Everything a request on the active tab will substitute, in the precedence
/// the runner applies (see `request::collection_vars`): the bound environment's
/// variables, **overridden by** the live capture pool.
///
/// The same question the terminal UI answers with `v`, laid out the same way —
/// keep the two in step. It is a tab rather than a strip along the bottom of
/// the environments list because both halves are lists that want the panel's
/// full height, and because the answer is about *this tab*, not about whichever
/// environment happens to be selected above it.
///
/// Read-only. Environment values are edited one tab across, where the
/// environment owning them is on screen; captures are not editable anywhere,
/// being whatever the last response yielded.
fn variables_tab(app: &mut GuiApp, ui: &mut egui::Ui) {
    let theme = app.theme;
    let ci = app.active_ci();
    let env = app.session.collections[ci]
        .env_id
        .and_then(|id| app.session.global_envs.iter().find(|e| e.id == id));
    let captures = crate::vars_view::capture_rows(&app.session.collections[ci], env);
    // Copied out rather than borrowed, so the borrow of `app.session` ends
    // before the Reveal toggle below needs `app` mutably.
    let vars: Vec<VarRow> = env
        .map(|e| {
            e.vars
                .iter()
                .map(|v| VarRow {
                    key: v.key.clone(),
                    // Status dot colour-matched to the request substitution
                    // scheme, as the terminal UI's popup does it: orange =
                    // loading, cyan = literal, green = loaded from a provider,
                    // red = failed to resolve.
                    dot: if v.loading {
                        theme.pending
                    } else if !v.resolved {
                        theme.err
                    } else if v.source == ValueSource::Literal {
                        theme.subst
                    } else {
                        theme.ok
                    },
                    value: if v.loading {
                        app.strings.env_loading.to_string()
                    } else {
                        v.display_value()
                    },
                    value_color: if v.resolved {
                        theme.text
                    } else {
                        theme.pending
                    },
                    // A masked secret is masked in `display_value`, so what is
                    // on screen is already safe to copy verbatim.
                    copy: v.display_value(),
                    overridden: crate::vars_view::env_var_overridden(
                        &app.session.collections[ci],
                        &v.key,
                    ),
                })
                .collect()
        })
        .unwrap_or_default();

    let s = &app.strings;
    let (
        lbl_env_group,
        hint_env_group,
        lbl_capture_group,
        hint_capture_group,
        lbl_none,
        lbl_no_captures,
        lbl_overridden,
        tip_overridden,
        lbl_shadows,
        lbl_copy,
        lbl_reveal,
        tip_reveal,
    ) = (
        s.vars_group_env,
        s.gui_env_group_hint,
        s.gui_captures_heading,
        s.gui_captures_heading_hint,
        s.vars_none,
        s.gui_no_captures_yet,
        s.vars_overridden,
        s.gui_overridden_tooltip,
        s.vars_shadows_env,
        s.gui_probe_copy_this,
        s.gui_reveal,
        s.gui_reveal_hint,
    );

    // The Reveal toggle sits above both groups rather than beside the Captures
    // heading: it is the tab's one control, and putting it on a heading that
    // scrolls away would take it off screen just as a long list needs it.
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if super::widgets::selectable(ui, app.vars_reveal, lbl_reveal)
                .on_hover_text(tip_reveal)
                .clicked()
            {
                app.vars_reveal = !app.vars_reveal;
            }
        });
    });
    let reveal = app.vars_reveal;
    let mut to_copy: Option<String> = None;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            vars_group_header(ui, &theme, lbl_env_group, hint_env_group);
            if vars.is_empty() {
                ui.colored_label(theme.dim, lbl_none);
            } else {
                egui::Grid::new("env-vars-grid")
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        for row in &vars {
                            ui.horizontal(|ui| {
                                ui.colored_label(row.dot, "\u{25cf}");
                                ui.label(RichText::new(&row.key).color(theme.text));
                            });
                            ui.horizontal(|ui| {
                                let label =
                                    ui.label(RichText::new(&row.value).color(row.value_color));
                                // A capture of this name wins, so the value
                                // beside it is not the one that gets sent.
                                // Saying so is the whole point of listing both
                                // groups together: without the note the row is
                                // not merely incomplete, it is wrong.
                                if row.overridden {
                                    ui.label(RichText::new(lbl_overridden).color(theme.pending))
                                        .on_hover_text(tip_overridden);
                                }
                                label.context_menu(|ui| {
                                    if ui.button(lbl_copy).clicked() {
                                        to_copy = Some(row.copy.clone());
                                        ui.close();
                                    }
                                });
                            });
                            ui.end_row();
                        }
                    });
            }

            ui.add_space(6.0);
            vars_group_header(ui, &theme, lbl_capture_group, hint_capture_group);
            if captures.is_empty() {
                // Said rather than left out: "nothing has been captured" is an
                // answer, where a missing group reads as a missing feature.
                ui.colored_label(theme.dim, lbl_no_captures);
                return;
            }
            egui::Grid::new("env-captures-grid")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    for row in &captures {
                        ui.horizontal(|ui| {
                            // Green, matching `SubstKind::Loaded`: a capture is
                            // a value resolved from a live response.
                            ui.colored_label(theme.ok, "\u{25cf}");
                            ui.label(RichText::new(&row.key).color(theme.text));
                        });
                        ui.horizontal(|ui| {
                            let label = ui.label(
                                RichText::new(crate::vars_view::shown_value(&row.value, reveal))
                                    .color(theme.ok),
                            );
                            if row.shadows_env {
                                ui.label(RichText::new(lbl_shadows).color(theme.pending));
                            }
                            // Copying yields the real value even while masked,
                            // the way the Response pane's Captures tab does: a
                            // value nobody can retrieve would defeat the point
                            // of listing it.
                            label.context_menu(|ui| {
                                if ui.button(lbl_copy).clicked() {
                                    to_copy = Some(row.value.clone());
                                    ui.close();
                                }
                            });
                        });
                        ui.end_row();
                    }
                });
        });
    if let Some(v) = to_copy {
        ui.ctx().copy_text(v);
    }
}

/// Editable variable table for one environment. Literal values are editable in
/// place; a secret-backed value that resolved shows its provider and a mask,
/// since the `.vars` reference is the source of truth and there is nothing
/// useful to type over it.
///
/// One that *failed* to resolve is editable: the reference is a source of truth
/// for nothing, and there may be no way to make the provider work from here. An
/// `op://`/`ssm:` value typed in is kept in memory only, as when editing a
/// resolved secret in the TUI - the reference stays in `raw`, and the pasted
/// value is never written to the plaintext state file.
///
/// Returns whether anything changed, and any pasted value that is itself a
/// provider reference and so needs background resolution.
fn var_editor(
    ui: &mut egui::Ui,
    theme: &super::theme::GuiTheme,
    s: &Strings,
    vars: &mut Vec<EnvVar>,
    overridden: &std::collections::HashSet<String>,
) -> (bool, Vec<PendingSecret>) {
    let mut changed = false;
    let mut pending: Vec<PendingSecret> = Vec::new();
    let mut remove: Option<usize> = None;
    // Give the key ~40% of the free width so it grows with the panel instead of
    // staying a fixed sliver next to the filling value (see `split_key_width`).
    let key_w = super::widgets::split_key_width(ui, 42.0);
    let x_w = super::widgets::remove_width(ui);
    let row_h = ui.spacing().interact_size.y;
    super::widgets::table_rows(ui, |ui| {
        for i in 0..vars.len() {
            // Top-aligned, explicit columns rather than a grid: a grid centres
            // each cell against a row height it only learns as cells are added,
            // so every cell sat a fraction lower than the one before it and the
            // table looked as though it sloped (see `widgets::table_row`).
            super::widgets::table_row(ui, |ui| {
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
                // The value takes what the remove ✕ leaves.
                let val_w = (ui.available_width() - x_w - 8.0).max(40.0);
                match source_label(source) {
                    None => {
                        // Literal: editable value.
                        // Environment values are tokens and URLs — the
                        // longest strings in the app — so they wrap.
                        if super::widgets::wrapping_field(
                            ui,
                            val_w,
                            &mut vars[i].value,
                            s.gui_hint_value,
                            theme.text,
                        )
                        .changed()
                        {
                            // `.vars` is a line-per-variable format, so a
                            // pasted multi-line secret has to be flattened
                            // here rather than truncated on the way to disk.
                            vars[i].value = crate::environment::flatten_value(&vars[i].value);
                            vars[i].raw = vars[i].value.clone();
                            vars[i].modified = true;
                            changed = true;
                        }
                    }
                    Some(provider) => {
                        ui.allocate_ui_with_layout(
                            egui::vec2(val_w, row_h),
                            egui::Layout::left_to_right(egui::Align::Min),
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
                                    // `value` still holds the reference until
                                    // it is typed over, so comparing against
                                    // `raw` keeps an untouched row from
                                    // committing anything.
                                    ui.colored_label(theme.err, s.gui_unresolved);
                                    let w = ui.available_width().max(60.0);
                                    if super::widgets::wrapping_field(
                                        ui,
                                        w,
                                        &mut vars[i].value,
                                        s.gui_hint_value,
                                        theme.text,
                                    )
                                    .lost_focus()
                                        && vars[i].value != vars[i].raw
                                    {
                                        let v = vars[i].value.clone();
                                        let keep = vars[i].is_secret_source();
                                        if let Some(p) = vars[i].set_user_value_secrecy(v, keep, i)
                                        {
                                            pending.push(p);
                                        }
                                        changed = true;
                                    }
                                }
                            },
                        );
                    }
                }
                // A capture of the same name wins over this row (see
                // `request::collection_vars`), so the value shown beside it is
                // not the value that gets sent. Without the note the row is not
                // merely incomplete, it is wrong — which is what it was before
                // the Captures section below existed to be pointed at.
                if overridden.contains(&vars[i].key) {
                    ui.colored_label(theme.pending, s.vars_overridden)
                        .on_hover_text(s.gui_overridden_tooltip);
                }
                if super::widgets::flat_buttons(ui, |ui| {
                    ui.add_sized(
                        [x_w, row_h],
                        egui::Button::new(RichText::new(super::icons::CLOSE).color(theme.err)),
                    )
                })
                .clicked()
                {
                    remove = Some(i);
                }
            });
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
    (changed, pending)
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

    fn app_with_captures(captures: &[(&str, &str)], vars: &str) -> GuiApp {
        let mut session = crate::session::Session::default();
        session.collections.clear();
        session.collections.push(crate::collection::Collection::new(
            "api".to_string(),
            vec![],
        ));
        session.active_tab = 0;
        if !vars.is_empty() {
            let id = session.load_environment_text("dev".into(), vars, None, None);
            session.collections[0].env_id = id;
        }
        for (k, v) in captures {
            session.collections[0]
                .captures
                .insert((*k).to_string(), (*v).to_string());
        }
        GuiApp::for_test(session)
    }

    /// The panel listed the environment's rows while substitution reads the
    /// environment **overridden by** the capture pool, so a captured value was
    /// not merely unlisted — it left the environment row showing a value that
    /// is not the one sent.
    #[test]
    fn the_panel_lists_the_live_capture_pool() {
        let mut app = app_with_captures(&[("session", "abc123")], "");
        app.env_panel_tab = EnvPanelTab::Variables;
        let painted = draw(&mut app);
        assert!(
            painted.iter().any(|t| t == "session"),
            "the capture is listed: {painted:?}"
        );
        assert!(
            painted.iter().any(|t| t == crate::environment::SECRET_MASK),
            "masked until asked for: {painted:?}"
        );
        assert!(
            !painted.iter().any(|t| t.contains("abc123")),
            "the value must not be on screen unasked: {painted:?}"
        );

        app.vars_reveal = true;
        let painted = draw(&mut app);
        assert!(
            painted.iter().any(|t| t == "abc123"),
            "Reveal shows it: {painted:?}"
        );
    }

    /// The section is always there, so "nothing has been captured" is a visible
    /// answer rather than a missing section that reads as a missing feature.
    #[test]
    fn the_captures_section_is_there_even_with_nothing_in_it() {
        let mut app = app_with_captures(&[], "");
        app.env_panel_tab = EnvPanelTab::Variables;
        let empty = app.strings.gui_no_captures_yet.to_string();
        let painted = draw(&mut app);
        assert!(
            painted
                .iter()
                .any(|t| t.starts_with(app.strings.gui_captures_heading)),
            "the heading is always shown: {painted:?}"
        );
        assert!(
            painted.contains(&empty),
            "and says it is empty: {painted:?}"
        );
    }

    /// The environment row a capture shadows is the one the panel was actively
    /// misleading about.
    #[test]
    fn an_environment_row_a_capture_shadows_is_marked() {
        let mut app = app_with_captures(
            &[],
            "TOKEN=from-env
",
        );
        let marker = app.strings.vars_overridden.to_string();
        app.reveal_env = app.session.global_envs.first().map(|e| e.id);
        let painted = draw(&mut app);
        assert!(
            !painted.contains(&marker),
            "nothing is shadowing it yet: {painted:?}"
        );

        app.session.collections[0]
            .captures
            .insert("TOKEN".to_string(), "from-capture".to_string());
        app.reveal_env = app.session.global_envs.first().map(|e| e.id);
        let painted = draw(&mut app);
        assert!(
            painted.contains(&marker),
            "the row whose value is no longer the one sent must say so: {painted:?}"
        );
    }

    /// The two halves of "what will this request substitute?" are both on the
    /// Variables tab and in precedence order, the way the terminal UI's `v`
    /// popup shows them: an environment row is only half an answer once a
    /// capture of that name exists.
    #[test]
    fn the_variables_tab_shows_both_groups_in_precedence_order() {
        let mut app = app_with_captures(&[("TOKEN", "from-capture")], "TOKEN=from-env\n");
        app.env_panel_tab = EnvPanelTab::Variables;
        app.vars_reveal = true;
        let (env_group, capture_group, overridden, shadows) = {
            let s = &app.strings;
            (
                s.vars_group_env.to_string(),
                s.gui_captures_heading.to_string(),
                s.vars_overridden.to_string(),
                s.vars_shadows_env.to_string(),
            )
        };
        let painted = draw(&mut app);
        let at = |needle: &str| painted.iter().position(|t| t == needle);

        let env_at = at(&env_group).unwrap_or_else(|| panic!("no env heading: {painted:?}"));
        let cap_at =
            at(&capture_group).unwrap_or_else(|| panic!("no captures heading: {painted:?}"));
        assert!(
            env_at < cap_at,
            "the environment comes first and the captures override it: {painted:?}"
        );
        assert!(
            painted.iter().any(|t| t == "from-env"),
            "the environment's own value is listed: {painted:?}"
        );
        assert!(
            painted.iter().any(|t| t == "from-capture"),
            "and the value that actually gets sent: {painted:?}"
        );
        // Both sides of the clash are marked, or the reader has two rows of the
        // same name and no way to tell which one wins.
        assert!(painted.contains(&overridden), "env row marked: {painted:?}");
        assert!(
            painted.contains(&shadows),
            "capture row marked: {painted:?}"
        );
    }

    /// The panel opens on the environments, which is what it is for; the
    /// variables are a tab across. Guards against a stray `Default` on the tab
    /// enum silently reversing that.
    #[test]
    fn the_panel_opens_on_the_environments_tab() {
        let mut app = app_with_captures(&[("session", "abc123")], "");
        assert_eq!(app.env_panel_tab, EnvPanelTab::Environments);
        let (envs_tab, vars_tab, captures_heading) = {
            let s = &app.strings;
            (
                s.gui_environments.to_string(),
                s.vars_heading.to_string(),
                s.gui_captures_heading.to_string(),
            )
        };
        let painted = draw(&mut app);
        // Both tab labels are on screen, so the variables are reachable...
        assert!(
            painted.iter().filter(|t| **t == envs_tab).count() >= 2,
            "panel header and its own tab: {painted:?}"
        );
        assert!(
            painted.contains(&vars_tab),
            "the Variables tab is offered: {painted:?}"
        );
        // ...but their contents are not stealing height from the list, which is
        // the whole reason the captures moved off the bottom of this tab.
        assert!(
            !painted.contains(&captures_heading),
            "the captures are not on this tab: {painted:?}"
        );
    }

    /// A `# [Gen]` value reaches the pool the way a capture does but must never
    /// be displayed — it may be an HMAC of a secret, and the figure on hand is
    /// the previous send's.
    #[test]
    fn a_computed_value_is_not_listed_among_the_captures() {
        let mut app = app_with_captures(&[("nonce", "deadbeef")], "");
        app.env_panel_tab = EnvPanelTab::Variables;
        app.session.collections[0]
            .entries
            .push(crate::hurl::HurlEntry {
                title: "gen".to_string(),
                generators: vec![("nonce".to_string(), "uuid".to_string())],
                ..Default::default()
            });
        app.vars_reveal = true;
        let painted = draw(&mut app);
        assert!(
            !painted.iter().any(|t| t.contains("deadbeef")),
            "a computed value must never be shown: {painted:?}"
        );
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

    /// The complaint: with several hundred environments the active one is
    /// impossible to find again. The button widens whatever is hiding it and
    /// asks the list to reveal it — the same rule the terminal UI's `g` follows.
    #[test]
    fn the_goto_active_button_reveals_the_active_environment() {
        let (mut app, dir) = app_with_workspace("gotoactive");
        app.session
            .load_environment_text("hand-made".into(), "A=1\n", None, None);
        let id = app.session.global_envs.last().unwrap().id;
        let ci = app.active_ci();
        app.session.collections[ci].env_id = Some(id);

        // Hidden twice over: by the text filter and by a workspace-only source.
        app.env_query = "PROD".into();
        app.session.env_source = crate::env_panel::EnvSource::Workspace;
        assert!(
            env_rows(&app).iter().all(|r| r.env_id() != Some(id)),
            "the precondition is that it is hidden"
        );

        goto_active(&mut app, id);

        assert!(app.env_query.is_empty(), "the text filter is cleared");
        assert_eq!(app.session.env_source, crate::env_panel::EnvSource::Both);
        assert_eq!(app.reveal_env, Some(id), "and the list is asked to show it");
        assert!(env_rows(&app).iter().any(|r| r.env_id() == Some(id)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing is disturbed when the active environment is already on show.
    #[test]
    fn the_goto_active_button_leaves_a_visible_row_alone() {
        let (mut app, dir) = app_with_workspace("gotovisible");
        app.session
            .load_environment_text("hand-made".into(), "A=1\n", None, None);
        let id = app.session.global_envs.last().unwrap().id;
        let ci = app.active_ci();
        app.session.collections[ci].env_id = Some(id);
        app.env_query = "hand".into();

        goto_active(&mut app, id);

        assert_eq!(app.env_query, "hand", "the user's filter is theirs to keep");
        assert_eq!(app.reveal_env, Some(id));
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

    /// Editability is visible in the paint: an editable field draws the whole
    /// reference as its contents, where the label beside it only ever says
    /// `{{ env }}`.
    #[test]
    fn a_provider_value_that_failed_to_resolve_is_editable() {
        let mut app = GuiApp::for_test(crate::session::Session::default());
        let id = app
            .session
            .load_environment_text(
                "dev".into(),
                "TOKEN={{ env:PB_TEST_DELIBERATELY_UNSET }}\n",
                None,
                None,
            )
            .unwrap();
        app.reveal_env = Some(id);

        let painted = draw(&mut app);

        assert!(
            painted
                .iter()
                .any(|t| t.contains("PB_TEST_DELIBERATELY_UNSET")),
            "the reference must be in an editable field, not just labelled: {painted:?}"
        );
        assert!(
            !painted.iter().any(|t| t.contains("••••••")),
            "nothing resolved, so there is no value to mask: {painted:?}"
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
        let ci = app.active_ci();
        assert_eq!(
            app.session.collections[ci].env_id, None,
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
            app.session.collections[ci].env_id, id,
            "the row the user double-clicked is the one that became active"
        );

        // And again turns it back off: the underlying action is a toggle, so
        // the gesture must not activate-only and leave no way back.
        drive(
            &mut app,
            &[vec![], click_at(pos, egui::PointerButton::Primary, 2)],
        );
        assert_eq!(
            app.session.collections[ci].env_id, None,
            "double-click toggles"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An environment belongs to a tab, so the panel has to answer for the tab
    /// you are *looking at*. Switching tabs must re-read it: the row marked
    /// active, and the action the menu offers, both follow the new tab rather
    /// than staying on whatever the last tab had.
    #[test]
    fn the_panel_follows_the_active_tab() {
        let (mut app, dir) = app_with_workspace("tabswitch");
        app.session
            .load_environment_text("hand-made".into(), "A=1\n", None, None);
        let id = app.session.global_envs.last().unwrap().id;
        let first = app.active_ci();
        app.session.set_tab_env(first, Some(id));

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
        assert!(
            painted.iter().any(|t| t == "Deactivate"),
            "on the tab that activated it, the row is active: {painted:?}"
        );

        // A second tab, which activated nothing. It has no workspace, so the
        // panel drops the source selector and the row moves — find it again.
        let second = app.session.add_collection("other");
        app.session.active_tab = second;
        let (_, shapes) = drive(&mut app, &[vec![]]);
        let pos = text_pos(&shapes, "hand-made").expect("the row is still painted");
        let (painted, _) = drive(
            &mut app,
            &[
                vec![],
                click_at(pos, egui::PointerButton::Secondary, 1),
                vec![],
            ],
        );
        assert!(
            painted.iter().any(|t| t == "Activate"),
            "the new tab has no environment, so the row offers to activate: {painted:?}"
        );
        assert!(
            !painted.iter().any(|t| t == "Deactivate"),
            "the other tab's choice must not show here: {painted:?}"
        );

        // Activating it here leaves the first tab exactly as it was.
        app.session.set_tab_env(second, Some(id));
        assert_eq!(app.session.collections[first].env_id, Some(id));
        assert_eq!(app.session.collections[second].env_id, Some(id));
        app.session.set_tab_env(second, None);
        assert_eq!(
            app.session.collections[first].env_id,
            Some(id),
            "turning it off on one tab must not turn it off on the other"
        );
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
        for label in ["Activate", "Save…", "Delete"] {
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
        let ci = app.active_ci();
        app.session.set_tab_env(ci, id);
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
