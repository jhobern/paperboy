//! The GUI's "import a whole Postman workspace" wizard.
//!
//! Every decision about *what happens next* lives in [`crate::postman_flow`],
//! shared with the terminal UI so the two front-ends cannot drift. What remains
//! here is the desktop presentation of it: the dialog, its text fields, a
//! native folder picker for the destination, and a live progress bar.
//!
//! All state lives in [`PostmanUi`], so the rest of the GUI needs one field on
//! [`GuiApp`] and one [`show`] call per frame; [`PostmanUi::open`] starts it.

use std::time::Duration;

use eframe::egui;

use crate::i18n::{Status, Strings};
use crate::postman_flow::{
    PostmanEvent, PostmanFlow, Step, default_dest_name, human_duration, item_kind_label,
};
use crate::postman_import::{ImportFormat, ImportSummary, WaitReason};

use super::app::GuiApp;

/// egui only repaints on input, so a progress bar and an ETA would freeze
/// mid-import without a standing request to come back.
const REPAINT_WHILE_BUSY: Duration = Duration::from_millis(100);

/// All Postman-import UI state. Owned by [`GuiApp`]; `Default` = nothing open.
#[derive(Default)]
pub struct PostmanUi {
    flow: Option<Wizard>,
}

struct Wizard {
    /// The shared state machine — the terminal UI drives the same one, so the
    /// two front-ends cannot disagree about how an import behaves.
    flow: PostmanFlow,
    /// Whether the user has edited the destination. A blank field is filled in
    /// from the workspace name, but only until they say otherwise.
    dest_touched: bool,
    /// The step a failure interrupted. The core replaces the whole step with
    /// `Step::Failed`, which suits the terminal UI's dedicated error screen but
    /// would throw a GUI user back to the first field over a bad path; keeping
    /// the previous step lets the error be shown *in place*.
    last_step: Step,
}

impl Wizard {
    fn new() -> Self {
        // An API key in the environment is the one credential a user is likely
        // to have to hand, and a PMAK is miserable to type.
        Self {
            flow: PostmanFlow::new().with_env_key(),
            dest_touched: false,
            last_step: Step::Connect,
        }
    }

    /// The step to draw: the last real one when the flow is showing a failure —
    /// or, when that step has nothing to show, the last one that has. A
    /// rejected API key fails during the workspace listing, so the step it
    /// interrupted is a picker for a list that was never fetched; the error
    /// belongs over the key field it came from.
    fn step(&self) -> Step {
        match self.flow.step() {
            Step::Failed(_) => self.flow.recoverable(self.last_step.clone()),
            step => step.clone(),
        }
    }

    /// Called once per frame, before drawing, so a failure never loses the
    /// step it interrupted.
    fn remember_step(&mut self) {
        if !matches!(self.flow.step(), Step::Failed(_)) {
            self.last_step = self.flow.step().clone();
        }
    }

    /// Fill a blank destination in from the chosen workspace's name — a
    /// suggestion, never an override.
    /// `base` is where the last import landed, if there was one; imports are
    /// collected somewhere deliberate, so last time's folder beats any default.
    fn suggest_dest(&mut self, base: Option<&std::path::Path>) {
        if self.dest_touched && !self.flow.dest.trim().is_empty() {
            return;
        }
        // The working directory of a desktop app is wherever the launcher
        // happened to start it, which is rarely somewhere a user wants files;
        // the home directory is at least predictable.
        let base = base
            .filter(|p| p.is_dir())
            .map(std::path::Path::to_owned)
            .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        self.flow.dest = base
            .join(default_dest_name(self.flow.workspace_name()))
            .to_string_lossy()
            .into_owned();
    }

    /// Advance the shared flow and act on a finished import. Returns whether
    /// the dialog should close.
    fn poll(&mut self, app: &mut GuiApp) -> bool {
        match self.flow.poll(&app.strings) {
            Some(PostmanEvent::Imported(summary)) => {
                finish_import(app, *summary);
                true
            }
            None => false,
        }
    }
}

