//! Native GUI front-end (eframe / egui).
//!
//! A Postman-style graphical client built on the exact same front-end-agnostic
//! core as the terminal UI — [`crate::session::Session`] plus the shared
//! `request` / `collection` / `environment` / `theme` modules — so the two
//! never duplicate logic. egui is immediate-mode like ratatui, so the panel
//! model and the RGB [`crate::theme`] map across directly; mouse, scrolling,
//! text selection, clipboard and click-and-drag panel resizing are all handled
//! natively by egui rather than the hand-written plumbing the terminal UI needs.

mod app;
mod editor;
mod environments;
mod icons;
mod menu;
mod remote;
mod report_editor;
mod report_run;
mod report_wizard;
mod reports;
mod requests;
mod response;
mod theme;
mod widgets;

pub use app::GuiApp;

use eframe::egui;

/// Launch the GUI. Blocks until the window is closed.
pub fn run() -> Result<(), String> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([760.0, 500.0])
            .with_title("PaperBoy")
            .with_app_id("paperboy"),
        ..Default::default()
    };
    eframe::run_native(
        "PaperBoy",
        native_options,
        Box::new(|cc| Ok(Box::new(GuiApp::new(cc)))),
    )
    .map_err(|e| e.to_string())
}

/// The five focusable regions, mirroring the terminal UI's `Pane` and its Tab
/// cycle order exactly: Tabs → List → Main → GlobalEnv → Response.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Focus {
    Tabs,
    List,
    Main,
    GlobalEnv,
    Response,
}

impl Focus {
    /// The Tab cycle order (identical to the terminal UI's `TuiApp::panes`).
    pub const ORDER: [Focus; 5] = [
        Focus::Tabs,
        Focus::List,
        Focus::Main,
        Focus::GlobalEnv,
        Focus::Response,
    ];

    /// The next/previous focus in the cycle (Tab / Shift+Tab).
    pub fn cycle(self, forward: bool) -> Focus {
        let n = Self::ORDER.len();
        let cur = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0);
        let next = if forward {
            (cur + 1) % n
        } else {
            (cur + n - 1) % n
        };
        Self::ORDER[next]
    }
}
