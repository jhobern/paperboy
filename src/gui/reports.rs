//! Centre panel in "Reports" view: a read-only list of the report tabs carried
//! in the shared session state, and their captured text.
//!
//! The interactive PaperTrail node editor (the Scratch-like editor) is
//! deliberately **out of scope** here — that is step 2. This panel only lists
//! and views the reports the session already holds so they are never lost when
//! a session is saved from the GUI.

use eframe::egui::{self, RichText};

use super::app::GuiApp;

pub fn ui(app: &mut GuiApp, ui: &mut egui::Ui) {
    let theme = app.theme;
    let (lbl_reports, lbl_close, lbl_note, lbl_no_reports, lbl_path, lbl_empty) = {
        let s = &app.strings;
        (
            s.gui_reports,
            s.gui_close_reports,
            s.gui_papertrail_note,
            s.gui_no_reports,
            s.gui_report_path,
            s.gui_empty_paren,
        )
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(lbl_reports).strong().color(theme.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button(format!("{} {}", super::icons::CLOSE, lbl_close))
                .clicked()
            {
                app.show_reports = false;
            }
        });
    });
    ui.colored_label(theme.dim, lbl_note);
    ui.separator();

    if app.session.reports.is_empty() {
        ui.add_space(8.0);
        ui.colored_label(theme.dim, lbl_no_reports);
        return;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for report in &app.session.reports {
                egui::CollapsingHeader::new(RichText::new(&report.name).strong().color(theme.text))
                    .id_salt(("report", &report.name))
                    .show(ui, |ui| {
                        if let Some(path) = &report.path {
                            ui.colored_label(theme.dim, format!("{lbl_path} {path}"));
                        }
                        if report.text.is_empty() {
                            ui.colored_label(theme.dim, lbl_empty);
                        } else {
                            let mut text = report.text.clone();
                            ui.add(
                                egui::TextEdit::multiline(&mut text)
                                    .code_editor()
                                    .desired_width(f32::INFINITY)
                                    .interactive(false),
                            );
                        }
                    });
            }
        });
}