/// A finished import: open the folder it produced as a Workspace, exactly as
/// if the user had picked it with the folder browser — the point of the whole
/// feature.
fn finish_import(app: &mut GuiApp, summary: ImportSummary) {
    // Only one status line fits, so the more actionable of the two wins:
    // missing data beats a note about data deliberately dropped.
    if let Some(parent) = summary.dest.parent() {
        app.session
            .remember_picker_dir(crate::session::PickerKind::Import, parent);
    }
    if !summary.failures.is_empty() {
        app.session.status = Some(Status::PostmanSkipped(summary.failures.len()));
    } else if summary.converted_with_notes {
        app.session.status = Some(Status::PostmanNotes);
    }
    app.session.open_workspace(summary.dest);
    app.session.save();
}

impl PostmanUi {
    /// Begin a bulk import from Postman.
    pub fn open(&mut self) {
        self.flow = Some(Wizard::new());
    }

    fn is_open(&self) -> bool {
        self.flow.is_some()
    }
}

/// What a frame's worth of clicks asked for. Collected rather than acted on
/// inside the closure, because every one of these needs `&mut GuiApp`, which
/// the drawing code deliberately does not have.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum UiAction {
    #[default]
    None,
    Cancel,
    /// Step 1: take the key and either list the workspaces or, when an id was
    /// supplied, skip straight to the options.
    Connect,
    /// Step 2: take the highlighted workspace.
    PickWorkspace,
    /// Step 3: validate the options and start planning.
    StartPlanning,
    /// Step 4: approve the plan and let the parked worker download.
    Confirm,
    /// Open a native folder picker for the destination.
    BrowseDest,
    BackToConnect,
    BackToWorkspaces,
}

#[derive(Clone, Copy)]
struct UiColors {
    dim: egui::Color32,
    accent: egui::Color32,
    err: egui::Color32,
}

/// Render the Postman dialog (if any) and drive its worker. Call once per frame.
pub fn show(app: &mut GuiApp, ctx: &egui::Context) {
    if !app.postman.is_open() {
        return;
    }
    let Some(mut w) = app.postman.flow.take() else {
        return;
    };
    w.remember_step();
    if w.poll(app) {
        return;
    }

    let colors = UiColors {
        dim: app.theme.dim,
        accent: app.theme.accent,
        err: app.theme.err,
    };
    let mut action = UiAction::None;
    let import_base = app
        .session
        .picker_dir(crate::session::PickerKind::Import)
        .map(std::path::Path::to_owned);
    let strings = &app.strings;
    egui::Window::new(strings.postman_title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_min_width(520.0);
            action = draw(ui, &mut w, colors, strings);
        });

    match action {
        UiAction::None => {}
        UiAction::Cancel => {
            w.flow.cancel();
            return;
        }
        UiAction::Connect => {
            w.flow.submit_connect(&app.strings);
            if matches!(w.flow.step(), Step::Options) {
                w.suggest_dest(import_base.as_deref());
            }
        }
        UiAction::PickWorkspace => {
            if w.flow.submit_workspace() {
                w.suggest_dest(import_base.as_deref());
            }
        }
        UiAction::StartPlanning => {
            w.flow.submit_options(&app.strings);
        }
        UiAction::Confirm => {
            w.flow.confirm();
        }
        UiAction::BrowseDest => {
            let seed = std::path::PathBuf::from(w.flow.dest.trim());
            let seed = seed.parent().filter(|p| p.exists());
            app.request_pick(
                super::filepick::PickKind::Folder,
                app.strings.postman_dest_label,
                seed,
                super::menu::PickAction::PostmanDest,
            );
        }
        UiAction::BackToConnect => w.flow.back_to_connect(),
        UiAction::BackToWorkspaces => w.flow.to_pick_workspace(),
    }

    if w.flow.is_busy() {
        ctx.request_repaint_after(REPAINT_WHILE_BUSY);
    }
    app.postman.flow = Some(w);
}

