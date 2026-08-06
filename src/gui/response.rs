//! Centre-bottom panel: the response viewer — status line, timing and section
//! tabs (Body, Headers, Asserts) over the shared response buffer.

use eframe::egui::{self, RichText};

use crate::http::ApiResponse;
use crate::hurl::RunStatus;
use crate::session::Session;

use super::app::{GuiApp, ResponseSection};
use super::widgets::{self, status_color};

/// The response the panel should show for collection `ci`, plus whether that
/// request is still in flight.
///
/// It is the SELECTED request's own last response, not whatever came back most
/// recently — the same rule the terminal UI follows. The shared
/// `Session::response` buffer is the live one-slot state a send writes into as
/// it streams; reading it here meant clicking through a collection left the
/// previous request's body sitting under a different request's name. Each
/// entry's `last_response` is stamped when its run completes (single runs and
/// Run All alike), so selecting any entry shows what that entry actually got
/// back.
///
/// "Sending" is per-entry for the same reason: an entry is in flight while its
/// `last_run` is `Running`, so a request still on the wire shows the spinner
/// while selecting a different entry shows that entry's finished response —
/// even though some other request is mid-send.
fn selected_response(session: &Session, ci: usize) -> (Option<&ApiResponse>, bool) {
    let entry = session
        .collections
        .get(ci)
        .and_then(|col| col.entries.get(col.selected_entry));
    let loading = entry.is_some_and(|e| e.last_run == RunStatus::Running);
    (entry.and_then(|e| e.last_response.as_ref()), loading)
}

pub fn ui(app: &mut GuiApp, ui: &mut egui::Ui) {
    let theme = app.theme;
    let (
        lbl_sending,
        lbl_error,
        lbl_no_response,
        lbl_copy,
        lbl_copy_body,
        lbl_compact,
        lbl_compact_hint,
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
            s.gui_compact,
            s.gui_compact_hint,
            s.gui_empty_body,
            s.gui_no_headers,
            s.gui_no_assertions,
            s.gui_sec_body,
            s.gui_sec_headers,
            s.gui_sec_asserts,
        )
    };

    // See `selected_response`: the panel follows the selection, not the wire.
    let (shown, loading) = selected_response(&app.session, app.active_ci());
    let (status, status_text, body, error, headers, asserts, duration) = match shown {
        Some(r) => (
            r.status,
            r.status_text.clone(),
            r.body.clone(),
            r.error.clone(),
            r.headers.clone(),
            r.assert_results.clone(),
            r.duration_ms,
        ),
        None => (
            0,
            String::new(),
            std::sync::Arc::from(""),
            String::new(),
            Vec::new(),
            Vec::new(),
            None,
        ),
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
                // Compact toggle — only meaningful on the Body section. It is
                // display-only: the Copy button above always yields the full
                // body, so a copied value is never truncated.
                if app.response_section == ResponseSection::Body {
                    let mut compact = app.response_compact;
                    if ui
                        .selectable_label(compact, lbl_compact)
                        .on_hover_text(lbl_compact_hint)
                        .clicked()
                    {
                        compact = !compact;
                    }
                    app.response_compact = compact;
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
                    let mut text = if app.response_compact {
                        crate::shared_utils::compact_long_strings(&body[..])
                    } else {
                        body.to_string()
                    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::Collection;
    use crate::hurl::HurlEntry;

    /// Two entries, each with its own recorded response, in one collection.
    fn session_with_two_answered_requests() -> Session {
        let mut a = HurlEntry {
            title: "First".to_string(),
            ..Default::default()
        };
        a.last_response = Some(ApiResponse {
            status: 201,
            body: std::sync::Arc::from("first body"),
            ..Default::default()
        });
        let mut b = HurlEntry {
            title: "Second".to_string(),
            ..Default::default()
        };
        b.last_response = Some(ApiResponse {
            status: 404,
            body: std::sync::Arc::from("second body"),
            ..Default::default()
        });

        let mut session = Session::default();
        // A fresh Session starts with an empty scratch collection; drop it so
        // index 0 is the fixture and the assertions read plainly.
        session.collections.clear();
        session
            .collections
            .push(Collection::new("api".to_string(), vec![a, b]));
        session
    }

    #[test]
    fn the_response_panel_follows_the_selected_request_not_the_last_one_run() {
        let mut session = session_with_two_answered_requests();

        session.collections[0].selected_entry = 1;
        let (shown, _) = selected_response(&session, 0);
        assert_eq!(
            shown.map(|r| r.status),
            Some(404),
            "the second request is selected, so its own answer is shown"
        );

        // Selecting back to the first must bring back *its* answer, even though
        // nothing has been re-run — the whole point of remembering per-entry.
        session.collections[0].selected_entry = 0;
        let (shown, _) = selected_response(&session, 0);
        assert_eq!(
            shown.map(|r| r.status),
            Some(201),
            "selecting the first request shows what the first request got back"
        );
        assert_eq!(
            shown.map(|r| r.body.to_string()).as_deref(),
            Some("first body")
        );
    }

    #[test]
    fn only_the_request_actually_in_flight_shows_as_sending() {
        let mut session = session_with_two_answered_requests();
        session.collections[0].entries[1].last_run = RunStatus::Running;

        session.collections[0].selected_entry = 1;
        let (_, loading) = selected_response(&session, 0);
        assert!(loading, "the selected request is the one on the wire");

        // Selecting a different, idle request while that one is still going
        // must show its finished response rather than a spinner.
        session.collections[0].selected_entry = 0;
        let (shown, loading) = selected_response(&session, 0);
        assert!(
            !loading,
            "a settled request doesn't borrow another request's spinner"
        );
        assert_eq!(shown.map(|r| r.status), Some(201));
    }

    #[test]
    fn a_request_that_has_never_run_shows_nothing_rather_than_a_neighbours_answer() {
        let mut session = session_with_two_answered_requests();
        session.collections[0].entries.push(HurlEntry {
            title: "Third".to_string(),
            ..Default::default()
        });
        session.collections[0].selected_entry = 2;

        let (shown, loading) = selected_response(&session, 0);
        assert!(
            shown.is_none(),
            "an unrun request has no response of its own to show"
        );
        assert!(!loading);
    }

    /// An out-of-range collection index (no collections, or a stale tab) must
    /// not panic the draw pass.
    #[test]
    fn an_index_with_no_collection_behind_it_is_simply_empty() {
        let session = Session::default();
        let (shown, loading) = selected_response(&session, 0);
        assert!(shown.is_none());
        assert!(!loading);
    }
}
