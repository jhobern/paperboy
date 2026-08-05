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

    super::widgets::panel_header(ui, &theme, lbl_environments, |ui| {
        if ui.button(lbl_load).on_hover_text(tip_load).clicked() {
            app.dialog = Some(Dialog::OpenFile {
                kind: OpenKind::Environment,
                path: String::new(),
                error: None,
            });
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
    ui.separator();

    // Collect ids up front so we can mutate the session inside the loop.
    let env_ids: Vec<u64> = app.session.global_envs.iter().map(|e| e.id).collect();
    let active = app.session.active_env_id;
    let linked = app.session.collections[ci].linked_env_id;

    let mut activate: Option<u64> = None;
    let mut link: Option<u64> = None;
    let mut delete: Option<u64> = None;
    let mut save: Option<u64> = None;
    let mut changed = false;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Truncate long environment names so they can't report a content
            // width wider than the panel (see the note in `requests.rs` — an
            // over-wide panel leaves an unpainted strip while being dragged).
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            if env_ids.is_empty() {
                ui.add_space(6.0);
                ui.colored_label(theme.dim, lbl_no_envs);
            }
            for id in env_ids {
                let idx = match app.session.global_envs.iter().position(|e| e.id == id) {
                    Some(i) => i,
                    None => continue,
                };
                let is_active = active == Some(id);
                let is_linked = linked == Some(id);
                let name = app.session.global_envs[idx].name.clone();
                let from_git = app.session.global_envs[idx].git_origin.is_some();
                let header = format!(
                    "{}{}{}",
                    if from_git {
                        format!("{} ", super::icons::GIT)
                    } else {
                        String::new()
                    },
                    name,
                    if is_active {
                        format!("  {}", super::icons::ACTIVE)
                    } else {
                        String::new()
                    }
                );

                let header_color = if is_active { theme.accent } else { theme.text };
                super::widgets::tree_header(
                    ui,
                    ("env", id),
                    false,
                    RichText::new(header).color(header_color),
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
            }
        });

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
        let path = app
            .session
            .global_envs
            .iter()
            .find(|e| e.id == id)
            .and_then(|e| e.path.as_ref())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        app.dialog = Some(Dialog::SaveFile {
            kind: SaveKind::Environment(id),
            path,
            error: None,
        });
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
        .striped(true)
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
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(RichText::new(super::icons::CLOSE).color(theme.err))
                        .clicked()
                    {
                        remove = Some(i);
                    }
                    match source_label(source) {
                        None => {
                            // Literal: editable value.
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut vars[i].value)
                                        .desired_width(f32::INFINITY)
                                        .hint_text(s.gui_hint_value),
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
