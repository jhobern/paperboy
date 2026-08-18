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
    KeySource, PostmanEvent, PostmanFlow, Step, default_dest_name, human_duration, item_kind_label,
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
    /// Where the API key lives. The user picks the provider and types only the
    /// part they can read off it (an item path, a parameter name); the wizard
    /// assembles the `{{ … }}` reference the resolver understands, so nobody
    /// has to learn that syntax to import a workspace.
    key_source: KeySource,
    /// What the user typed for the key: the credential itself under
    /// [`KeySource::Paste`], otherwise the address of one.
    key_entry: String,
    /// Whether the user has edited the destination. A blank field is filled in
    /// from the workspace name, but only until they say otherwise.
    dest_touched: bool,
    /// Put out of the way while it works. An import can take many minutes of
    /// deliberately paced calls, and there is nothing to answer during them —
    /// so it can be sent to the status bar and got on without.
    hidden: bool,
    /// The step a failure interrupted. The core replaces the whole step with
    /// `Step::Failed`, which suits the terminal UI's dedicated error screen but
    /// would throw a GUI user back to the first field over a bad path; keeping
    /// the previous step lets the error be shown *in place*.
    last_step: Step,
    /// What a finished import landed, kept so the receipt can say it. The
    /// dialog used to simply vanish on success, which left the user looking at
    /// a newly switched-to tab with no word of what had happened — the one
    /// moment in the whole flow where something is worth reading.
    done: Option<ImportSummary>,
}

impl Wizard {
    fn new() -> Self {
        // An API key in the environment is the one credential a user is likely
        // to have to hand, and a PMAK is miserable to type.
        let flow = PostmanFlow::new().with_env_key();
        let (key_source, key_entry) = KeySource::detect(&flow.key);
        Self {
            flow,
            key_source,
            key_entry,
            dest_touched: false,
            hidden: false,
            last_step: Step::Connect,
            done: None,
        }
    }

    /// Whether the wizard is *doing* something rather than *asking* something.
    /// The download is the only such stretch that lasts — the paced fetches
    /// can run for many minutes with no question in them — so it is the one
    /// the rest of the app stays usable through. The earlier steps are busy
    /// only in bursts, and going modeless under the user mid-burst would move
    /// the ground while they read.
    #[cfg(test)]
    fn seed_for_audit(&mut self, step: Step) {
        self.flow.key = "PMAK-x".into();
        self.flow.dest = "/tmp/pb/import".into();
        self.flow.seed_chosen(crate::postman_api::WorkspaceSummary {
            id: "ws-a".into(),
            name: "Alpha".into(),
            kind: crate::postman_api::WorkspaceKind::Team,
        });
        self.flow.seed_step(step);
        self.remember_step();
    }

    fn working(&self) -> bool {
        matches!(self.flow.step(), Step::Downloading)
    }

