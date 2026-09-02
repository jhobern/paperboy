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
        lbl_unsent,
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
            s.no_response_yet,
            s.gui_sec_body,
            s.gui_sec_headers,
            s.gui_sec_asserts,
        )
    };

    // See `selected_response`: the panel follows the selection, not the wire.
    let (shown, loading) = selected_response(&app.session, app.active_ci());
    // Whether this request has a response *at all*, as opposed to a response
    // that happens to be empty. Without the distinction each section falls back
    // to its own "there is nothing here" note — "(empty body)", "(no headers)",
    // "(no assertions)" — and every one of those is a claim about a reply that
    // was never received. A request that has not been sent would report that the
    // server returned no body, which is exactly backwards: the reader is being
    // told the result of a request rather than that there isn't one yet.
    let sent = shown.is_some();
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
                if !sent {
                    ui.colored_label(theme.dim, lbl_unsent);
                } else if body.is_empty() {
                    ui.colored_label(theme.dim, lbl_empty_body);
                } else {
                    let text = if app.response_compact {
                        crate::shared_utils::compact_long_strings(&body[..])
                    } else {
                        body.to_string()
                    };
                    // `&mut &str`, not `&mut String`: `TextBuffer for &str`
                    // reports `is_mutable() == false`, so the field stays
                    // read-only while remaining interactive - which is what
                    // gives it selection, word-on-double-click and Ctrl+C.
                    // `interactive(false)` takes those away too.
                    ui.add(
                        egui::TextEdit::multiline(&mut text.as_str())
                            .id(egui::Id::new("resp_body"))
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .desired_rows(12),
                    );
                }
            }
            ResponseSection::Headers => {
                if !sent {
                    ui.colored_label(theme.dim, lbl_unsent);
                } else if headers.is_empty() {
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
                if !sent {
                    ui.colored_label(theme.dim, lbl_unsent);
                } else if asserts.is_empty() {
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

    /// Read-only must not mean unselectable: the one panel whose whole purpose
    /// is text to copy out of was the one panel that could not be dragged
    /// across.
    #[test]
    fn double_clicking_the_response_body_selects_the_word_under_the_pointer() {
        let mut app = GuiApp::for_test(session_with_two_answered_requests());
        app.response_section = ResponseSection::Body;

        let ctx = egui::Context::default();
        let mut time = 0.0;
        let mut frame = |app: &mut GuiApp, events: Vec<egui::Event>| {
            let mut input = egui::RawInput::default();
            input.time = Some(time);
            time += 0.05;
            input.screen_rect = Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(600.0, 400.0),
            ));
            input.events = events;
            ctx.run_ui(input, |panel| super::ui(app, panel)).shapes
        };

        // The first frame only locates the body text; the next two are the
        // double-click's two clicks, which egui pairs by time.
        let shapes = frame(&mut app, Vec::new());
        let pos = body_text_pos(&shapes).expect("the body must be drawn");
        frame(&mut app, click_at(pos));
        frame(&mut app, click_at(pos));

        let state = egui::text_edit::TextEditState::load(&ctx, egui::Id::new("resp_body"))
            .expect("the body must be a real, interactive TextEdit");
        let range = state
            .cursor
            .char_range()
            .expect("a double-click must leave a selection");
        let (a, b) = (
            range.primary.index.0.min(range.secondary.index.0),
            range.primary.index.0.max(range.secondary.index.0),
        );
        assert_eq!(
            &"first body"[a..b],
            "first",
            "a word, not a character and not the whole line"
        );
    }

    fn body_text_pos(shapes: &[egui::epaint::ClippedShape]) -> Option<egui::Pos2> {
        fn walk(shape: &egui::epaint::Shape, out: &mut Option<egui::Pos2>) {
            match shape {
                egui::epaint::Shape::Text(t) if out.is_none() => {
                    if t.galley.text().contains("first body") {
                        // Two characters in: inside "first", clear of the edge
                        // where rounding could land the cursor on the word
                        // before it.
                        let h = t.galley.size().y;
                        *out = Some(t.pos + egui::vec2(2.0 * h * 0.5, h * 0.5));
                    }
                }
                egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = None;
        for c in shapes {
            walk(&c.shape, &mut out);
        }
        out
    }

    fn click_at(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
        ]
    }
}

#[cfg(test)]
mod unsent_tests {
    use super::*;
    use crate::collection::Collection;
    use crate::gui::app::GuiApp;
    use crate::hurl::HurlEntry;

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

    /// Draw the response panel and read back every string it painted. A real
    /// screen rect matters: the sections live in a `ScrollArea`, which culls
    /// anything it believes is offscreen.
    fn draw(app: &mut GuiApp, section: ResponseSection) -> Vec<String> {
        app.response_section = section;
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(700.0, 600.0),
        ));
        let out = ctx.run_ui(input, |panel| super::ui(app, panel));
        painted_text(&out.shapes)
    }

    fn app_with(response: Option<ApiResponse>) -> GuiApp {
        let mut e = HurlEntry {
            title: "First".to_string(),
            ..Default::default()
        };
        e.last_response = response;
        let mut session = Session::default();
        session.collections.clear();
        session
            .collections
            .push(Collection::new("api".to_string(), vec![e]));
        GuiApp::for_test(session)
    }

    /// A request that has never been sent has no response to describe, so the
    /// panel must say that rather than describing one. Every section had its
    /// own "nothing here" note and every one of them read as a *result*:
    /// "(empty body)" claims the server answered and sent nothing back.
    #[test]
    fn an_unsent_request_says_so_in_every_section() {
        let mut app = app_with(None);
        let unsent = app.strings.no_response_yet.to_string();
        let empty_body = app.strings.gui_empty_body.to_string();
        let no_headers = app.strings.gui_no_headers.to_string();
        let no_asserts = app.strings.gui_no_assertions.to_string();

        for (name, section) in [
            ("body", ResponseSection::Body),
            ("headers", ResponseSection::Headers),
            ("asserts", ResponseSection::Asserts),
        ] {
            let painted = draw(&mut app, section);
            assert!(
                painted.contains(&unsent),
                "{name}: expected the not-sent-yet note, got {painted:?}"
            );
            for lie in [&empty_body, &no_headers, &no_asserts] {
                assert!(
                    !painted.contains(lie),
                    "{name}: {lie:?} describes a reply that never arrived: {painted:?}"
                );
            }
        }
    }

    /// The other half: a request that *was* sent and genuinely came back with
    /// nothing must still say so. The fix must not swallow the real empty case.
    #[test]
    fn a_sent_request_with_an_empty_body_still_says_the_body_was_empty() {
        let mut app = app_with(Some(ApiResponse {
            status: 204,
            body: std::sync::Arc::from(""),
            ..Default::default()
        }));
        let painted = draw(&mut app, ResponseSection::Body);
        assert!(
            painted.contains(&app.strings.gui_empty_body.to_string()),
            "204 really did return no body: {painted:?}"
        );
        assert!(
            !painted.contains(&app.strings.no_response_yet.to_string()),
            "but it was sent, so the not-sent note would now be the lie: {painted:?}"
        );
    }
}
