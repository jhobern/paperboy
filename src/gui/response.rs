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

/// The colour a hovered subject is washed in, wherever it is drawn.
///
/// One definition for the body and the headers so the two tabs cannot drift
/// apart, and thin enough (alpha 56) that the text underneath stays legible --
/// the point is to say *which* value, not to hide it.
pub(super) fn wash(theme: &super::theme::GuiTheme) -> egui::Color32 {
    let c = theme.accent;
    egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 56)
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
        lbl_probe,
        lbl_probe_hint,
        lbl_probe_this,
        lbl_copy_this,
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
            s.gui_probe_button,
            s.gui_probe_button_hint,
            s.gui_probe_assert_this,
            s.gui_probe_copy_this,
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
    // What a right-click (or the Assert… button) asked for, applied once the
    // panel's closures have released their borrow of the app: `Some(None)`
    // opens the builder on the whole list, `Some(Some(probe))` on one value.
    let mut open_probe: Option<Option<crate::probe::Probe>> = None;
    // Set by "Copy this value": the status line is written after the panel's
    // closures have released their borrow of the app.
    let mut copied = false;
    // Clone the `Arc`, not the bytes: the context-menu closure below only needs
    // to borrow the body on a right-click, but this line runs on every repaint.
    // `body.to_string()` here was a full-body memcpy per frame on large replies.
    let raw_body = body.clone();
    let compact = app.response_compact;
    // The body field is keyed per request so a selection can't survive into the
    // next one (egui keeps cursor state per widget id); the reader must use the
    // same id.
    let body_id = super::probe::body_field_id(app);
    // Where the body field parks the value the pointer was on when a context
    // menu was opened (see the body arm below).
    let aim_id = body_id.with("aim");
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
            let line = ui.label(
                RichText::new(format!("{status} {status_text}"))
                    .strong()
                    .color(col),
            );
            // The status is the only subject with nothing in the body to aim
            // at, so without this the sole way to assert on it is to find it at
            // the top of the builder's list — which nobody looking straight at
            // "200 OK" thinks to do. Right-clicking what you mean works here
            // exactly as it does on a header row or a body value.
            line.context_menu(|ui| {
                if ui.button(lbl_probe_this).clicked() {
                    open_probe = Some(Some(crate::probe::Probe {
                        subject: crate::probe::Subject::Status,
                        value: Some(serde_json::Value::from(status)),
                    }));
                    ui.close();
                }
            });
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
            // Gated on `sent`, not on a non-empty body: a 204 (or any reply with
            // no body) still has a status and headers to assert on, and the
            // status is the one subject that sets the HTTP line. Right-clicking
            // a value is the quick way in, but it is also invisible: the button
            // is what tells anyone the feature is here at all, and it opens the
            // same builder on the full list.
            if sent && ui.button(lbl_probe).on_hover_text(lbl_probe_hint).clicked() {
                open_probe = Some(None);
            }
            // Compact toggle — only meaningful on the Body section, and only
            // when there is a body to compact. It is display-only: the Copy
            // button above always yields the full body, so a copied value is
            // never truncated.
            if !body.is_empty() && app.response_section == ResponseSection::Body {
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
                    let out = egui::TextEdit::multiline(&mut text.as_str())
                        .id(body_id)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(12)
                        .show(ui);
                    let field = out.response.clone();
                    // What the mouse is over, resolved to a whole JSON value.
                    // A right-click menu that says "Assert this..." has to make
                    // "this" visible *before* it is clicked: the body is a wall
                    // of near-identical tokens, and the difference between
                    // aiming at a key, its value and the object around them is
                    // a few pixels of pointer travel.
                    let aimed = ui
                        .ctx()
                        .pointer_hover_pos()
                        .filter(|_| field.hovered())
                        .and_then(|pos| {
                            super::probe::hovered_in(
                                &out.galley,
                                out.galley_pos,
                                pos,
                                &raw_body,
                                compact,
                            )
                        });
                    // Opening the menu takes the pointer off the text, so the
                    // aim is frozen at the click and held while the menu is up
                    // -- otherwise the highlight vanishes at the exact moment
                    // the user is reading the menu that acts on it.
                    if field.secondary_clicked() {
                        ui.data_mut(|d| d.insert_temp(aim_id, aimed.clone()));
                    }
                    let shown = if field.context_menu_opened() {
                        ui.data_mut(|d| d.get_temp::<Option<crate::probe::Probe>>(aim_id))
                            .flatten()
                    } else {
                        aimed
                    };
                    if let Some(probe) = &shown {
                        super::probe::paint_span(
                            ui.painter(),
                            &out.galley,
                            out.galley_pos,
                            &raw_body,
                            &probe.subject,
                            compact,
                            wash(&theme),
                        );
                    }
                    // The caret is read *after* the field has been drawn, from
                    // the state egui stored under the field's own id — the same
                    // trick the request editor's "Extract to parameter…" uses.
                    field.context_menu(|ui| {
                        if ui.button(lbl_probe_this).clicked() {
                            // The value that was highlighted under the pointer,
                            // so the menu acts on what it showed. The caret is
                            // the fallback for a menu opened from the keyboard,
                            // where there is no pointer to have aimed with.
                            open_probe = Some(shown.clone().or_else(|| {
                                super::probe::pointed_in(ui.ctx(), &raw_body, compact, body_id)
                            }));
                            ui.close();
                        }
                        // Isolating the value under the caret is what the
                        // assert builder already does; copying it is the other
                        // thing anyone who has just isolated a value wants, and
                        // hand-selecting a token out of a minified body is
                        // exactly the fiddly job this avoids. Copies the raw
                        // bytes, so a compacted view still yields the real
                        // value rather than the shortened one on screen.
                        if ui.button(lbl_copy_this).clicked() {
                            if let Some(text) = shown
                                .clone()
                                .or_else(|| {
                                    super::probe::pointed_in(ui.ctx(), &raw_body, compact, body_id)
                                })
                                .and_then(|p| super::probe::raw_value_text(&raw_body, &p))
                            {
                                ui.ctx().copy_text(text);
                                copied = true;
                            }
                            ui.close();
                        }
                    });
                }
            }
            ResponseSection::Headers => {
                if !sent {
                    ui.colored_label(theme.dim, lbl_unsent);
                } else if headers.is_empty() {
                    ui.colored_label(theme.dim, lbl_no_headers);
                } else {
                    // The band a row occupies, taken from the panel rather
                    // than from the two labels: a header row *is* the full
                    // width of the list (that is what the striping says), so
                    // the gap between the columns and the space after a short
                    // value belong to the row too. Aiming at them used to hit
                    // nothing at all -- no highlight, and a right-click that
                    // offered no assert -- which made the target the text
                    // rather than the row it is in.
                    let band = ui.max_rect().x_range();
                    egui::Grid::new("resp_headers")
                        .num_columns(2)
                        .spacing([12.0, 4.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for (i, (k, v)) in headers.iter().enumerate() {
                                let name = ui.label(RichText::new(k).strong().color(theme.accent));
                                let value =
                                    ui.label(RichText::new(v).monospace().color(theme.text));
                                let row_rect = egui::Rect::from_x_y_ranges(
                                    band,
                                    name.rect.union(value.rect).y_range(),
                                )
                                .expand2(egui::vec2(0.0, 2.0));
                                // Interacted with as one thing, after both
                                // labels: the whole row is a single subject, so
                                // it hovers and opens its menu as a single
                                // widget. Keyed by position, since a response
                                // may repeat a header name.
                                let row = ui.interact(
                                    row_rect,
                                    ui.id().with(("resp_header_row", i)),
                                    egui::Sense::click(),
                                );
                                // Lit for the same reason the body highlights
                                // the value under the pointer: "Assert this..."
                                // has to say which "this".
                                if row.hovered() || row.context_menu_opened() {
                                    ui.painter().rect_filled(row_rect, 2.0, wash(&theme));
                                }
                                row.context_menu(|ui| {
                                    if ui.button(lbl_probe_this).clicked() {
                                        open_probe = Some(Some(crate::probe::Probe {
                                            subject: crate::probe::Subject::Header(k.clone()),
                                            value: Some(serde_json::Value::String(v.clone())),
                                        }));
                                        ui.close();
                                    }
                                });
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

    if let Some(pointed) = open_probe {
        super::probe::open(app, ui.ctx(), pointed);
    }
    if copied {
        app.session.status = Some(crate::i18n::Status::Copied);
    }
}

#[cfg(test)]
mod probe_button_tests {
    use super::*;
    use crate::gui::app::GuiApp;
    use crate::gui::theme::GuiTheme;
    use crate::hurl::HurlEntry;
    use eframe::egui;

    /// The right-click route is invisible; the button is what says the feature
    /// exists. It shares a right-to-left row with Copy, where a widget that
    /// doesn't fit is simply never painted rather than clipped — so this checks
    /// it is on screen and inside the panel, not merely that it was added.
    #[test]
    fn the_assert_button_is_painted_inside_the_panel() {
        let mut entry = HurlEntry {
            title: "Login".to_string(),
            ..Default::default()
        };
        entry.last_response = Some(ApiResponse {
            status: 200,
            body: std::sync::Arc::from(r#"{"token":"abc"}"#),
            ..Default::default()
        });
        let mut session = Session::default();
        session.collections.clear();
        session.collections.push(crate::collection::Collection::new(
            "api".into(),
            vec![entry],
        ));
        let mut app = GuiApp::for_test(session);
        let th = GuiTheme::from_spec(&crate::theme::default_preset());
        let ctx = egui::Context::default();
        th.apply(&ctx);
        let width = 900.0;
        let mut placed = Vec::new();
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(width, 700.0),
                )),
                ..Default::default()
            };
            let full = ctx.run_ui(input, |u| super::ui(&mut app, u));
            placed = full.shapes.iter().flat_map(|c| flatten(&c.shape)).collect();
        }
        let label = app.strings.gui_probe_button;
        let hit = placed
            .iter()
            .find(|(t, _)| t.contains(label))
            .unwrap_or_else(|| panic!("the {label} button was never painted: {placed:?}"));
        assert!(hit.1.max.x <= width, "{label} is off the right edge");
    }

    fn flatten(shape: &egui::epaint::Shape) -> Vec<(String, egui::Rect)> {
        match shape {
            egui::epaint::Shape::Text(t) => {
                vec![(t.galley.text().to_string(), t.visual_bounding_rect())]
            }
            egui::epaint::Shape::Vec(v) => v.iter().flat_map(flatten).collect(),
            _ => Vec::new(),
        }
    }
}

/// The two ways into the builder from the response panel — the Assert… button
/// and a right-click on a header row — driven through the real panel with
/// simulated pointer events. Harness in [`crate::gui::probe_test_support`].
#[cfg(test)]
mod probe_route_tests {
    use crate::gui::app::{Dialog, ResponseSection};
    use crate::gui::probe_test_support::*;
    use eframe::egui;

    /// The Assert… button is gated on whether the request was *sent*, not on a
    /// non-empty body: a 204 has no body to right-click, but its status (and
    /// every header) is a perfectly good subject — and the status is the one
    /// verb that sets `expected_status`, which is exactly what a 204 wants.
    #[test]
    fn a_204_offers_no_way_into_the_builder() {
        let mut app = app_with("", vec![("X-Trace".into(), "abc".into())], 204);
        let ctx = themed_ctx();
        panel_frame(&mut app, &ctx, vec![]);
        let painted = panel_frame(&mut app, &ctx, vec![]);
        let seen = texts(&painted);
        let subjects = crate::probe::probes(204, Some(12), &[], "");
        assert!(
            seen.iter()
                .any(|t| t.contains(app.strings.gui_probe_button)),
            "no Assert… button on a reply with no body, though it has {} subjects",
            subjects.len()
        );
    }

    /// The status has no text in the body to aim at, so the status line itself
    /// is the thing to right-click. Without this the only route to `HTTP 201`
    /// is the top row of the builder's list, which is not where anyone reading
    /// "201 Created" looks.
    #[test]
    fn the_status_line_opens_the_builder_on_the_status() {
        let mut app = app_with(r#"{"a":1}"#, vec![], 201);
        let ctx = themed_ctx();
        panel_frame(&mut app, &ctx, vec![]);
        let painted = panel_frame(&mut app, &ctx, vec![]);
        let line = centre_of(&painted, "201");
        let (press, release) = click_events(line, egui::PointerButton::Secondary);
        panel_frame(&mut app, &ctx, press);
        let mut painted = panel_frame(&mut app, &ctx, release);
        for _ in 0..3 {
            painted = panel_frame(&mut app, &ctx, vec![]);
        }
        let item = centre_of(&painted, app.strings.gui_probe_assert_this);
        let (press, release) = click_events(item, egui::PointerButton::Primary);
        panel_frame(&mut app, &ctx, press);
        panel_frame(&mut app, &ctx, release);
        panel_frame(&mut app, &ctx, vec![]);
        match &app.dialog {
            Some(Dialog::ProbeBuilder(b)) => {
                let chosen = b.chosen.as_ref().expect("no subject chosen");
                assert_eq!(crate::probe::subject_label(&chosen.subject), "status");
            }
            _ => panic!("right-clicking the status line did not open the builder"),
        }
    }

    /// A menu that says "Assert this…" has to show what "this" is *before*
    /// it is opened: the body is a wall of near-identical tokens and a few
    /// pixels of pointer travel is the difference between a key, its value and
    /// the object around them. The wash follows the pointer, and it covers the
    /// value it is over rather than some other part of the reply.
    #[test]
    fn hovering_the_body_washes_the_value_under_the_pointer() {
        let body = "{\n  \"status\": \"success\",\n  \"token\": \"abc123\"\n}";
        let mut app = app_with(body, vec![], 200);
        let ctx = themed_ctx();
        panel_frame(&mut app, &ctx, vec![]);
        let painted = panel_frame(&mut app, &ctx, vec![]);
        let at = pos_in_text(&painted, "success");
        panel_output(&mut app, &ctx, vec![egui::Event::PointerMoved(at)]);
        let full = panel_output(&mut app, &ctx, vec![]);
        let washes: Vec<_> = fills(&full)
            .into_iter()
            .filter(|(r, c)| *c == super::wash(&app.theme) && r.contains(at))
            .collect();
        assert!(
            !washes.is_empty(),
            "nothing was highlighted under the pointer"
        );
        // The wash is the *value*, not the whole line: it must not reach back
        // over the key that names it.
        let key = pos_in_text(&painted, "status");
        assert!(
            washes.iter().all(|(r, _)| !r.contains(key)),
            "the highlight swallowed the key as well as the value"
        );
    }

    /// The pointer moving to another token moves the highlight with it --
    /// otherwise the first thing hovered would be highlighted for ever and the
    /// menu would act on something else entirely.
    #[test]
    fn the_wash_follows_the_pointer_to_the_next_value() {
        let body = "{\n  \"status\": \"success\",\n  \"token\": \"abc123\"\n}";
        let mut app = app_with(body, vec![], 200);
        let ctx = themed_ctx();
        panel_frame(&mut app, &ctx, vec![]);
        let painted = panel_frame(&mut app, &ctx, vec![]);
        let first = pos_in_text(&painted, "success");
        let second = pos_in_text(&painted, "abc123");
        panel_output(&mut app, &ctx, vec![egui::Event::PointerMoved(first)]);
        panel_output(&mut app, &ctx, vec![]);
        panel_output(&mut app, &ctx, vec![egui::Event::PointerMoved(second)]);
        let full = panel_output(&mut app, &ctx, vec![]);
        let washes: Vec<_> = fills(&full)
            .into_iter()
            .filter(|(_, c)| *c == super::wash(&app.theme))
            .collect();
        assert!(
            washes.iter().any(|(r, _)| r.contains(second)),
            "the second value was not highlighted"
        );
        assert!(
            washes.iter().all(|(r, _)| !r.contains(first)),
            "the first value stayed highlighted after the pointer left it"
        );
    }

    /// The same promise on the headers tab: hovering either half of a row
    /// lights the whole row, because either half is the same subject and the
    /// menu that opens from it asserts on the pair.
    #[test]
    fn hovering_a_header_lights_the_whole_row() {
        let mut app = app_with(
            r#"{"a":1}"#,
            vec![
                ("X-Request-Id".into(), "r-42".into()),
                ("Content-Type".into(), "application/json".into()),
            ],
            200,
        );
        app.response_section = ResponseSection::Headers;
        let ctx = themed_ctx();
        panel_frame(&mut app, &ctx, vec![]);
        let painted = panel_frame(&mut app, &ctx, vec![]);
        let value = centre_of(&painted, "application/json");
        let name = centre_of(&painted, "Content-Type");
        let other = centre_of(&painted, "r-42");
        panel_output(&mut app, &ctx, vec![egui::Event::PointerMoved(value)]);
        let full = panel_output(&mut app, &ctx, vec![]);
        let washes: Vec<_> = fills(&full)
            .into_iter()
            .filter(|(_, c)| c.a() > 0 && c.a() < 255)
            .collect();
        assert!(
            washes
                .iter()
                .any(|(r, _)| r.contains(value) && r.contains(name)),
            "hovering the value did not light the name beside it"
        );
        assert!(
            washes.iter().all(|(r, _)| !r.contains(other)),
            "the highlight reached a row the pointer was nowhere near"
        );
    }

    /// A row is the full width of the list, so the gap between the columns and
    /// the space after a short value are part of it too: aiming there used to
    /// hit nothing, giving neither a highlight nor a menu.
    #[test]
    fn hovering_past_the_end_of_a_header_still_lights_its_row() {
        let mut app = app_with(
            r#"{"a":1}"#,
            vec![
                ("X-Request-Id".into(), "r-42".into()),
                ("Content-Type".into(), "application/json".into()),
            ],
            200,
        );
        app.response_section = ResponseSection::Headers;
        let ctx = themed_ctx();
        panel_frame(&mut app, &ctx, vec![]);
        let painted = panel_frame(&mut app, &ctx, vec![]);
        let short = rect_of(&painted, "r-42");
        // Well past the value's last character, still on its line.
        let empty = egui::pos2(short.max.x + 120.0, short.center().y);
        panel_output(&mut app, &ctx, vec![egui::Event::PointerMoved(empty)]);
        let full = panel_output(&mut app, &ctx, vec![]);
        let washes: Vec<_> = fills(&full)
            .into_iter()
            .filter(|(_, c)| c.a() > 0 && c.a() < 255)
            .collect();
        assert!(
            washes
                .iter()
                .any(|(r, _)| r.contains(empty) && r.contains(short.center())),
            "the empty space after a short value did not light its row"
        );
    }

    /// The headers section has no Assert… button, but its rows are
    /// right-clickable — this pins the working route so the 204 gap above is
    /// clearly about the button and not about the feature.
    #[test]
    fn a_header_row_still_opens_the_builder_on_that_header() {
        let mut app = app_with(
            r#"{"a":1}"#,
            vec![
                ("X-Request-Id".into(), "r-42".into()),
                ("Content-Type".into(), "application/json".into()),
            ],
            200,
        );
        app.response_section = ResponseSection::Headers;
        let ctx = themed_ctx();
        panel_frame(&mut app, &ctx, vec![]);
        let painted = panel_frame(&mut app, &ctx, vec![]);
        let row = centre_of(&painted, "application/json");
        let (press, release) = click_events(row, egui::PointerButton::Secondary);
        panel_frame(&mut app, &ctx, press);
        let mut painted = panel_frame(&mut app, &ctx, release);
        for _ in 0..3 {
            painted = panel_frame(&mut app, &ctx, vec![]);
        }
        let item = centre_of(&painted, app.strings.gui_probe_assert_this);
        let (press, release) = click_events(item, egui::PointerButton::Primary);
        panel_frame(&mut app, &ctx, press);
        panel_frame(&mut app, &ctx, release);
        panel_frame(&mut app, &ctx, vec![]);
        match &app.dialog {
            Some(Dialog::ProbeBuilder(b)) => {
                let chosen = b.chosen.as_ref().expect("no subject chosen");
                assert_eq!(
                    crate::probe::subject_label(&chosen.subject),
                    "header Content-Type",
                    "the second row's menu targeted another row"
                );
            }
            _ => panic!("the header right-click did not open the builder"),
        }
    }
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

        // The body field is now keyed per request (so a selection can't leak
        // into the next one); read the state back under that same id.
        let id = crate::gui::probe::body_field_id(&app);
        let state = egui::text_edit::TextEditState::load(&ctx, id)
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
