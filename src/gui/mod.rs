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
mod filepick;
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

/// Inner size the window opens at on a profile that has never been resized.
pub const DEFAULT_WINDOW: (f32, f32) = (1280.0, 820.0);

/// Smallest inner size the window can be shrunk to. Also the floor a restored
/// size must clear before it is trusted.
pub const MIN_WINDOW: (f32, f32) = (760.0, 500.0);

/// Launch the GUI. Blocks until the window is closed.
pub fn run() -> Result<(), String> {
    // Best-effort: register a freedesktop launcher so Linux taskbars/docks show
    // the PaperBoy logo instead of a generic icon (see the function's docs).
    install_desktop_integration();
    // The window has to be sized before `GuiApp::new` runs (that's when the
    // session is loaded), so peek at the saved layout here. Reading the state
    // file twice is cheap next to opening a window, and keeps the size in the
    // one `state.json` both front-ends share rather than a second eframe store.
    let saved = crate::persistence::load_state()
        .map(|s| s.gui)
        .unwrap_or_default();
    let (w, h) = saved
        .window
        // A window saved as degenerate (a crash mid-resize, or a monitor that
        // has since gone away) would reopen unusable, so anything smaller than
        // the minimum falls back to the default size.
        .filter(|(w, h)| *w >= MIN_WINDOW.0 && *h >= MIN_WINDOW.1)
        .unwrap_or(DEFAULT_WINDOW);
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([w, h])
        .with_min_inner_size([MIN_WINDOW.0, MIN_WINDOW.1])
        .with_title("PaperBoy")
        .with_app_id("paperboy");
    // Use the PaperBoy logo as the window / taskbar icon instead of the
    // platform's default. Decoding can't fail for our bundled asset, but if it
    // ever did we simply fall back to the default icon rather than refusing to
    // launch.
    if let Some(icon) = app::load_app_icon() {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "PaperBoy",
        native_options,
        Box::new(|cc| Ok(Box::new(GuiApp::new(cc)))),
    )
    .map_err(|e| e.to_string())
}

/// Best-effort install of a freedesktop `.desktop` entry + icon so Linux
/// desktop environments show the PaperBoy logo in the taskbar/dock rather than
/// a generic fallback.
///
/// A window's `_NET_WM_ICON` (set via [`egui::ViewportBuilder::with_icon`]) is
/// honoured by the title bar, but most GNOME/KDE/Wayland shells resolve the
/// *taskbar* icon by matching the running window's app_id
/// (`with_app_id("paperboy")`) to an installed `.desktop` file and using its
/// `Icon=`. With no such file there is nothing to match, so the shell shows its
/// default. We therefore write, if absent, a per-user entry under
/// `$XDG_DATA_HOME/applications` whose `StartupWMClass` equals the app_id and
/// whose `Icon` is an absolute path to a copy of the bundled logo.
///
/// Everything here is best-effort: writing to the user's home must never block
/// launching, so all errors are swallowed, and pre-existing files are left
/// untouched (a user who customised them keeps their version). The desktop
/// environment may only pick the entry up on its next scan / login.
#[cfg(target_os = "linux")]
fn install_desktop_integration() {
    use std::path::PathBuf;

    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
    let Some(data_home) = data_home else {
        return;
    };

    // 1. Drop a copy of the logo where the entry can point at it (if missing).
    let icon_dir = data_home.join("paperboy");
    let icon_path = icon_dir.join("paperboy_logo.png");
    if !icon_path.exists() {
        let _ = std::fs::create_dir_all(&icon_dir);
        let _ = std::fs::write(&icon_path, app::LOGO_PNG);
    }

    // 2. Write the launcher entry (if missing). `Exec` re-launches the GUI
    //    (`-g`), `StartupWMClass` must equal the window app_id, and `Icon` is an
    //    absolute path so no icon-theme install is required.
    let apps_dir = data_home.join("applications");
    let desktop_path = apps_dir.join("paperboy.desktop");
    if !desktop_path.exists() {
        let exec = std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(str::to_string))
            .unwrap_or_else(|| "paperboy".to_string());
        let icon = icon_path.to_string_lossy();
        let entry = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=PaperBoy\n\
             Comment=Rust-native API client\n\
             Exec={exec} -g\n\
             Icon={icon}\n\
             Terminal=false\n\
             StartupWMClass=paperboy\n\
             Categories=Development;Utility;\n"
        );
        let _ = std::fs::create_dir_all(&apps_dir);
        let _ = std::fs::write(&desktop_path, entry);
    }
}

/// No desktop-integration step is needed off Linux (macOS/Windows resolve the
/// taskbar icon from the window/bundle directly).
#[cfg(not(target_os = "linux"))]
fn install_desktop_integration() {}

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
