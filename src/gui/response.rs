//! Centre-bottom panel: the response viewer — status line, timing and section
//! tabs (Body, Headers, Asserts) over the shared response buffer.

use eframe::egui::{self, RichText};

use super::app::{GuiApp, ResponseSection};
use super::widgets::{self, status_color};

pub fn ui(app: &mut GuiApp, ui: &mut egui::Ui) {
    let theme = app.theme;
    let (
        lbl_sending,
        lbl_error,
        lbl_no_response,
        lbl_copy,
        lbl_copy_body,
        lbl_empty_body,
        lbl_no_headers,
        lbl_no_assertions,
        lbl_body,
        lbl_headers,
        lbl_asserts,
    ) = {
        let s = &app.strings;
        (
            s.sending,
            s.gui_error,
            s.gui_no_response_yet,
            s.gui_copy,
            s.gui_copy_body,
            s.gui_empty_body,
            s.gui_no_headers,
            s.gui_no_assertions,
            s.gui_sec_body,
            s.gui_sec_headers,
            s.gui_sec_asserts,
        )
    };

    // Snapshot the shared buffer so we don't hold the lock while drawing.
    let (status, status_text, body, loading, error, headers, asserts, duration) = {
        let r = app.session.response.lock().unwrap();
        (
            r.status,
            r.status_text.clone(),
            r.body.clone(),
            r.loading,
            r.error.clone(),
            r.headers.clone(),
            r.assert_results.clone(),
            r.duration_ms,
        )
    };

    // ── Status line ───────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        if loading {
            ui.spinner();
            ui.colored_label(theme.dim, lbl_sending);
        } else if !error.is_empty() {
            ui.colored_label(
                theme.err,
                format!("{} {}", super::icons::WARNING, lbl_error),
            );
        } else if status > 0 {
            let col = status_color(&theme, status);
            ui.label(
                RichText::new(format!("{status} {status_text}"))
                    .strong()
                    .color(col),
            );
        } else {
            ui.colored_label(theme.dim, lbl_no_response);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(ms) = duration {
                ui.colored_label(theme.dim, format!("{ms} ms"));
            }
            if !body.is_empty() {
                ui.colored_label(theme.dim, format!("{} B", body.len()));
                if ui.button(lbl_copy).on_hover_text(lbl_copy_body).clicked() {
                    ui.ctx().copy_text(body.to_string());
                }
            }
        });
    });

    if !error.is_empty() {
        ui.add_space(4.0);
        ui.colored_label(theme.err, error);
    }

    ui.add_space(2.0);
    let failed = asserts.iter().filter(|a| !a.passed).count();
    let assert_label = if asserts.is_empty() {
        lbl_asserts.to_string()
    } else if failed == 0 {
        format!("{lbl_asserts} {} ({})", super::icons::PASS, asserts.len())
    } else {
        format!(
            "{lbl_asserts} {} ({failed}/{})",
            super::icons::FAIL,
            asserts.len()
        )
    };
    widgets::section_tabs(
        ui,
        &theme,
        &mut app.response_section,
        &[
            (ResponseSection::Body, lbl_body),
            (ResponseSection::Headers, lbl_headers),
            (ResponseSection::Asserts, assert_label.as_str()),
        ],
    );
    ui.separator();

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| match app.response_section {
            ResponseSection::Body => {
                if body.is_empty() {
                    ui.colored_label(theme.dim, lbl_empty_body);
                } else {
                    let mut text = body.to_string();
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .desired_rows(12)
                            .interactive(false),
                    );
                }
            }
            ResponseSection::Headers => {
                if headers.is_empty() {
                    ui.colored_label(theme.dim, lbl_no_headers);
                } else {
                    egui::Grid::new("resp_headers")
                        .num_columns(2)
                        .spacing([12.0, 4.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for (k, v) in &headers {
                                ui.label(RichText::new(k).strong().color(theme.accent));
                                ui.label(RichText::new(v).monospace().color(theme.text));
                                ui.end_row();
                            }
                        });
                }
            }
            ResponseSection::Asserts => {
                if asserts.is_empty() {
                    ui.colored_label(theme.dim, lbl_no_assertions);
                } else {
                    for a in &asserts {
                        ui.horizontal(|ui| {
                            if a.passed {
                                ui.colored_label(theme.ok, super::icons::PASS);
                            } else {
                                ui.colored_label(theme.err, super::icons::FAIL);
                            }
                            ui.label(RichText::new(&a.expr).monospace().color(theme.text));
                        });
                        if !a.passed && !a.detail.is_empty() {
                            ui.horizontal(|ui| {
                                ui.add_space(18.0);
                                ui.colored_label(theme.err, &a.detail);
                            });
                        }
                    }
                }
            }
        });
}