fn draw(ui: &mut egui::Ui, w: &mut Wizard, colors: UiColors, s: &Strings) -> UiAction {
    // The busy spinner is deliberately not shown while downloading: that step
    // has a progress bar, which says the same thing with more information.
    let downloading = matches!(w.flow.step(), Step::Downloading);
    if let Some(phase) = w.flow.busy().filter(|_| !downloading) {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.colored_label(colors.dim, phase.label(s));
        });
        ui.add_space(6.0);
    }
    if let Some(error) = w.flow.error() {
        ui.colored_label(colors.err, error);
        ui.add_space(6.0);
    }

    match w.step() {
        Step::Connect => draw_connect(ui, w, colors, s),
        Step::PickWorkspace => draw_pick_workspace(ui, w, colors, s),
        Step::Options => draw_options(ui, w, colors, s),
        Step::Confirm => draw_confirm(ui, w, colors, s),
        Step::Downloading => draw_downloading(ui, w, colors, s),
        Step::Done => draw_done(ui, w, colors, s),
        // `step()` never returns this — it falls back to `last_step` so an
        // error can be drawn in place, above.
        Step::Failed(_) => UiAction::None,
    }
}

fn draw_connect(ui: &mut egui::Ui, w: &mut Wizard, colors: UiColors, s: &Strings) -> UiAction {
    let mut action = UiAction::None;
    let busy = w.flow.is_busy();

    ui.colored_label(colors.accent, s.postman_key_label);
    ui.add_enabled(
        !busy,
        egui::TextEdit::singleline(&mut w.flow.key)
            .password(true)
            .desired_width(f32::INFINITY),
    );
    ui.colored_label(colors.dim, s.postman_key_hint);
    ui.add_space(8.0);

    ui.colored_label(colors.accent, s.postman_workspace_label);
    ui.add_enabled(
        !busy,
        egui::TextEdit::singleline(&mut w.flow.workspace_ref).desired_width(f32::INFINITY),
    );
    ui.colored_label(colors.dim, s.postman_workspace_hint);
    ui.add_space(8.0);

    ui.collapsing(s.postman_base_url_label, |ui| {
        ui.add_enabled(
            !busy,
            egui::TextEdit::singleline(&mut w.flow.base_url).desired_width(f32::INFINITY),
        );
        ui.colored_label(colors.dim, s.postman_base_url_hint);
    });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!busy, egui::Button::new(s.gui_git_next))
            .clicked()
        {
            action = UiAction::Connect;
        }
        if ui.button(s.gui_cancel).clicked() {
            action = UiAction::Cancel;
        }
    });
    action
}

fn draw_pick_workspace(
    ui: &mut egui::Ui,
    w: &mut Wizard,
    colors: UiColors,
    s: &Strings,
) -> UiAction {
    let mut action = UiAction::None;
    let busy = w.flow.is_busy();

    ui.colored_label(colors.accent, s.postman_pick_workspace);
    ui.add_enabled(
        !busy,
        egui::TextEdit::singleline(&mut w.flow.filter)
            .hint_text(s.git_filter_label)
            .desired_width(f32::INFINITY),
    );
    ui.add_space(4.0);

    let names: Vec<String> = w
        .flow
        .visible_workspaces()
        .iter()
        .map(|ws| ws.name.clone())
        .collect();
    let mut selected = w.flow.selected;
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .show(ui, |ui| {
            for (i, name) in names.iter().enumerate() {
                if ui.selectable_label(selected == i, name).clicked() {
                    selected = i;
                }
            }
        });
    w.flow.selected = selected;

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !busy && !names.is_empty(),
                egui::Button::new(s.gui_git_next),
            )
            .clicked()
        {
            action = UiAction::PickWorkspace;
        }
        if ui
            .add_enabled(!busy, egui::Button::new(s.gui_git_back))
            .clicked()
        {
            action = UiAction::BackToConnect;
        }
        if ui.button(s.gui_cancel).clicked() {
            action = UiAction::Cancel;
        }
    });
    action
}

