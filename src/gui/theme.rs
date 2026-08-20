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
    pub line: Color32,
    field: Color32,
    raised: Color32,
    sunken: Color32,
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

/// Whether a ground colour is light, by relative luminance — the sRGB formula,
/// so a mid-tone is judged by how bright it actually looks rather than by the
/// sum of its channels.
fn is_light(c: Color32) -> bool {
    let f = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b()) > 0.5
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
            line: c(s.line),
            field: c(s.field),
            raised: c(s.raised),
            sunken: c(s.sunken),
        }
    }

    /// A raised surface (panel headers, buttons, inactive tabs).
    ///
    /// Read from the theme rather than mixed out of `panel`: the mix put this
    /// and [`Self::field`] two RGB points apart, so every button and every text
    /// box in the app were the same colour and a panel full of controls read as
    /// one undifferentiated grey.
    pub fn raised(&self) -> Color32 {
        self.raised
    }

    /// A recessed surface (scroll bars, code blocks).
    pub fn sunken(&self) -> Color32 {
        self.sunken
    }

    /// The wash behind an editable field.
    ///
    /// Still a tint rather than a sunken, outlined box — a screen of requests
    /// is mostly fields, and drawing each as a box turns the content anyone is
    /// reading into a minor detail inside furniture (see
    /// `widgets::flat_fields`, which brings a border in only when the pointer
    /// or the keyboard arrives). It is now a tint that is distinguishable from
    /// the surface a *button* is drawn on, which is the part that was missing.
    pub fn field(&self) -> Color32 {
        self.field
    }

    /// Install this theme as egui's active visual style.
    pub fn apply(&self, ctx: &egui::Context) {
        // egui keeps a `dark_mode` flag that a handful of widgets consult for
        // colours we don't override (shadows, the odd derived tint), so the
        // base is chosen from the theme's own ground rather than assumed —
        // otherwise a light theme gets dark-mode furniture in the corners.
        let mut visuals = if is_light(self.bg) {
            egui::Visuals::light()
        } else {
            egui::Visuals::dark()
        };
        visuals.override_text_color = Some(self.text);
        visuals.window_fill = self.bg;
        visuals.panel_fill = self.panel;
        visuals.faint_bg_color = self.raised();
        visuals.extreme_bg_color = self.sunken();
        // Text fields get their own, much lighter wash: `extreme_bg_color`
        // still dresses the scroll bars and code blocks, which do want to look
        // recessed.
        visuals.text_edit_bg_color = Some(self.field());
        visuals.hyperlink_color = self.accent;
        // Use the theme's own selection colours (matching the terminal UI's
        // selected-row look) rather than deriving them from the accent.
        visuals.selection.bg_fill = self.select_bg;
        visuals.selection.stroke = Stroke::new(1.0, self.select_fg);
        visuals.window_stroke = Stroke::new(1.0, self.line);

        let w = &mut visuals.widgets;
        w.noninteractive.bg_fill = self.panel;
        w.noninteractive.weak_bg_fill = self.panel;
        w.noninteractive.fg_stroke = Stroke::new(1.0, self.dim);
        // Separators and panel edges — structure, so the theme's own hairline
        // colour rather than another shade of the surface they sit on.
        w.noninteractive.bg_stroke = Stroke::new(1.0, self.line);

        w.inactive.bg_fill = self.raised();
        w.inactive.weak_bg_fill = self.raised();
        w.inactive.fg_stroke = Stroke::new(1.0, self.text);
        w.inactive.bg_stroke = Stroke::new(1.0, self.line);

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

#[cfg(test)]
mod tests {
    use super::*;

    /// The GUI's surfaces come from the theme now, not from a mix of `panel`:
    /// a field and a button used to land two RGB points apart, which is what
    /// made a dark theme look like one flat sheet with slightly different
    /// sheets on it.
    #[test]
    fn the_surfaces_come_from_the_theme_rather_than_from_panel() {
        for spec in crate::theme::builtin_presets() {
            let th = GuiTheme::from_spec(&spec);
            assert_eq!(th.field(), c(spec.field), "{}: field", spec.name);
            assert_eq!(th.raised(), c(spec.raised), "{}: raised", spec.name);
            assert_eq!(th.sunken(), c(spec.sunken), "{}: sunken", spec.name);
            assert_ne!(
                th.field(),
                th.raised(),
                "{}: a field and a button are not the same colour",
                spec.name
            );
        }
    }

    /// A light theme must not inherit egui's dark-mode furniture, and a dark
    /// one must keep it.
    #[test]
    fn the_base_visuals_follow_the_themes_own_ground() {
        let light = crate::theme::builtin_presets()
            .into_iter()
            .find(|p| p.name == crate::theme::PRESET_DAYLIGHT)
            .expect("the light preset is offered");
        for (spec, expect_dark) in [(crate::theme::default_preset(), true), (light, false)] {
            let ctx = egui::Context::default();
            GuiTheme::from_spec(&spec).apply(&ctx);
            // egui 0.35 has no `Context::style`; read it from inside a frame.
            let mut dark = None;
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                dark = Some(ui.visuals().dark_mode);
            });
            assert_eq!(dark, Some(expect_dark), "{}", spec.name);
        }
    }
}

/// The badge colour for an HTTP method, from the shared `hurl::method_rgb`
/// table (so GET/POST/… colours match the terminal UI exactly).
///
/// A method the table has no colour for (a custom verb) falls back to the
/// caller's `fallback` rather than a fixed grey, so every colour the GUI paints
/// still comes from the active theme.
pub fn method_color(method: &str, fallback: Color32) -> Color32 {
    match crate::hurl::method_rgb(method) {
        Some((r, g, b)) => Color32::from_rgb(r, g, b),
        None => fallback,
    }
}
