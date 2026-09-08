//! The response panel's "assert this / capture this" picker.
//!
//! Pressing `a` in the Response pane opens this: a two-step palette over
//! [`crate::probe`]. The first step is *what* — every value the reply offers,
//! filterable by typing. The second is *what about it* — the assert lines that
//! subject can carry, each spelled out in full so the row shows exactly what
//! is about to be written, plus "keep it in a variable".
//!
//! It exists because the alternative is reading a jsonpath off the screen and
//! typing it back into the request wizard by hand, which is slow and is the
//! easiest place in the app to introduce a typo that later looks like a server
//! fault.

use crate::i18n::Strings;
use crate::probe::{self, Predicate, Probe, Subject};
use crate::tui::app::{Overlay, PromptKind};
use crate::tui::line_editor::Editor;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Which of the two steps the palette is on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum ProbeStep {
    /// Choosing the subject: status, duration, a header, a value in the body.
    PickSubject,
    /// Choosing what to say about the subject picked in step one.
    PickVerb,
}

/// What the second step can do with the chosen subject.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Verb {
    /// Append this line to `[Asserts]`.
    Assert(Predicate),
    /// Set the entry's expected status, which lives on the `HTTP <status>`
    /// line rather than in `[Asserts]` — Hurl's own placement for it.
    ExpectStatus(u16),
    /// Add a `[Captures]` row, after asking for the variable name.
    Capture,
}

/// State of the assert/capture palette.
pub(crate) struct ProbeMenu {
    pub(crate) step: ProbeStep,
    /// Every subject the response offers. The *whole* list, never narrowed in
    /// place, so backspacing over the filter brings the hidden rows straight
    /// back (the same rule as the report setting menu).
    pub(crate) subjects: Vec<Probe>,
    /// The subject chosen in step one; `None` while still on step one.
    pub(crate) chosen: Option<Probe>,
    /// The second step's rows, built from the chosen subject.
    pub(crate) verbs: Vec<Verb>,
    /// What has been typed to narrow step one. Empty means "show everything".
    pub(crate) filter: String,
    /// The cursor, as an index into the **visible** rows.
    pub(crate) selected: usize,
    /// The collection the request belongs to, by runtime id rather than index
    /// so a tab reorder between opening the menu and choosing can't write the
    /// assert into somebody else's request.
    pub(crate) collection_id: u64,
    /// Which request in that collection, by its position at open time.
    pub(crate) entry: usize,
    /// Text the user had selected in the response panel when the menu opened.
    /// It seeds the filter (so selecting a token narrows straight to the field
    /// holding it) and supplies the literal for a `body contains` on a reply
    /// that isn't JSON.
    pub(crate) selection: Option<String>,
}

impl ProbeMenu {
    /// The rows step one's filter leaves standing, in document order.
    ///
    /// Case-insensitive substring, matched against both the path and the value:
    /// "I can see `4f2a…` on screen, which field is that?" is as common a way
    /// in as knowing the field's name.
    pub(crate) fn visible(&self) -> Vec<&Probe> {
        if self.filter.is_empty() {
            return self.subjects.iter().collect();
        }
        let needle = self.filter.to_lowercase();
        self.subjects
            .iter()
            .filter(|p| {
                subject_label(&p.subject).to_lowercase().contains(&needle)
                    || value_preview(p.value.as_ref(), usize::MAX)
                        .to_lowercase()
                        .contains(&needle)
            })
            .collect()
    }

    /// The number of rows currently on screen, whichever step is showing.
    pub(crate) fn row_count(&self) -> usize {
        match self.step {
            ProbeStep::PickSubject => self.visible().len(),
            ProbeStep::PickVerb => self.verbs.len(),
        }
    }

    /// The subject the cursor is on, or `None` when the filter matches nothing.
    pub(crate) fn choice(&self) -> Option<Probe> {
        self.visible().get(self.selected).map(|p| (*p).clone())
    }

    /// Keep the cursor on a row that exists after the filter changed. It goes
    /// to the top rather than following the previous row: the point of typing
    /// is to bring the wanted row *to* the top.
    pub(crate) fn clamp_selection(&mut self) {
        self.selected = 0;
    }