fn draw_options(ui: &mut egui::Ui, w: &mut Wizard, colors: UiColors, s: &Strings) -> UiAction {
    let mut action = UiAction::None;
    let busy = w.flow.is_busy();

    ui.colored_label(colors.accent, s.postman_options_title);
    ui.add_space(4.0);
    ui.checkbox(
        &mut w.flow.include_collections,
        s.postman_include_collections,
    );
    ui.checkbox(
        &mut w.flow.include_environments,
        s.postman_include_environments,
    );
    ui.add_space(8.0);

    ui.colored_label(colors.accent, s.postman_format_label);
    ui.radio_value(&mut w.flow.format, ImportFormat::Raw, s.postman_format_raw);
    ui.radio_value(
        &mut w.flow.format,
        ImportFormat::Hurl,
        s.postman_format_hurl,
    );
    if w.flow.format == ImportFormat::Hurl {
        ui.colored_label(colors.dim, s.postman_format_hurl_note);
    }
    ui.add_space(8.0);

    ui.colored_label(colors.accent, s.postman_dest_label);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut w.flow.dest).desired_width(360.0),
            )
            .changed()
        {
            w.dest_touched = true;
        }
        if ui
            .add_enabled(!busy, egui::Button::new(s.gui_browse))
            .clicked()
        {
            action = UiAction::BrowseDest;
        }
    });
    ui.checkbox(&mut w.flow.overwrite, s.postman_overwrite);

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!busy, egui::Button::new(s.gui_git_next))
            .clicked()
        {
            action = UiAction::StartPlanning;
        }
        if ui
            .add_enabled(!busy, egui::Button::new(s.gui_git_back))
            .clicked()
        {
            // Back to wherever the workspace came from: the list if there is
            // one, otherwise the key prompt (an id was typed instead).
            action = if w.flow.workspaces().is_empty() {
                UiAction::BackToConnect
            } else {
                UiAction::BackToWorkspaces
            };
        }
        if ui.button(s.gui_cancel).clicked() {
            action = UiAction::Cancel;
        }
    });
    action
}

fn draw_confirm(ui: &mut egui::Ui, w: &mut Wizard, colors: UiColors, s: &Strings) -> UiAction {
    let mut action = UiAction::None;
    // The plan is still being fetched; the spinner above already says so.
    let Some(plan) = w.flow.plan() else {
        ui.horizontal(|ui| {
            if ui.button(s.gui_cancel).clicked() {
                action = UiAction::Cancel;
            }
        });
        return action;
    };

    ui.colored_label(colors.accent, s.postman_confirm_title);
    ui.add_space(4.0);
    ui.label(format!(
        "{} {} · {} {}",
        plan.collections.len(),
        s.postman_word_collections,
        plan.environments.len(),
        s.postman_word_environments
    ));
    ui.add_space(6.0);
    ui.colored_label(colors.dim, s.postman_rate_limit_note);
    ui.label(format!(
        "{} {}",
        s.postman_estimate,
        human_duration(plan.estimated_duration(), s)
    ));
    if plan.strains_monthly_budget() {
        ui.colored_label(colors.err, s.postman_budget_warning);
    }

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button(s.postman_start).clicked() {
            action = UiAction::Confirm;
        }
        if ui.button(s.gui_cancel).clicked() {
            action = UiAction::Cancel;
        }
    });
    action
}

