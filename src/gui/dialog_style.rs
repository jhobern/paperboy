//! A consistency guard over the app's dialogs.
//!
//! Every dialog is painted headlessly and every piece of text it draws is
//! collected with the size it was drawn at. The app has exactly two text
//! sizes — body for content and heading for a window's title — and a dialog
//! that reaches for a third (a `.small()` annotation, a title left at body
//! size) reads as belonging to a different program. That is easy to do by
//! accident and impossible to see in a diff, so it is asserted here instead.

#[cfg(test)]
mod tests {
    use crate::gui::app::{Dialog, GuiApp, PromptKind, RenameTarget};
    use crate::session::Session;
    use eframe::egui;

    /// The only two sizes any dialog may paint text at.
    const BODY: f32 = 13.0;
    const HEADING: f32 = 18.0;

    /// Every run of text one frame painted, with the size it was set in.
    fn painted(app: &mut GuiApp) -> Vec<(String, f32)> {
        fn walk(s: &egui::epaint::Shape, out: &mut Vec<(String, f32)>) {
            match s {
                egui::epaint::Shape::Text(t) => {
                    let text = t.galley.text().to_string();
                    if text.trim().is_empty() {
                        return;
                    }
                    for sec in &t.galley.job.sections {
                        out.push((text.clone(), sec.format.font_id.size));
                    }
                }
                egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }

        let ctx = egui::Context::default();
        app.theme.apply(&ctx);
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        };
        // Three frames: a modal's layer is laid out on the frame after the one
        // that asks for it, and a window is only sized once it has been seen.
        let mut last = Vec::new();
        for _ in 0..3 {
            let out = ctx.run_ui(input(), |ui| {
                let ctx = ui.ctx().clone();
                crate::gui::menu::show_dialog(app, &ctx);
                crate::gui::postman::show(app, &ctx);
                crate::gui::remote::show(app, &ctx);
                if app.report_editor.is_some() {
                    crate::gui::report_editor::ui(app, ui);
                }
            });
            last.clear();
            out.shapes.iter().for_each(|s| walk(&s.shape, &mut last));
        }
        last
    }

