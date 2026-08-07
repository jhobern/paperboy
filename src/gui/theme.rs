//! Maps the shared [`crate::theme::ThemeSpec`] (RGB triples, the same source of
//! truth the terminal UI themes from) onto egui's visual style, so every
//! built-in preset and user-defined custom theme applies to the GUI unchanged.

use eframe::egui::{self, Color32, Stroke};

use crate::theme::ThemeSpec;

/// The active theme's colours as egui `Color32`s, for direct use by panels
/// (method badges, status colours, substitution highlighting, …).
#[derive(Clone, Copy)]
pub struct GuiTheme {
    pub bg: Color32,
    pub panel: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub ok: Color32,
    pub err: Color32,
    pub subst: Color32,
    pub pending: Color32,
    pub select_bg: Color32,
    pub select_fg: Color32,
}

fn c([r, g, b]: [u8; 3]) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// Convert a `ratatui` colour to an egui one, so the shared PaperTrail
/// highlighter (`tui::report_highlight`, which speaks in ratatui spans) can
/// paint the GUI's Source view.
///
/// Every colour a theme produces is an RGB triple — `ThemeSpec::to_theme` only
/// ever builds `Color::Rgb` — so that arm is the whole story in practice. The
/// terminal's indexed/named colours have no fixed RGB value (they're whatever
/// the terminal's own palette says), so rather than invent one they fall back
/// to the caller's default foreground.
pub fn from_ratatui(color: ratatui::style::Color, fallback: Color32) -> Color32 {
    match color {
        ratatui::style::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        _ => fallback,
    }
}

/// Blend `a` toward `b` by `t` (0..1) — used to derive hover/active shades from
/// the panel colour so widgets sit naturally on any theme.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

impl GuiTheme {
    pub fn from_spec(s: &ThemeSpec) -> Self {
        Self {
            bg: c(s.bg),
            panel: c(s.panel),
            text: c(s.text),
            dim: c(s.dim),
            accent: c(s.accent),
            ok: c(s.ok),
            err: c(s.err),
            subst: c(s.subst),
            pending: c(s.pending),
            select_bg: c(s.select_bg),
            select_fg: c(s.select_fg),
        }
    }

    /// A slightly-raised surface colour (panel headers, table stripes, inactive
    /// tabs), derived from the panel colour so it tracks the theme.
    pub fn raised(&self) -> Color32 {
        mix(self.panel, self.text, 0.06)
    }

    /// A slightly-recessed surface colour (text-edit backgrounds).
    pub fn sunken(&self) -> Color32 {
        mix(self.bg, Color32::BLACK, 0.10)
    }

    /// Install this theme as egui's active visual style.
    pub fn apply(&self, ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();
        visuals.override_text_color = Some(self.text);
        visuals.window_fill = self.bg;
        visuals.panel_fill = self.panel;
        visuals.faint_bg_color = self.raised();
        visuals.extreme_bg_color = self.sunken();
        visuals.hyperlink_color = self.accent;
        // Use the theme's own selection colours (matching the terminal UI's
        // selected-row look) rather than deriving them from the accent.
        visuals.selection.bg_fill = self.select_bg;
        visuals.selection.stroke = Stroke::new(1.0, self.select_fg);
        visuals.window_stroke = Stroke::new(1.0, mix(self.panel, self.text, 0.18));

        let w = &mut visuals.widgets;
        w.noninteractive.bg_fill = self.panel;
        w.noninteractive.weak_bg_fill = self.panel;
        w.noninteractive.fg_stroke = Stroke::new(1.0, self.dim);
        w.noninteractive.bg_stroke = Stroke::new(1.0, mix(self.panel, self.text, 0.10));

        w.inactive.bg_fill = self.raised();
        w.inactive.weak_bg_fill = self.raised();
        w.inactive.fg_stroke = Stroke::new(1.0, self.text);
        w.inactive.bg_stroke = Stroke::new(1.0, mix(self.panel, self.text, 0.12));

        w.hovered.bg_fill = mix(self.panel, self.accent, 0.30);
        w.hovered.weak_bg_fill = mix(self.panel, self.accent, 0.22);
        w.hovered.fg_stroke = Stroke::new(1.0, self.text);
        w.hovered.bg_stroke = Stroke::new(1.0, self.accent);

        w.active.bg_fill = mix(self.panel, self.accent, 0.45);
        w.active.weak_bg_fill = mix(self.panel, self.accent, 0.35);
        w.active.fg_stroke = Stroke::new(1.0, self.text);
        w.active.bg_stroke = Stroke::new(1.0, self.accent);

        w.open.bg_fill = self.raised();
        w.open.fg_stroke = Stroke::new(1.0, self.text);

        // egui 0.35 has no `Context::set_style`; mutate every theme variant's
        // style in place (we force our own colours, so dark/light are identical).
        ctx.all_styles_mut(|style| {
            style.visuals = visuals.clone();
            // Deliberately tighter than egui's defaults. Generous padding makes
            // controls read as large friendly buttons; a tool that professionals
            // look at all day wants a denser grid, and the extra rows it buys
            // are worth more than the air. Kept in one place so the whole app
            // moves together rather than drifting control by control.
            style.spacing.item_spacing = egui::vec2(5.0, 5.0);
            style.spacing.button_padding = egui::vec2(7.0, 3.0);
        });
    }
}

/// The badge colour for an HTTP method, from the shared `hurl::method_rgb`
/// table (so GET/POST/… colours match the terminal UI exactly).
pub fn method_color(method: &str) -> Color32 {
    match crate::hurl::method_rgb(method) {
        Some((r, g, b)) => Color32::from_rgb(r, g, b),
        None => Color32::GRAY,
    }
}