fn draw_downloading(ui: &mut egui::Ui, w: &mut Wizard, colors: UiColors, s: &Strings) -> UiAction {
    let mut action = UiAction::None;
    let p = w.flow.progress();

    ui.add(
        egui::ProgressBar::new(p.fraction().clamp(0.0, 1.0))
            .text(format!("{}/{}", p.done, p.total)),
    );
    ui.add_space(4.0);
    let current = match p.current_kind {
        Some(kind) => format!("{}: {}", item_kind_label(kind, s), p.current),
        None => p.current.clone(),
    };
    ui.label(current);
    if let Some(eta) = p.eta() {
        ui.colored_label(
            colors.dim,
            format!("{} {}", human_duration(eta, s), s.postman_remaining),
        );
    }
    // A paced import spends most of its life deliberately idle; saying so is
    // the difference between "working" and "hung".
    if let Some((reason, secs)) = &p.waiting {
        let label = match reason {
            WaitReason::Pacing => s.postman_waiting_paced,
            WaitReason::RateLimited => s.postman_waiting_limited,
        };
        ui.colored_label(colors.accent, format!("{label} ({secs}s)"));
    }

    ui.add_space(8.0);
    if ui.button(s.gui_cancel).clicked() {
        action = UiAction::Cancel;
    }
    action
}

