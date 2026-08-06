//! Centre panel in "Reports" view: the list of PaperTrail reports carried in
//! the shared session state. Selecting one opens it in the Scratch-style block
//! editor ([`super::report_editor`]); "New report" starts a fresh scratch one.

use std::path::PathBuf;

use eframe::egui::{self, RichText};

use super::app::GuiApp;
use super::report_editor::ReportOrigin;
use crate::persistence::PersistedReport;
use crate::report::Report;

pub fn ui(app: &mut GuiApp, ui: &mut egui::Ui) {
    let theme = app.theme;

    ui.horizontal(|ui| {
        ui.label(
            RichText::new(app.strings.gui_reports)
                .strong()
                .color(theme.text),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button(format!(
                    "{} {}",
                    super::icons::CLOSE,
                    app.strings.gui_close_reports
                ))
                .clicked()
            {
                app.show_reports = false;
            }
            if ui
                .button(format!(
                    "{} {}",
                    super::icons::PLUS,
                    app.strings.gui_new_report
                ))
                .clicked()
            {
                new_report(app);
            }
        });
    });
    ui.separator();

    if app.session.reports.is_empty() {
        ui.add_space(8.0);
        ui.colored_label(theme.dim, app.strings.gui_no_reports);
        return;
    }

    // Collect the index to open (if any) so the immutable list iteration doesn't
    // overlap mutating `app`.
    let mut open: Option<usize> = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, report) in app.session.reports.iter().enumerate() {
                let label = RichText::new(format!("{} {}", super::icons::REPORT, report.name))
                    .color(theme.text);
                if super::widgets::selectable(ui, false, label).clicked() {
                    open = Some(i);
                }
                if let Some(path) = &report.path {
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        ui.colored_label(
                            theme.dim,
                            format!("{} {path}", app.strings.gui_report_path),
                        );
                    });
                }
            }
        });

    if let Some(i) = open {
        open_report(app, i);
    }
}

/// Open the session report at index `i` in the block editor.
fn open_report(app: &mut GuiApp, i: usize) {
    let Some(pr) = app.session.reports.get(i) else {
        return;
    };
    let mut report = Report::from_text(pr.name.clone(), pr.text.clone());
    report.name = pr.name.clone();
    report.path = pr.path.as_ref().map(PathBuf::from);
    report.git_origin = pr.git_origin.clone();
    report.dirty = false;
    app.open_report_editor(ReportOrigin::Session(i), report);
    app.focus = super::Focus::Main;
}

/// Create a fresh scratch report, append it to the session, and open it.
fn new_report(app: &mut GuiApp) {
    let n = app.session.reports.len() + 1;
    let report = Report::scratch(format!("Report {n}"));
    app.session.reports.push(PersistedReport {
        name: report.name.clone(),
        text: report.text.clone(),
        path: None,
        git_origin: None,
        workspace_root: None,
        embedded_active: true,
    });
    let idx = app.session.reports.len() - 1;
    app.open_report_editor(ReportOrigin::Session(idx), report);
    app.focus = super::Focus::Main;
    app.session.save();
}