    /// The overlay title for the current step.
    pub(crate) fn title(&self, s: &Strings) -> String {
        match (&self.step, &self.chosen) {
            (ProbeStep::PickSubject, _) => s.probe_pick_subject_title.to_string(),
            (ProbeStep::PickVerb, Some(p)) => subject_label(&p.subject),
            (ProbeStep::PickVerb, None) => s.probe_pick_verb_title.to_string(),
        }
    }

    /// Move to step two on the chosen subject, or stay put when the filter
    /// matches nothing (there is no subject to say anything about).
    pub(crate) fn advance(&mut self) -> bool {
        let Some(chosen) = self.choice() else {
            return false;
        };
        self.verbs = verbs_for(&chosen, self.selection.as_deref());
        if self.verbs.is_empty() {
            return false;
        }
        self.chosen = Some(chosen);
        self.step = ProbeStep::PickVerb;
        self.selected = 0;
        true
    }

    /// Back to step one with the filter (and so the list) as it was — Esc from
    /// step two should undo one decision, not the whole trip.
    pub(crate) fn retreat(&mut self) {
        self.step = ProbeStep::PickSubject;
        self.chosen = None;
        self.verbs.clear();
        self.selected = 0;
    }
}

/// The verbs worth offering for a subject.
///
/// `selection` is the text highlighted in the response panel, which is the only
/// possible source of a literal for a body that isn't JSON: there is no value
/// to pre-fill from, so `body contains …` is offered only when the user has
/// already pointed at the text they mean.
pub(crate) fn verbs_for(probe: &Probe, selection: Option<&str>) -> Vec<Verb> {
    let mut out = Vec::new();
    for predicate in probe::default_predicates(probe) {
        match (&probe.subject, &predicate) {
            (Subject::Status, Predicate::Eq(v)) => match v.parse::<u16>() {
                Ok(code) => out.push(Verb::ExpectStatus(code)),
                Err(_) => continue,
            },
            (Subject::Body, Predicate::Contains(_)) => {
                let Some(text) = selection.map(str::trim).filter(|t| !t.is_empty()) else {
                    continue;
                };
                out.push(Verb::Assert(Predicate::Contains(
                    probe::literal(&serde_json::Value::String(text.to_string()))
                        .unwrap_or_default(),
                )));
            }
            _ => {
                // Anything Hurl has no spelling for is dropped rather than
                // shown as a row that produces nothing when chosen.
                if probe::assert_line(probe.subject.clone(), predicate.clone()).is_some() {
                    out.push(Verb::Assert(predicate));
                }
            }
        }
    }
    // Capturing is offered wherever there is a query to capture — which is
    // everywhere except the body-as-text fallback, where the "value" is the
    // whole reply and no variable wants that.
    if !matches!(probe.subject, Subject::Body) {
        out.push(Verb::Capture);
    }
    out
}

/// How a subject reads in the picker: the jsonpath for a body value, and a
/// worded form for the parts of the exchange that aren't one.
pub(crate) fn subject_label(subject: &Subject) -> String {
    match subject {
        Subject::Json(path) => path.clone(),
        Subject::JsonCount(path) => format!("{path} []"),
        Subject::Header(name) => format!("header {name}"),
        Subject::Status => "status".to_string(),
        Subject::Duration => "duration".to_string(),
        Subject::Body => "body".to_string(),
    }
}

/// The observed value, shortened to fit a row.
///
/// Shown next to every subject because the path alone rarely settles "is this
/// the field I'm looking at?", and because the value is what the offered
/// equality assert is going to contain.
pub(crate) fn value_preview(value: Option<&serde_json::Value>, max: usize) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let raw = match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(m) => format!("{{{}}}", m.len()),
        serde_json::Value::Array(a) => format!("[{}]", a.len()),
        other => other.to_string(),
    };
    // One line: a preview that wraps would push the rows below it off the
    // bottom of a list whose whole job is to be scanned.
    let raw = raw.replace(['\n', '\r', '\t'], " ");
    if max == usize::MAX || raw.chars().count() <= max {
        return raw;
    }
    let head: String = raw.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// How a verb reads in step two: the assert line itself, so the row shows
/// exactly what will be written to the file.
pub(crate) fn verb_label(subject: &Subject, verb: &Verb, s: &Strings) -> String {
    match verb {
        Verb::Assert(predicate) => probe::assert_line(subject.clone(), predicate.clone())
            .unwrap_or_else(|| s.probe_verb_unavailable.to_string()),
        Verb::ExpectStatus(code) => format!("HTTP {code}"),
        Verb::Capture => s.probe_verb_capture.to_string(),
    }
}