    /// Regression guard: a dialog that paints text at any other size — the
    /// `.small()` annotations the parameter cards used to carry, or a modal
    /// that drew its own title at body size — is a dialog that no longer looks
    /// like the rest of the app.
    #[test]
    fn every_dialog_paints_at_the_apps_two_text_sizes() {
        let cases: Vec<(&str, Box<dyn Fn(&mut GuiApp)>)> = vec![
            (
                "Rename",
                Box::new(|a: &mut GuiApp| {
                    a.dialog = Some(Dialog::Rename {
                        target: RenameTarget::Tab { ci: 0 },
                        text: "My collection".into(),
                    })
                }),
            ),
            (
                "Prompt/BaseUrl",
                Box::new(|a: &mut GuiApp| {
                    a.dialog = Some(Dialog::Prompt {
                        kind: PromptKind::BaseUrl,
                        text: "https://example.test".into(),
                    })
                }),
            ),
            (
                "Prompt/NewEnvName",
                Box::new(|a: &mut GuiApp| {
                    a.dialog = Some(Dialog::Prompt {
                        kind: PromptKind::NewEnvName,
                        text: String::new(),
                    })
                }),
            ),
            (
                "UnsavedQuit",
                Box::new(|a: &mut GuiApp| {
                    a.dialog = Some(Dialog::UnsavedQuit {
                        count: 3,
                        tabs: "Alpha, Beta".into(),
                    })
                }),
            ),
            (
                "UnsavedCloseTab",
                Box::new(|a: &mut GuiApp| {
                    a.dialog = Some(Dialog::UnsavedCloseTab {
                        ci: 0,
                        name: "Alpha".into(),
                        count: 2,
                    })
                }),
            ),
            (
                "ExportResults",
                Box::new(|a: &mut GuiApp| {
                    a.dialog = Some(Dialog::ExportResults {
                        path: "/home/me/report.csv".into(),
                    })
                }),
            ),
            (
                "CloseGitWorkspace",
                Box::new(|a: &mut GuiApp| {
                    a.dialog = Some(Dialog::CloseGitWorkspace {
                        ci: 0,
                        root: "/tmp/pb/ws".into(),
                    })
                }),
            ),
            (
                "RevertToSaved",
                Box::new(|a: &mut GuiApp| {
                    a.dialog = Some(Dialog::RevertToSaved {
                        ci: 0,
                        path: "/tmp/pb/alpha.hurl".into(),
                        entry: None,
                        name: "Alpha".into(),
                    })
                }),
            ),
            (
                "ConfirmDeleteRequest",
                Box::new(|a: &mut GuiApp| {
                    a.dialog = Some(Dialog::ConfirmDeleteRequest {
                        ci: 0,
                        idx: 0,
                        name: "Login".into(),
                    })
                }),
            ),
            (
                "ConfirmRunAll",
                Box::new(|a: &mut GuiApp| {
                    a.dialog = Some(Dialog::ConfirmRunAll {
                        ci: 0,
                        total: 8,
                        non_get: 3,
                    })
                }),
            ),
            (
                "Shortcuts",
                Box::new(|a: &mut GuiApp| a.dialog = Some(Dialog::Shortcuts)),
            ),
            (
                "Theme",
                Box::new(|a: &mut GuiApp| {
                    let spec = a.session.active_theme_spec();
                    a.dialog = Some(Dialog::Theme(Box::new(crate::gui::menu::ThemeEditState {
                        original_name: spec.name.clone(),
                        spec,
                    })));
                }),
            ),
            (
                "Report/RunSettings",
                Box::new(|a: &mut GuiApp| {
                    const TRAIL: &str = "\
# name: Face
# collection: c.hurl
PARAM TEXT TICKET LABEL \"Ticket number\"
PARAM CHOICE(\"au\", \"eu\") REGION = \"au\"
PARAM FILE SAMPLES
REPORT REQUEST x
";
                    a.open_report_editor(
                        crate::gui::report_editor::ReportOrigin::Workspace,
                        crate::report::Report::from_text("Face", TRAIL),
                    );
                    let ed = a.report_editor.as_mut().unwrap();
                    crate::gui::report_editor::arm_params_for_audit(ed);
                }),
            ),
            (
                "Report/NodeWizard",
                Box::new(|a: &mut GuiApp| {
                    const TRAIL: &str = "\
# name: Face
# collection: c.hurl
REPORT REQUEST x
";
                    a.open_report_editor(
                        crate::gui::report_editor::ReportOrigin::Workspace,
                        crate::report::Report::from_text("Face", TRAIL),
                    );
                    let mut ed = a.report_editor.take().unwrap();
                    crate::gui::report_wizard::open(&mut ed, a, &[0]);
                    a.report_editor = Some(ed);
                }),
            ),
            (
                "Git/LoadCollection",
                Box::new(|a: &mut GuiApp| a.remote.open_load()),
            ),
            (
                "Git/SaveReport",
                Box::new(|a: &mut GuiApp| a.remote.open_save_report()),
            ),
            (
                "Postman/Options",
                Box::new(|a: &mut GuiApp| {
                    a.postman.open();
                    a.postman
                        .seed_step_for_audit(crate::postman_flow::Step::Options);
                }),
            ),
            (
                "Postman/Confirm",
                Box::new(|a: &mut GuiApp| {
                    a.postman.open();
                    a.postman
                        .seed_step_for_audit(crate::postman_flow::Step::Confirm);
                }),
            ),
            (
                "Postman/Downloading",
                Box::new(|a: &mut GuiApp| {
                    a.postman.open();
                    a.postman
                        .seed_step_for_audit(crate::postman_flow::Step::Downloading);
                }),
            ),
            (
                "Postman/Done",
                Box::new(|a: &mut GuiApp| {
                    a.postman.open();
                    a.postman
                        .seed_step_for_audit(crate::postman_flow::Step::Done);
                }),
            ),
            (
                "Postman/Connect",
                Box::new(|a: &mut GuiApp| {
                    a.postman.open();
                }),
            ),
        ];
        for (name, setup) in cases {
            let mut app = GuiApp::for_test(Session::default());
            setup(&mut app);
            for (text, size) in painted(&mut app) {
                assert!(
                    size == BODY || size == HEADING,
                    "{name} paints {text:?} at {size}, which is neither the \
                     body size ({BODY}) nor a title ({HEADING})"
                );
            }
        }
    }
}