/// Only reachable if opening the imported folder as a Workspace somehow did
/// not close the dialog; kept so the wizard can never be left with nothing on
/// screen and no way out.
fn draw_done(ui: &mut egui::Ui, w: &mut Wizard, colors: UiColors, s: &Strings) -> UiAction {
    let mut action = UiAction::None;
    ui.colored_label(colors.accent, s.postman_done_title);
    ui.label(w.flow.dest_path().to_string_lossy().into_owned());
    let failures = w.flow.failures().len();
    if failures > 0 {
        ui.colored_label(colors.err, format!("{failures} {}", s.postman_skipped));
    }
    ui.add_space(8.0);
    if ui.button(s.gui_cancel).clicked() {
        action = UiAction::Cancel;
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;
    use crate::postman_api::{WorkspaceKind, WorkspaceSummary};
    use crate::postman_import::ImportPlan;
    use crate::session::Session;

    fn s() -> Strings {
        Strings::for_language(&Language::English)
    }

    fn a_workspace(name: &str, id: &str) -> WorkspaceSummary {
        WorkspaceSummary {
            id: id.to_string(),
            name: name.to_string(),
            kind: WorkspaceKind::Team,
        }
    }

    fn a_plan() -> ImportPlan {
        ImportPlan {
            workspace_id: "ws-a".to_string(),
            workspace_name: "Alpha".to_string(),
            collections: Vec::new(),
            environments: Vec::new(),
            remaining_month: None,
        }
    }

    fn wizard() -> Wizard {
        let mut w = Wizard::new();
        w.flow.key = "PMAK-test".to_string();
        w
    }

    /// The whole reason `last_step` exists: the core replaces the step with
    /// `Failed`, which would otherwise throw a GUI user back to the first
    /// field. The message must show *where the mistake was made*.
    #[test]
    fn a_rejected_option_leaves_the_user_on_the_options_with_the_reason() {
        let mut w = wizard();
        w.flow.seed_chosen(a_workspace("Alpha", "ws-a"));
        w.flow.seed_step(Step::Options);
        w.remember_step();

        w.flow.dest = String::new();
        assert!(!w.flow.submit_options(&s()));

        assert_eq!(w.step(), Step::Options, "the user stays on the options");
        assert_eq!(w.flow.error(), Some(s().postman_err_dest_required));
    }

    /// A wizard showing no step at all would be a dead dialog, so every core
    /// step must map to something drawable.
    #[test]
    fn every_core_step_maps_to_a_drawable_step() {
        for step in [
            Step::Connect,
            Step::PickWorkspace,
            Step::Options,
            Step::Confirm,
            Step::Downloading,
            Step::Done,
        ] {
            let mut w = wizard();
            w.flow.seed_step(step.clone());
            w.remember_step();
            assert_eq!(w.step(), step);
        }
    }

    #[test]
    fn the_destination_is_suggested_from_the_workspace_until_the_user_says_otherwise() {
        let mut w = wizard();
        w.flow.seed_chosen(a_workspace("Alpha Team", "ws-a"));
        w.suggest_dest(None);
        assert!(
            w.flow.dest.ends_with("Alpha Team"),
            "the folder is named after the workspace, got {:?}",
            w.flow.dest
        );

        // Once edited, a later suggestion must not stomp on it.
        w.dest_touched = true;
        w.flow.dest = "/somewhere/mine".to_string();
        w.flow.seed_chosen(a_workspace("Beta", "ws-b"));
        w.suggest_dest(None);
        assert_eq!(w.flow.dest, "/somewhere/mine");
    }

    /// A blank field is still a suggestion opportunity even after the user has
    /// been in it — otherwise clearing it strands the wizard with no path.
    #[test]
    fn a_cleared_destination_is_suggested_again() {
        let mut w = wizard();
        w.dest_touched = true;
        w.flow.dest = "   ".to_string();
        w.flow.seed_chosen(a_workspace("Gamma", "ws-g"));
        w.suggest_dest(None);
        assert!(w.flow.dest.ends_with("Gamma"));
    }

    /// The confirmation step must not offer to start an import it has no plan
    /// for; the plan arrives asynchronously while the step is already showing.
    #[test]
    fn confirming_before_the_plan_arrives_does_nothing() {
        let mut w = wizard();
        w.flow.seed_chosen(a_workspace("Alpha", "ws-a"));
        w.flow.seed_step(Step::Confirm);
        assert!(!w.flow.confirm());
        assert_eq!(w.flow.step(), &Step::Confirm, "still waiting");

        w.flow.seed_plan(a_plan());
        // Still no worker parked on the go channel, so this is as far as a
        // hand-built flow goes — but the plan is now what gates it.
        assert!(w.flow.plan().is_some());
    }

    /// A finished import is only worth anything if the folder it produced is
    /// opened; this is the end the whole wizard exists to reach.
    #[test]
    fn a_finished_import_opens_the_folder_as_a_workspace() {
        let root = std::env::temp_dir().join(format!("pb_gui_postman_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Collections")).unwrap();

        let mut app = GuiApp::for_test(Session::default());
        let before = app.session.collections.len();
        finish_import(
            &mut app,
            ImportSummary {
                dest: root.clone(),
                workspace_name: "Alpha".to_string(),
                collections: 1,
                environments: 0,
                failures: Vec::new(),
                converted_with_notes: true,
                elapsed: Duration::from_secs(1),
            },
        );

        assert_eq!(app.session.collections.len(), before + 1);
        assert_eq!(
            app.session
                .collections
                .last()
                .unwrap()
                .workspace_root
                .as_deref(),
            Some(root.as_path())
        );
        assert!(matches!(app.session.status, Some(Status::PostmanNotes)));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Only one status line fits, so the more actionable of the two wins.
    #[test]
    fn skipped_items_are_reported_ahead_of_the_conversion_notes() {
        let root = std::env::temp_dir().join(format!("pb_gui_postman_f_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let mut app = GuiApp::for_test(Session::default());
        finish_import(
            &mut app,
            ImportSummary {
                dest: root.clone(),
                workspace_name: "Alpha".to_string(),
                collections: 1,
                environments: 0,
                failures: vec![("Billing API".to_string(), "404".to_string())],
                converted_with_notes: true,
                elapsed: Duration::from_secs(1),
            },
        );
        assert!(matches!(
            app.session.status,
            Some(Status::PostmanSkipped(1))
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Leaving the options must return to whichever step the workspace came
    /// from, not unconditionally to the key prompt.
    #[test]
    fn going_back_from_the_options_returns_to_wherever_the_workspace_came_from() {
        // Typed id: nothing was ever listed, so back means the key prompt.
        let mut w = wizard();
        w.flow.seed_chosen(a_workspace("Alpha", "ws-a"));
        w.flow.seed_step(Step::Options);
        assert!(w.flow.workspaces().is_empty());
        w.flow.back_to_connect();
        assert_eq!(w.flow.step(), &Step::Connect);

        // Picked from a list: back means the list, and it is still there.
        let mut w = wizard();
        w.flow.seed_workspaces(vec![
            a_workspace("Alpha", "ws-a"),
            a_workspace("Beta", "ws-b"),
        ]);
        w.flow.seed_chosen(a_workspace("Alpha", "ws-a"));
        w.flow.seed_step(Step::Options);
        w.flow.to_pick_workspace();
        assert_eq!(w.flow.step(), &Step::PickWorkspace);
        assert_eq!(w.flow.workspaces().len(), 2, "the listing is not refetched");
    }

    /// The dialog must actually render on every step. Without this, a panic in
    /// a step the tests never reach — a missing plan, an empty list — would
    /// only be found by a user.
    #[test]
    fn every_step_of_the_dialog_renders() {
        let steps = [
            Step::Connect,
            Step::PickWorkspace,
            Step::Options,
            Step::Confirm,
            Step::Downloading,
            Step::Done,
            Step::Failed("boom".to_string()),
        ];
        for step in steps {
            let mut app = GuiApp::for_test(Session::default());
            app.postman.open();
            {
                let w = app.postman.flow.as_mut().unwrap();
                w.flow.seed_workspaces(vec![a_workspace("Alpha", "ws-a")]);
                w.flow.seed_chosen(a_workspace("Alpha", "ws-a"));
                w.flow.seed_step(step.clone());
            }
            let ctx = egui::Context::default();
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| show(&mut app, ui.ctx()));
            assert!(
                app.postman.is_open(),
                "{step:?} closed the dialog without anything happening"
            );
        }
    }

    /// The confirmation step renders before its plan arrives — the plan is
    /// fetched while the step is already on screen, so a `None` there is the
    /// normal case, not an edge one.
    #[test]
    fn the_confirmation_step_renders_while_the_plan_is_still_being_fetched() {
        let mut app = GuiApp::for_test(Session::default());
        app.postman.open();
        {
            let w = app.postman.flow.as_mut().unwrap();
            w.flow.seed_chosen(a_workspace("Alpha", "ws-a"));
            w.flow.seed_step(Step::Confirm);
        }
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| show(&mut app, ui.ctx()));
        assert!(app.postman.is_open());
        assert!(app.postman.flow.as_ref().unwrap().flow.plan().is_none());
    }

    /// Going back to the key discards the listing — a workspace list belongs to
    /// the key that fetched it, and that is exactly what is being changed.
    #[test]
    fn going_back_to_the_key_discards_the_listing() {
        let mut w = wizard();
        w.flow.seed_workspaces(vec![a_workspace("Alpha", "ws-a")]);
        w.flow.seed_step(Step::PickWorkspace);
        w.flow.back_to_connect();
        assert!(w.flow.workspaces().is_empty());
    }
}

/// Write back the folder the destination picker returned, frames later (see
/// [`super::filepick`] for why it can't be written back at the click).
pub(super) fn apply_picked_dest(app: &mut GuiApp, picked: Option<std::path::PathBuf>) {
    let Some(dir) = picked else {
        return; // cancelled
    };
    let Some(w) = app.postman.flow.as_mut() else {
        return; // the wizard closed while the dialog was open
    };
    // The picker names an existing *parent*; the import wants its own folder
    // inside it, or it would scatter Collections/ and Environments/ into
    // whatever the user happened to pick.
    w.flow.dest = dir
        .join(default_dest_name(w.flow.workspace_name()))
        .to_string_lossy()
        .into_owned();
    w.dest_touched = true;
}
