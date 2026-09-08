//! Headless egui harness shared by the probe builder's inline tests.
//!
//! The response-viewer probe route is a mouse feature: a right-click resolves a
//! caret to a subject, a drag selects the literal for `body contains …`, and
//! the three-step dialog is driven by clicks. None of that can be exercised by
//! calling functions — it only exists once egui has laid the panel out and
//! stored the cursor state a later frame reads back. So the tests paint the
//! real panel (and the real modal layer) with simulated pointer events and read
//! back what was actually drawn.
//!
//! Kept in one place, rather than duplicated into each file's `mod tests`,
//! because every one of `probe`, `response` and `menu` needs the same rig.

#![cfg(test)]

use eframe::egui;

use super::app::GuiApp;
use super::theme::GuiTheme;
use crate::hurl::HurlEntry;
use crate::session::Session;

/// Every run of text one frame painted, with where it landed.
pub(crate) type Painted = Vec<(egui::Pos2, std::sync::Arc<egui::Galley>)>;

fn galleys(shape: &egui::epaint::Shape, out: &mut Painted) {
    match shape {
        egui::epaint::Shape::Text(t) => out.push((t.pos, t.galley.clone())),
        egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| galleys(s, out)),
        _ => {}
    }
}

pub(crate) fn collect(full: &egui::FullOutput) -> Painted {
    let mut out = Vec::new();
    for c in &full.shapes {
        galleys(&c.shape, &mut out);
    }
    out
}

pub(crate) fn texts(painted: &Painted) -> Vec<String> {
    painted.iter().map(|(_, g)| g.text().to_string()).collect()
}

pub(crate) fn input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(900.0, 700.0),
        )),
        events,
        ..Default::default()
    }
}

/// One frame of the response panel.
pub(crate) fn panel_frame(
    app: &mut GuiApp,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
) -> Painted {
    let full = ctx.run_ui(input(events), |ui| super::response::ui(app, ui));
    collect(&full)
}

/// One frame of the modal dialog layer.
pub(crate) fn dialog_frame(
    app: &mut GuiApp,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
) -> Painted {
    let full = ctx.run_ui(input(events), |ui| super::menu::show_dialog(app, ui.ctx()));
    collect(&full)
}

pub(crate) fn click_events(
    pos: egui::Pos2,
    button: egui::PointerButton,
) -> (Vec<egui::Event>, Vec<egui::Event>) {
    let ev = |pressed| egui::Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: Default::default(),
    };
    (
        vec![egui::Event::PointerMoved(pos), ev(true)],
        vec![ev(false)],
    )
}

/// Where a piece of painted text sits, by exact match.
pub(crate) fn centre_of(painted: &Painted, needle: &str) -> egui::Pos2 {
    let (pos, g) = painted
        .iter()
        .find(|(_, g)| g.text().contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} was never painted: {:?}", texts(painted)));
    *pos + egui::vec2(g.size().x / 2.0, g.size().y / 2.0)
}

pub(crate) fn app_with(body: &str, headers: Vec<(String, String)>, status: u16) -> GuiApp {
    let mut entry = HurlEntry {
        title: "Login".to_string(),
        ..Default::default()
    };
    entry.last_response = Some(crate::http::ApiResponse {
        status,
        body: std::sync::Arc::from(body),
        headers,
        duration_ms: Some(12),
        ..Default::default()
    });
    let mut session = Session::default();
    session.collections.clear();
    session.collections.push(crate::collection::Collection::new(
        "api".into(),
        vec![entry],
    ));
    GuiApp::for_test(session)
}

pub(crate) fn themed_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    GuiTheme::from_spec(&crate::theme::default_preset()).apply(&ctx);
    ctx
}