    /// Fold the key source and what was typed under it back into the flow's
    /// single key field, which is what the worker resolves.
    fn sync_key(&mut self) {
        self.flow.key = self.key_source.reference(&self.key_entry);
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
        // Once the key has actually worked, keep the reference: finding the
        // 1Password item path is the tedious half of setting an import up.
        if let Some(key) = self.flow.key_to_remember().map(str::to_string)
            && app.session.remember_key_ref(&key)
        {
            app.session.save();
        }
        match self.flow.poll(&app.strings) {
            Some(PostmanEvent::Imported(summary)) => {
                // Stay open on the receipt rather than closing: the import is
                // the one thing here that can finish while the user is looking
                // somewhere else entirely.
                self.done = Some((*summary).clone());
                finish_import(app, *summary);
                false
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
    /// Begin a bulk import from Postman. An import already under way is left
    /// alone and brought back into view instead: starting a second one would
    /// throw away however many paced minutes the first had spent.
    pub fn open(&mut self) {
        match &mut self.flow {
            Some(w) => w.hidden = false,
            None => self.flow = Some(Wizard::new()),
        }
    }

    pub fn is_open(&self) -> bool {
        self.flow.is_some()
    }

    /// The one-line progress for the status bar, present only while an import
    /// is running out of sight. `None` means there is nothing to say — either
    /// no import, or one that is on screen already.
    pub fn background_line(&self, s: &Strings) -> Option<String> {
        let w = self.flow.as_ref()?;
        if !w.hidden {
            return None;
        }
        let p = w.flow.progress();
        Some(if p.total > 0 {
            format!("{} {}/{}", s.postman_background, p.done, p.total)
        } else {
            format!(
                "{} {}",
                s.postman_background,
                w.flow.busy_line(s).unwrap_or_default()
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn seed_step_for_audit(&mut self, step: Step) {
        if let Some(w) = &mut self.flow {
            w.seed_for_audit(step);
        }
    }

    /// Land a finished import, so a test can paint the receipt without running
    /// one.
    #[cfg(test)]
    pub(crate) fn seed_done_for_audit(&mut self, summary: ImportSummary) {
        if let Some(w) = &mut self.flow {
            w.flow.seed_step(Step::Done);
            w.done = Some(summary);
        }
    }

    /// Put the connect step on a given key source, so a test can paint the
    /// explanation that source is supposed to carry.
    #[cfg(test)]
    pub(crate) fn seed_key_source_for_audit(&mut self, src: KeySource) {
        if let Some(w) = &mut self.flow {
            w.key_source = src;
        }
    }

    /// Bring a backgrounded import back on screen.
    pub fn reveal(&mut self) {
        if let Some(w) = &mut self.flow {
            w.hidden = false;
        }
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
    /// Put the dialog away and let the import carry on in the background.
    Hide,
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
    /// Reserved for what the import is *doing* — the paced waits. Field labels
    /// are dim, as they are everywhere else in the app.
    accent: egui::Color32,
    err: egui::Color32,
    text: egui::Color32,
    ok: egui::Color32,
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

    // Anything that isn't pure waiting wants the user back: a question to
    // answer, or an error to read. Only the working stretches can stay hidden.
    if w.hidden && !w.working() {
        w.hidden = false;
    }
    if w.hidden {
        ctx.request_repaint_after(REPAINT_WHILE_BUSY);
        app.postman.flow = Some(w);
        return;
    }

    let colors = UiColors {
        dim: app.theme.dim,
        accent: app.theme.accent,
        err: app.theme.err,
        text: app.theme.text,
        ok: app.theme.ok,
    };
    let mut action = UiAction::None;
    let import_base = app
        .session
        .picker_dir(crate::session::PickerKind::Import)
        .map(std::path::Path::to_owned);
    // Cloned rather than borrowed: the dialog closure needs `app.strings` too.
    let recent = app.session.recent_key_refs.clone();
    let strings = &app.strings;
    // Fixed width, reserving what the widest step needs: the connect step's
    // label changes with the key source ("API key" vs "1Password item") and a
    // dialog sized to its content grew and shrank around it, dragging the whole
    // form sideways while the user was reading it.
    // While it is only working, the dialog stops holding the app hostage: no
    // sheet behind it, and the rest of PaperBoy stays clickable underneath.
    let working = w.working();
    let title = strings.postman_title;
    let dismissed = if working {
        super::widgets::dialog_modeless(ctx, title, Some(CONNECT_WIDTH), |ui| {
            action = draw(ui, &mut w, &recent, colors, strings);
        })
        .dismissed
    } else {
        super::widgets::dialog(ctx, title, Some(CONNECT_WIDTH), |ui| {
            action = draw(ui, &mut w, &recent, colors, strings);
        })
        .dismissed
    };
    // The ✕ and Escape are the Cancel button by another name.
    if dismissed {
        action = UiAction::Cancel;
    }

    match action {
        UiAction::None => {}
        UiAction::Cancel => {
            w.flow.cancel();
            return;
        }
        UiAction::Hide => w.hidden = true,
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

fn draw(
    ui: &mut egui::Ui,
    w: &mut Wizard,
    recent: &[String],
    colors: UiColors,
    s: &Strings,
) -> UiAction {
    // The busy spinner is deliberately not shown while downloading: that step
    // has a progress bar, which says the same thing with more information.
    let downloading = matches!(w.flow.step(), Step::Downloading);
    if let Some(line) = w.flow.busy_line(s).filter(|_| !downloading) {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.colored_label(colors.dim, line);
        });
        // The allowance is the fact behind the wait: a listing paced to one
        // call every twelve seconds is a rate limit being respected, not a
        // stall, and only the headers can say which.
        if let Some(budget) = w.flow.budget_line(s) {
            ui.colored_label(colors.dim, budget);
        }
        ui.add_space(6.0);
    }
    if let Some(error) = w.flow.error() {
        ui.colored_label(colors.err, error);
        ui.add_space(6.0);
    }

    match w.step() {
        Step::Connect => draw_connect(ui, w, recent, colors, s),
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

/// Width reserved for the connect step's label column, and for the dialog as a
/// whole. Both are fixed so that changing the key source — which changes the
/// label beside the key field — doesn't move anything else on screen.
const LABEL_WIDTH: f32 = 130.0;
const CONNECT_WIDTH: f32 = 540.0;

fn draw_connect(
    ui: &mut egui::Ui,
    w: &mut Wizard,
    recent: &[String],
    colors: UiColors,
    s: &Strings,
) -> UiAction {
    let mut action = UiAction::None;
    let busy = w.flow.is_busy();

    // A label column with the field beside it, and what the field wants shown
    // as hint text inside it: an example is read where the answer goes, and
    // costs nothing once the field is filled in.
    egui::Grid::new("pb_postman_connect")
        .num_columns(2)
        .spacing([12.0, 6.0])
        // Wide enough for the longest label any source can put here, so the
        // value column stays where it is as the source changes.
        .min_col_width(LABEL_WIDTH)
        .show(ui, |ui| {
            ui.colored_label(colors.dim, s.postman_key_source_label);
            egui::ComboBox::from_id_salt("pb_postman_key_source")
                .width(200.0)
                .selected_text(w.key_source.source_label(s))
                .show_ui(ui, |ui| {
                    for src in KeySource::ALL {
                        if ui
                            .selectable_label(src == w.key_source, src.source_label(s))
                            .clicked()
                        {
                            w.key_source = src;
                        }
                    }
                });
            ui.end_row();

            ui.colored_label(colors.dim, w.key_source.field_label(s));
            // The references this key has been read from before, offered beside
            // the field: finding the 1Password item path is the tedious half of
            // setting an import up, and it is the same path every time. Only
            // the ones belonging to the chosen source — an SSM parameter is no
            // help to someone who has picked 1Password.
            let known: Vec<String> = recent
                .iter()
                .filter_map(|raw| {
                    let (src, entry) = KeySource::detect(raw);
                    (src == w.key_source && !entry.is_empty()).then_some(entry)
                })
                .collect();
            ui.horizontal(|ui| {
                // Only a pasted key is the credential; a reference is the
                // *address* of one, and masking that would only stop the user
                // checking it.
                ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut w.key_entry)
                        .password(w.key_source.is_secret())
                        .hint_text(w.key_source.field_hint(s))
                        .desired_width(if known.is_empty() { 340.0 } else { 250.0 }),
                );
                if !known.is_empty() {
                    ui.add_enabled_ui(!busy, |ui| {
                        ui.menu_button(s.gui_git_recent, |ui| {
                            for entry in &known {
                                if ui.button(entry).clicked() {
                                    w.key_entry = entry.clone();
                                    ui.close();
                                }
                            }
                        });
                    });
                }
            });
            ui.end_row();

            // Under the field, in the field's own column: what choosing this
            // source actually means. The picker names four sources but says
            // nothing about what each one needs of the user, which is the whole
            // question for someone who has never referenced a secret by
            // address before.
            ui.label("");
            ui.add(
                egui::Label::new(egui::RichText::new(w.key_source.field_help(s)).color(colors.dim))
                    .wrap(),
            );
            ui.end_row();

            ui.colored_label(colors.dim, s.postman_workspace_label);
            ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut w.flow.workspace_ref)
                    .hint_text(s.postman_workspace_hint)
                    .desired_width(f32::INFINITY),
            );
            ui.end_row();

            ui.colored_label(colors.dim, s.postman_base_url_label);
            ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut w.flow.base_url)
                    .hint_text(s.postman_base_url_hint)
                    .desired_width(f32::INFINITY),
            );
            ui.end_row();
        });
    w.sync_key();

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

    ui.label(
        egui::RichText::new(s.postman_pick_workspace)
            .strong()
            .color(colors.text),
    );
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
        .max_height(260.0)
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

    // No heading: the window's title bar already says what this dialog is.
    ui.checkbox(
        &mut w.flow.include_collections,
        s.postman_include_collections,
    );
    ui.checkbox(
        &mut w.flow.include_environments,
        s.postman_include_environments,
    );
    ui.add_space(8.0);

    ui.colored_label(colors.dim, s.postman_format_label);
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

    ui.colored_label(colors.dim, s.postman_dest_label);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut w.flow.dest)
                    .hint_text(s.postman_dest_unset)
                    .desired_width(360.0),
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

    ui.label(
        egui::RichText::new(s.postman_confirm_title)
            .strong()
            .color(colors.text),
    );
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
            format!(
                "{} {} {}",
                s.postman_estimate,
                human_duration(eta, s),
                s.postman_remaining
            ),
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
    if let Some(line) = w.flow.budget_line(s) {
        ui.colored_label(colors.dim, line);
    }

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .button(s.postman_background_button)
            .on_hover_text(s.postman_background_hint)
            .clicked()
        {
            action = UiAction::Hide;
        }
        if ui.button(s.gui_cancel).clicked() {
            action = UiAction::Cancel;
        }
    });
    action
}

/// Only reachable if opening the imported folder as a Workspace somehow did
/// not close the dialog; kept so the wizard can never be left with nothing on
/// screen and no way out.
fn draw_done(ui: &mut egui::Ui, w: &mut Wizard, colors: UiColors, s: &Strings) -> UiAction {
    let mut action = UiAction::None;
    let name = w
        .done
        .as_ref()
        .map(|d| d.workspace_name.clone())
        .unwrap_or_default();
    let heading = if name.trim().is_empty() {
        s.postman_done_title.to_string()
    } else {
        format!("{} — {name}", s.postman_done_title)
    };
    ui.label(egui::RichText::new(heading).strong().color(colors.ok));
    if let Some(d) = &w.done {
        ui.label(crate::postman_flow::imported_counts(
            d.collections,
            d.environments,
            s,
        ));
    }
    ui.add_space(4.0);
    // Where it went, and — the thing a vanishing dialog never said — that the
    // folder is now a tab, and which half of the window to look at.
    ui.colored_label(
        colors.dim,
        format!(
            "{} {}",
            s.postman_done_saved_to,
            w.flow.dest_path().to_string_lossy()
        ),
    );
    ui.label(s.postman_done_opened);
    let failures = w.flow.failures().len();
    if failures > 0 {
        ui.colored_label(colors.err, format!("{failures} {}", s.postman_skipped));
    }
    if w.done.as_ref().is_some_and(|d| d.converted_with_notes) {
        ui.colored_label(colors.dim, s.postman_notes_written);
    }
    ui.add_space(8.0);
    // Nothing is left to call off here, so the way out is Close, not Cancel.
    if ui.button(s.gui_close).clicked() {
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

    /// The GUI offers the same choice of key source the terminal does, and
    /// assembles the reference itself: a user who keeps their key in 1Password
    /// types the item path their password manager shows them, not the
    /// `{{ op://… }}` syntax around it.
    #[test]
    fn the_key_source_choice_writes_the_reference_the_resolver_understands() {
        let mut w = Wizard::new();
        w.key_source = KeySource::OnePassword;
        w.key_entry = "Private/Postman/credential".to_string();
        w.sync_key();
        assert_eq!(w.flow.key, "{{ op://Private/Postman/credential }}");
        assert!(
            !w.key_source.is_secret(),
            "an item path is an address, so it is shown rather than masked"
        );

        // A pasted key is the credential itself: used as typed, and hidden.
        w.key_source = KeySource::Paste;
        w.key_entry = "PMAK-abc".to_string();
        w.sync_key();
        assert_eq!(w.flow.key, "PMAK-abc");
        assert!(w.key_source.is_secret());
    }

    /// A key picked up from the environment arrives as a plain key, and the
    /// wizard has to open showing it that way round rather than as syntax.
    #[test]
    fn a_wizard_opens_on_the_source_its_existing_key_came_from() {
        let mut w = Wizard::new();
        w.flow.key = "{{ ssm:/prod/postman/api-key }}".to_string();
        let (src, entry) = KeySource::detect(&w.flow.key);
        assert_eq!(src, KeySource::Ssm);
        assert_eq!(entry, "/prod/postman/api-key");
    }

    /// Choosing where the key comes from is the first thing the importer asks
    /// and the least obvious: three of the four sources want the *address* of a
    /// key rather than a key, and one of them needs a command-line tool
    /// installed. The form has to say which, for whichever source is showing.
    #[test]
    fn the_connect_form_explains_the_key_source_that_is_showing() {
        use eframe::egui;

        fn painted(app: &mut GuiApp) -> String {
            fn walk(s: &egui::epaint::Shape, out: &mut Vec<String>) {
                match s {
                    egui::epaint::Shape::Text(t) => out.push(t.galley.text().to_string()),
                    egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                    _ => {}
                }
            }
            let ctx = egui::Context::default();
            app.theme.apply(&ctx);
            let mut last = Vec::new();
            for _ in 0..3 {
                let out = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::pos2(0.0, 0.0),
                            egui::vec2(1200.0, 800.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        let ctx = ui.ctx().clone();
                        super::show(app, &ctx);
                    },
                );
                last.clear();
                out.shapes.iter().for_each(|sh| walk(&sh.shape, &mut last));
            }
            // Wrapping is egui's business, so compare on the words.
            last.join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        }

        let s = s();
        for (src, help) in [
            (KeySource::OnePassword, s.postman_key_help_op),
            (KeySource::Ssm, s.postman_key_help_ssm),
            (KeySource::Env, s.postman_key_help_env),
            (KeySource::Paste, s.postman_key_help_paste),
        ] {
            let mut app = GuiApp::for_test(Session::default());
            app.postman.open();
            app.postman.seed_step_for_audit(Step::Connect);
            app.postman.seed_key_source_for_audit(src);
            let text = painted(&mut app);
            assert!(text.contains(help), "{src:?} went unexplained: {text}");
        }
    }

    /// An import that finishes leaves a receipt rather than vanishing. It can
    /// finish while the user is looking somewhere else entirely, and the only
    /// other sign of it — a tab quietly switching — says nothing about what
    /// landed or where.
    #[test]
    fn a_finished_import_leaves_a_receipt_saying_what_landed() {
        use eframe::egui;

        fn walk(sh: &egui::epaint::Shape, out: &mut Vec<String>) {
            match sh {
                egui::epaint::Shape::Text(t) => out.push(t.galley.text().to_string()),
                egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }

        let s = s();
        let mut app = GuiApp::for_test(Session::default());
        app.postman.open();
        app.postman.seed_done_for_audit(ImportSummary {
            dest: "/tmp/pb/Alpha".into(),
            workspace_name: "Alpha".into(),
            collections: 3,
            environments: 1,
            failures: Vec::new(),
            converted_with_notes: false,
            elapsed: Duration::from_secs(4),
        });

        let ctx = egui::Context::default();
        app.theme.apply(&ctx);
        let mut painted = Vec::new();
        for _ in 0..3 {
            let out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(1200.0, 800.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    let ctx = ui.ctx().clone();
                    super::show(&mut app, &ctx);
                },
            );
            painted.clear();
            out.shapes
                .iter()
                .for_each(|sh| walk(&sh.shape, &mut painted));
        }
        let text = painted
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");

        assert!(
            app.postman.is_open(),
            "the dialog stays up to be read, rather than vanishing"
        );
        assert!(
            text.contains("Alpha"),
            "the receipt names the workspace: {text}"
        );
        assert!(
            text.contains(&format!("3 {}", s.postman_word_collections)),
            "and what came with it: {text}"
        );
        // Singular where it is one — a receipt that says "1 environments"
        // reads like a machine.
        assert!(
            text.contains(&format!("1 {}", s.postman_word_environment)),
            "counted in the number it really is: {text}"
        );
        assert!(
            text.contains(s.postman_done_opened),
            "and says where the imported folder went: {text}"
        );
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

    /// An import is minutes of paced waiting with nothing to answer, so it is
    /// the one stretch the dialog steps out of the way for: no sheet behind
    /// it, and it can be put away entirely.
    #[test]
    fn only_the_download_lets_the_rest_of_the_app_carry_on() {
        for step in [
            Step::Connect,
            Step::PickWorkspace,
            Step::Options,
            Step::Confirm,
        ] {
            let mut w = wizard();
            w.flow.seed_step(step.clone());
            assert!(
                !w.working(),
                "{step:?} is asking something, so it must hold the app"
            );
        }
        let mut w = wizard();
        w.flow.seed_step(Step::Downloading);
        assert!(w.working());
    }

    /// The point of hiding it: the status bar keeps saying how far along the
    /// import is, and clicking that is the way back to it.
    #[test]
    fn a_backgrounded_import_still_reports_from_the_status_bar() {
        let mut ui = PostmanUi::default();
        assert_eq!(ui.background_line(&s()), None, "nothing running");

        ui.open();
        ui.flow.as_mut().unwrap().flow.seed_step(Step::Downloading);
        assert_eq!(
            ui.background_line(&s()),
            None,
            "an import on screen speaks for itself"
        );

        ui.flow.as_mut().unwrap().hidden = true;
        let line = ui.background_line(&s()).expect("a hidden import reports");
        assert!(line.starts_with(s().postman_background), "got {line:?}");

        ui.reveal();
        assert_eq!(ui.background_line(&s()), None);
    }

    /// Opening the importer while one is already running must not throw away
    /// however many paced minutes it has spent — it brings that one back.
    #[test]
    fn importing_again_returns_to_the_import_already_running() {
        let mut ui = PostmanUi::default();
        ui.open();
        ui.flow.as_mut().unwrap().flow.key = "PMAK-first".to_string();
        ui.flow.as_mut().unwrap().flow.seed_step(Step::Downloading);
        ui.flow.as_mut().unwrap().hidden = true;

        ui.open();
        let w = ui.flow.as_ref().unwrap();
        assert_eq!(w.flow.key, "PMAK-first", "the running import is kept");
        assert_eq!(w.flow.step(), &Step::Downloading);
        assert!(!w.hidden, "and it is brought back on screen");
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
