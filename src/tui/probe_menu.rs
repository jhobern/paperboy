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
use crate::probe::{self, Probe, Subject, Verb, subject_label, value_preview, verbs_for};
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

/// One row of step one: a subject, or the door to the response's headers.
pub(crate) enum ProbeRow<'a> {
    /// The collapsed headers, carrying how many are behind it.
    Headers(usize),
    Subject(&'a Probe),
}

impl ProbeRow<'_> {
    /// The row's left-hand column.
    pub(crate) fn label(&self, s: &Strings) -> String {
        match self {
            ProbeRow::Headers(_) => s.probe_headers_group.to_string(),
            ProbeRow::Subject(p) => subject_label(&p.subject),
        }
    }

    /// The row's right-hand column: what the value is, or how many headers
    /// are waiting behind the group row.
    pub(crate) fn value(&self, width: usize, s: &Strings) -> String {
        match self {
            ProbeRow::Headers(n) => {
                crate::i18n::fill(s.probe_headers_group_count, &[&n.to_string()])
            }
            ProbeRow::Subject(p) => value_preview(p.value.as_ref(), width),
        }
    }
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
    /// Whether the header rows are showing on their own.
    ///
    /// A reply carries a dozen headers nobody came here for, and listed flat
    /// they pushed the body -- the reason the palette was opened -- off the
    /// bottom of the screen. So they collapse to one row that opens them, in
    /// the place they used to occupy. Typing a filter shows matching headers
    /// straight away: someone who types "content-type" knows what they want
    /// and should not have to open a group first.
    pub(crate) headers_open: bool,
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
    pub(crate) fn visible(&self) -> Vec<ProbeRow<'_>> {
        let matching: Vec<&Probe> = self.subjects.iter().filter(|p| self.matches(p)).collect();
        let is_header = |p: &&Probe| matches!(p.subject, Subject::Header(_));
        if self.headers_open {
            return matching
                .into_iter()
                .filter(is_header)
                .map(ProbeRow::Subject)
                .collect();
        }
        if !self.filter.is_empty() {
            return matching.into_iter().map(ProbeRow::Subject).collect();
        }
        let headers = matching.iter().copied().filter(|p| is_header(&p)).count();
        let mut out = Vec::new();
        let mut group_placed = false;
        for probe in matching {
            if matches!(probe.subject, Subject::Header(_)) {
                // One row where the headers were, so the list keeps its order:
                // status, duration, headers, then the body.
                if !group_placed {
                    out.push(ProbeRow::Headers(headers));
                    group_placed = true;
                }
                continue;
            }
            out.push(ProbeRow::Subject(probe));
        }
        out
    }

    /// Whether a subject survives the typed filter.
    fn matches(&self, probe: &Probe) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        let needle = self.filter.to_lowercase();
        subject_label(&probe.subject)
            .to_lowercase()
            .contains(&needle)
            || value_preview(probe.value.as_ref(), usize::MAX)
                .to_lowercase()
                .contains(&needle)
    }

    /// The number of rows currently on screen, whichever step is showing.
    pub(crate) fn row_count(&self) -> usize {
        match self.step {
            ProbeStep::PickSubject => self.visible().len(),
            ProbeStep::PickVerb => self.verbs.len(),
        }
    }

    /// The subject the cursor is on, or `None` on the headers group (which is
    /// not a subject) or when the filter matches nothing.
    pub(crate) fn choice(&self) -> Option<Probe> {
        match self.visible().get(self.selected) {
            Some(ProbeRow::Subject(p)) => Some((*p).clone()),
            _ => None,
        }
    }

    /// Whether the cursor is on the row that opens the headers.
    pub(crate) fn on_headers_group(&self) -> bool {
        matches!(
            self.visible().get(self.selected),
            Some(ProbeRow::Headers(_))
        )
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
            (ProbeStep::PickSubject, _) if self.headers_open => {
                s.probe_pick_header_title.to_string()
            }
            (ProbeStep::PickSubject, _) => s.probe_pick_subject_title.to_string(),
            (ProbeStep::PickVerb, Some(p)) => subject_label(&p.subject),
            (ProbeStep::PickVerb, None) => s.probe_pick_verb_title.to_string(),
        }
    }

    /// Move to step two on the chosen subject, or stay put when the filter
    /// matches nothing (there is no subject to say anything about).
    pub(crate) fn advance(&mut self) -> bool {
        // The headers row is a door, not a subject: it opens the list it
        // stands for and stays on step one.
        if self.on_headers_group() {
            self.headers_open = true;
            self.selected = 0;
            return false;
        }
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

    /// Back out of the opened header list to the whole response.
    pub(crate) fn close_headers(&mut self) {
        self.headers_open = false;
        self.filter.clear();
        self.selected = 0;
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
            self.status = Some(crate::i18n::Status::ProbeNoResponse);
            return;
        };
        let subjects = probe::probes(
            response.status,
            response.duration_ms,
            &response.headers,
            &response.body,
        );
        if subjects.is_empty() {
            self.status = Some(crate::i18n::Status::ProbeNothingToProbe);
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
            headers_open: false,
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
                // One backspace too many while clearing the filter is not a
                // request to abandon the search: this used to close the
                // palette, throwing away a subject the user had just narrowed
                // down to. Only Esc closes it.
                menu.filter.pop();
                menu.clamp_selection();
                self.overlay = Some(Overlay::ProbeMenu(menu));
            }
            KeyCode::Enter if menu.step == ProbeStep::PickSubject => {
                // A filter matching nothing has no subject to advance on;
                // closing the menu would look like a pick was made.
                menu.advance();
                self.overlay = Some(Overlay::ProbeMenu(menu));
            }
            KeyCode::Enter => self.apply_probe_verb(*menu),
            // Esc backs out of the opened headers before it closes anything:
            // one press undoes one step, as it does on step two.
            KeyCode::Esc if menu.headers_open => {
                menu.close_headers();
                self.overlay = Some(Overlay::ProbeMenu(menu));
            }
            // Esc closes from step one (step two retreats, above).
            KeyCode::Esc => {}
            // A key with nothing to do does nothing, on either step. Falling
            // through to a cancel arm threw away the subject the user had just
            // hunted down and let the *next* keystroke land in the main view:
            // typing "contains" out of habit dismissed the palette, opened the
            // New Request wizard on the `n`, and typed the rest into its name
            // field. The same held for ←/→, which switch response section in
            // the main view and so closed the palette by passing through it.
            // The palette is closed on purpose -- Esc, or backspacing out of
            // an empty filter -- and not by accident.
            _ => {
                self.overlay = Some(Overlay::ProbeMenu(menu));
            }
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
                entry.mark_edited();
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
                entry.mark_edited();
                self.status = Some(crate::i18n::Status::ProbeAssertAdded(line));
            }
        }
        self.after_probe_edit(ci);
    }

    /// Ask for the variable name a capture should use, pre-filled with one
    /// derived from the field itself.
    fn open_probe_capture_prompt(&mut self, ci: usize, entry: usize, subject: Subject) {
        let captures = &self.collections[ci].entries[entry].captures;
        // Capturing a field that is already captured suggests the name it
        // already has. Suggesting a *fresh* name instead (`token_2`, because
        // `token` was taken) wrote the same query twice under two names, which
        // is not a thing anyone means to do; offering the existing name makes
        // the default answer a no-op, and typing over it is still a deliberate
        // second alias.
        let query = probe::capture_row(&subject, "").1;
        let name = match captures.iter().find(|(_, q)| q == &query) {
            Some((existing, _)) => existing.clone(),
            None => {
                let taken: Vec<String> = captures.iter().map(|(n, _)| n.clone()).collect();
                probe::suggest_name(&subject, &taken)
            }
        };
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
        target.mark_edited();
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