impl crate::tui::app::TuiApp {
    /// `a` in the Response pane: open the assert/capture palette over whatever
    /// the selected request last received.
    ///
    /// Refuses (with a status) when there is no response: every row of this
    /// menu is derived from a reply, so an empty one would be a menu with
    /// nothing in it and no explanation.
    pub(crate) fn open_probe_menu(&mut self) {
        let ci = self.active_tab;
        let Some(col) = self.collections.get(ci) else {
            return;
        };
        let entry_idx = col.selected_entry;
        let Some(response) = col
            .entries
            .get(entry_idx)
            .and_then(|e| e.last_response.as_ref())
        else {
            self.status = Some(crate::i18n::Status::NoResponse);
            return;
        };
        let subjects = probe::probes(
            response.status,
            response.duration_ms,
            &response.headers,
            &response.body,
        );
        if subjects.is_empty() {
            self.status = Some(crate::i18n::Status::NoResponse);
            return;
        }
        // A selection is the user having already pointed at the value they
        // mean, so it seeds the filter: highlight a token in the body and the
        // list opens on the field holding it. It is only a *seed* — the filter
        // can be backspaced away like any other, which is why this is nicer
        // than resolving the selection to one path and offering nothing else.
        let selection = self
            .concatenated_selection_text()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty() && !t.contains('\n'));
        let mut menu = ProbeMenu {
            step: ProbeStep::PickSubject,
            subjects,
            chosen: None,
            verbs: Vec::new(),
            filter: String::new(),
            selected: 0,
            collection_id: col.id,
            entry: entry_idx,
            selection: selection.clone(),
        };
        // Only if it actually narrows to something: a seed that matches nothing
        // opens an empty list, which reads as "this response has no values in
        // it" rather than "your selection isn't one of them".
        if let Some(seed) = selection {
            let trimmed = seed.trim_matches(['"', ',', ':', ' ']).to_string();
            menu.filter = trimmed;
            if menu.visible().is_empty() {
                menu.filter.clear();
            }
        }
        self.overlay = Some(Overlay::ProbeMenu(Box::new(menu)));
    }

    pub(crate) fn probe_menu_key_handler(&mut self, key: KeyEvent, mut menu: Box<ProbeMenu>) {
        let last = menu.row_count().saturating_sub(1);
        match key.code {
            KeyCode::Up => {
                menu.selected = menu.selected.saturating_sub(1);
                self.overlay = Some(Overlay::ProbeMenu(menu));
            }
            KeyCode::Down => {
                menu.selected = (menu.selected + 1).min(last);
                self.overlay = Some(Overlay::ProbeMenu(menu));
            }
            KeyCode::Home => {
                menu.selected = 0;
                self.overlay = Some(Overlay::ProbeMenu(menu));
            }
            KeyCode::End => {
                menu.selected = last;
                self.overlay = Some(Overlay::ProbeMenu(menu));
            }
            KeyCode::PageUp => {
                menu.selected = menu.selected.saturating_sub(10);
                self.overlay = Some(Overlay::ProbeMenu(menu));
            }
            KeyCode::PageDown => {
                menu.selected = (menu.selected + 10).min(last);
                self.overlay = Some(Overlay::ProbeMenu(menu));
            }
            // Esc from step two goes back one decision rather than closing:
            // "not that one, but still an assert" is the common correction.
            KeyCode::Esc if menu.step == ProbeStep::PickVerb => {
                menu.retreat();
                self.overlay = Some(Overlay::ProbeMenu(menu));
            }
            // Typing narrows step one. A JSON body of any size makes an
            // unfiltered list unscrollable, and the field name is nearly always
            // the thing the user knows. Step two is a handful of rows, so it
            // takes no filter and letters do nothing there.
            KeyCode::Char(c)
                if menu.step == ProbeStep::PickSubject
                    && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                menu.filter.push(c);
                menu.clamp_selection();
                self.overlay = Some(Overlay::ProbeMenu(menu));
            }
            KeyCode::Backspace if menu.step == ProbeStep::PickSubject => {
                // Backspace on an empty filter closes the menu, so the key that
                // undoes typing keeps undoing right out of the overlay.
                if menu.filter.pop().is_some() {
                    menu.clamp_selection();
                    self.overlay = Some(Overlay::ProbeMenu(menu));
                }
            }
            KeyCode::Enter if menu.step == ProbeStep::PickSubject => {
                // A filter matching nothing has no subject to advance on;
                // closing the menu would look like a pick was made.
                menu.advance();
                self.overlay = Some(Overlay::ProbeMenu(menu));
            }
            KeyCode::Enter => self.apply_probe_verb(*menu),
            // Esc / anything else: cancel (the overlay was already taken).
            _ => {}
        }
    }

    /// Write the chosen verb onto the request the response came from.
    fn apply_probe_verb(&mut self, menu: ProbeMenu) {
        let (Some(probe), Some(verb)) =
            (menu.chosen.as_ref(), menu.verbs.get(menu.selected).cloned())
        else {
            return;
        };
        // Addressed by collection id, not tab index: the menu may have been
        // open across a tab reorder, and writing an assert into whichever
        // request now sits at that index would be silent corruption.
        let Some(ci) = self
            .collections
            .iter()
            .position(|c| c.id == menu.collection_id)
        else {
            return;
        };
        if self.collections[ci].entries.get(menu.entry).is_none() {
            return;
        }
        match verb {
            Verb::Capture => {
                self.open_probe_capture_prompt(ci, menu.entry, probe.subject.clone());
                return;
            }
            Verb::ExpectStatus(code) => {
                let entry = &mut self.collections[ci].entries[menu.entry];
                if entry.expected_status == Some(code) {
                    self.status = Some(crate::i18n::Status::ProbeAlreadyThere);
                    return;
                }
                entry.expected_status = Some(code);
                entry.modified = true;
                self.status = Some(crate::i18n::Status::ProbeStatusSet(code));
            }
            Verb::Assert(predicate) => {
                let Some(line) = probe::assert_line(probe.subject.clone(), predicate) else {
                    return;
                };
                let entry = &mut self.collections[ci].entries[menu.entry];
                if entry.asserts.contains(&line) {
                    self.status = Some(crate::i18n::Status::ProbeAlreadyThere);
                    return;
                }
                entry.asserts.push(line.clone());
                entry.modified = true;
                self.status = Some(crate::i18n::Status::ProbeAssertAdded(line));
            }
        }
        self.after_probe_edit(ci);
    }

    /// Ask for the variable name a capture should use, pre-filled with one
    /// derived from the field itself.
    fn open_probe_capture_prompt(&mut self, ci: usize, entry: usize, subject: Subject) {
        let taken: Vec<String> = self.collections[ci].entries[entry]
            .captures
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        let name = probe::suggest_name(&subject, &taken);
        let s = Strings::for_language(&self.language);
        let collection_id = self.collections[ci].id;
        self.overlay = Some(Overlay::Prompt {
            kind: PromptKind::ProbeCapture {
                collection_id,
                entry,
                subject: Box::new(subject),
            },
            editor: Editor::new(&name, false),
            title: s.probe_capture_name_title.to_string(),
            mask: false,
            reset_to: None,
            secret_intact: false,
            secret_checkbox: None,
        });
    }

    /// Commit the capture once its name has been typed.
    pub(crate) fn finish_probe_capture(
        &mut self,
        collection_id: u64,
        entry: usize,
        subject: &Subject,
        name: &str,
    ) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let Some(ci) = self.collections.iter().position(|c| c.id == collection_id) else {
            return;
        };
        let Some(target) = self.collections[ci].entries.get_mut(entry) else {
            return;
        };
        let row = probe::capture_row(subject, name);
        if target.captures.contains(&row) {
            self.status = Some(crate::i18n::Status::ProbeAlreadyThere);
            return;
        }
        // A name already in use is *replaced* rather than added beside: two
        // `[Captures]` rows with one name is a file where the second silently
        // wins, and nobody means that.
        match target.captures.iter_mut().find(|(n, _)| n == &row.0) {
            Some(existing) => existing.1 = row.1.clone(),
            None => target.captures.push(row.clone()),
        }
        target.modified = true;
        self.status = Some(crate::i18n::Status::ProbeCaptureAdded(row.0));
        self.after_probe_edit(ci);
    }

    /// Shared bookkeeping after an assert or capture is written: the request
    /// preview and the folder tree both cache what the entry says.
    fn after_probe_edit(&mut self, ci: usize) {
        self.collections[ci].invalidate_request_json();
        self.collections[ci].sync_folder_to_selected();
        self.save_state();
    }
}
