//! The response viewer's "assert this / capture this" builder.
//!
//! The GUI half of [`crate::probe`]: a right-click on a value in the response
//! body (or on a header row) that turns what the server sent into an
//! `[Asserts]` line or a `[Captures]` row on the request that fetched it. The
//! terminal UI reaches the same code through its `a` palette
//! ([`crate::tui::probe_menu`]) — the two front-ends differ only in how the
//! subject is pointed at, which is the one thing a mouse and a keyboard do
//! genuinely differently.

use eframe::egui;

use crate::probe::{self, Probe, Subject, Verb, subject_label, value_preview, verb_label};

use super::app::{Dialog, GuiApp};

/// State of the open builder dialog.
///
/// The dialog is *browsable* even when it was opened by pointing at a value:
/// the click resolves to one subject, but "actually, the field next to it" is
/// common enough that throwing the rest of the list away would be a worse
/// dialog than one that starts with the right row already chosen.
pub(crate) struct ProbeBuilder {
    /// The collection, by runtime id: the dialog outlives the frame that
    /// opened it, and a tab reorder in between must not redirect the write.
    pub(super) collection_id: u64,
    pub(super) entry: usize,
    /// Everything the response offers, unfiltered.
    pub(super) subjects: Vec<Probe>,
    pub(super) filter: String,
    /// The subject chosen so far; `None` while still choosing one.
    pub(super) chosen: Option<Probe>,
    /// The verbs for `chosen`.
    pub(super) verbs: Vec<Verb>,
    /// The variable name being typed, once "keep it in a variable" is chosen.
    pub(super) capture_name: Option<String>,
    /// Text selected in the body when the dialog opened — the only possible
    /// literal for a `body contains` on a reply that isn't JSON.
    pub(super) selection: Option<String>,
}

impl ProbeBuilder {
    /// The rows the filter leaves standing. Matched against the path *and* the
    /// value: "I can see this token on screen, which field is it?" is as common
    /// a way in as knowing the field's name.
    pub(super) fn visible(&self) -> Vec<&Probe> {
        if self.filter.trim().is_empty() {
            return self.subjects.iter().collect();
        }
        let needle = self.filter.trim().to_lowercase();
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
}

/// Open the builder over the selected request's last response.
///
/// `pointed_at` is the subject the user clicked, when they clicked one; it
/// pre-selects that row and skips straight to the verbs.
pub(super) fn open(app: &mut GuiApp, ctx: &egui::Context, pointed_at: Option<Probe>) {
    let ci = app.active_ci();
    let Some(col) = app.session.collections.get(ci) else {
        return;
    };
    let entry = col.selected_entry;
    let Some(response) = col
        .entries
        .get(entry)
        .and_then(|e| e.last_response.as_ref())
    else {
        return;
    };
    let subjects = probe::probes(
        response.status,
        response.duration_ms,
        &response.headers,
        &response.body,
    );
    if subjects.is_empty() {
        return;
    }
    let selection = body_selection(app, ctx);
    let mut builder = ProbeBuilder {
        collection_id: col.id,
        entry,
        subjects,
        filter: String::new(),
        chosen: None,
        verbs: Vec::new(),
        capture_name: None,
        selection: selection.clone(),
    };
    if let Some(probe) = pointed_at {
        builder.verbs = probe::verbs_for(&probe, selection.as_deref());
        builder.chosen = Some(probe);
    }
    app.dialog = Some(Dialog::ProbeBuilder(Box::new(builder)));
}

/// Whatever is selected in the response body field, when it is a single line
/// of it. Used as the literal for `body contains …`, and as the dialog's
/// opening filter.
fn body_selection(app: &GuiApp, ctx: &egui::Context) -> Option<String> {
    let state = egui::TextEdit::load_state(ctx, egui::Id::new("resp_body"))?;
    let range = state.cursor.char_range()?.as_sorted_char_range();
    if range.start.0 >= range.end.0 {
        return None;
    }
    let body = app
        .session
        .collections
        .get(app.active_ci())
        .and_then(|c| c.entries.get(c.selected_entry))
        .and_then(|e| e.last_response.as_ref())
        .map(|r| r.body.to_string())?;
    let text: String = body
        .chars()
        .skip(range.start.0)
        .take(range.end.0 - range.start.0)
        .collect();
    let text = text.trim().to_string();
    (!text.is_empty() && !text.contains('\n')).then_some(text)
}

/// The value the caret (or the start of the selection) sits on in the response
/// body, as a subject.
///
/// `None` while the compact view is on: it rewrites the text it shows, so an
/// offset into it no longer indexes the raw body and would resolve to the wrong
/// field. Silently asserting on the wrong path is far worse than the menu
/// falling back to the browsable list.
///
/// Takes the body rather than reading it off the app because it is called from
/// inside the response panel's own closures, which already hold the borrow.
pub(super) fn pointed_in(ctx: &egui::Context, body: &str, compact: bool) -> Option<Probe> {
    if compact {
        return None;
    }
    let state = egui::TextEdit::load_state(ctx, egui::Id::new("resp_body"))?;
    let range = state.cursor.char_range()?.as_sorted_char_range();
    let offset = body
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(body.len()))
        .nth(range.start.0)?;
    probe::probe_at(body, offset)
}

/// Write the chosen verb onto the request the response came from.
///
/// Every index is re-checked rather than trusted: the user can switch request,
/// reorder tabs or close the collection while the dialog is up, and writing an
/// assert into whatever now sits at that position would be silent corruption.
pub(super) fn apply(app: &mut GuiApp, builder: &ProbeBuilder, verb: &Verb, name: &str) -> bool {
    let Some(probe) = builder.chosen.as_ref() else {
        return false;
    };
    let Some(ci) = app
        .session
        .collections
        .iter()
        .position(|c| c.id == builder.collection_id)
    else {
        return false;
    };
    let Some(col) = app.session.collections.get_mut(ci) else {
        return false;
    };
    let Some(target) = col.entries.get_mut(builder.entry) else {
        return false;
    };
    let changed = match verb {
        Verb::ExpectStatus(code) => {
            let fresh = target.expected_status != Some(*code);
            target.expected_status = Some(*code);
            fresh
        }
        Verb::Assert(predicate) => {
            match probe::assert_line(probe.subject.clone(), predicate.clone()) {
                Some(line) if !target.asserts.contains(&line) => {
                    target.asserts.push(line);
                    true
                }
                _ => false,
            }
        }
        Verb::Capture => {
            let name = name.trim();
            if name.is_empty() {
                return false;
            }
            let row = probe::capture_row(&probe.subject, name);
            // A name already in use is replaced rather than added beside: two
            // `[Captures]` rows with one name is a file where the second
            // silently wins, and nobody means that.
            match target.captures.iter_mut().find(|(n, _)| n == &row.0) {
                Some(existing) if existing.1 == row.1 => false,
                Some(existing) => {
                    existing.1 = row.1;
                    true
                }
                None => {
                    target.captures.push(row);
                    true
                }
            }
        }
    };
    if changed {
        target.modified = true;
        col.invalidate_request_json();
        app.session.save();
    }
    changed
}

/// A name for a capture of this subject that isn't already taken on the entry.
pub(super) fn suggested_name(app: &GuiApp, builder: &ProbeBuilder, subject: &Subject) -> String {
    let taken: Vec<String> = app
        .session
        .collections
        .iter()
        .find(|c| c.id == builder.collection_id)
        .and_then(|c| c.entries.get(builder.entry))
        .map(|e| e.captures.iter().map(|(n, _)| n.clone()).collect())
        .unwrap_or_default();
    probe::suggest_name(subject, &taken)
}

/// One row of the subject list, as it reads in the dialog.
pub(super) fn subject_row(probe: &Probe) -> String {
    let value = value_preview(probe.value.as_ref(), 48);
    if value.is_empty() {
        subject_label(&probe.subject)
    } else {
        format!("{}   {value}", subject_label(&probe.subject))
    }
}

/// One row of the verb list: the assert line itself, so the row is a preview of
/// what will be written into the file.
pub(super) fn verb_row(subject: &Subject, verb: &Verb, s: &crate::i18n::Strings) -> String {
    verb_label(subject, verb, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hurl::HurlEntry;

    /// One answered request, selected, in one collection.
    fn app_with_response(body: &str) -> GuiApp {
        let mut entry = HurlEntry {
            title: "Login".to_string(),
            ..Default::default()
        };
        entry.last_response = Some(crate::http::ApiResponse {
            status: 201,
            body: std::sync::Arc::from(body),
            headers: vec![("Content-Type".into(), "application/json".into())],
            duration_ms: Some(90),
            ..Default::default()
        });
        let mut session = crate::session::Session::default();
        session.collections.clear();
        session.collections.push(crate::collection::Collection::new(
            "api".into(),
            vec![entry],
        ));
        GuiApp::for_test(session)
    }

    fn builder(app: &GuiApp, body: &str) -> ProbeBuilder {
        ProbeBuilder {
            collection_id: app.session.collections[0].id,
            entry: 0,
            subjects: probe::probes(201, Some(90), &[], body),
            filter: String::new(),
            chosen: None,
            verbs: Vec::new(),
            capture_name: None,
            selection: None,
        }
    }

    #[test]
    fn choosing_a_value_and_a_verb_writes_the_assert() {
        let body = r#"{"token":"abc"}"#;
        let mut app = app_with_response(body);
        let mut b = builder(&app, body);
        let probe = b
            .subjects
            .iter()
            .find(|p| subject_label(&p.subject) == "$.token")
            .unwrap()
            .clone();
        b.verbs = probe::verbs_for(&probe, None);
        b.chosen = Some(probe);
        assert!(apply(&mut app, &b, &b.verbs[0], ""));
        let entry = &app.session.collections[0].entries[0];
        assert_eq!(entry.asserts, [r#"jsonpath "$.token" == "abc""#]);
        assert!(entry.modified);
        // The same choice twice adds nothing the second time.
        assert!(!apply(&mut app, &b, &b.verbs[0], ""));
        assert_eq!(app.session.collections[0].entries[0].asserts.len(), 1);
    }

    #[test]
    fn capturing_writes_a_row_under_the_typed_name() {
        let body = r#"{"data":{"access_token":"ey.."}}"#;
        let mut app = app_with_response(body);
        let mut b = builder(&app, body);
        let probe = b
            .subjects
            .iter()
            .find(|p| subject_label(&p.subject) == "$.data.access_token")
            .unwrap()
            .clone();
        // The name offered is the field's own, which is what anyone would type.
        assert_eq!(suggested_name(&app, &b, &probe.subject), "access_token");
        b.chosen = Some(probe);
        assert!(apply(&mut app, &b, &Verb::Capture, "tok"));
        assert_eq!(
            app.session.collections[0].entries[0].captures,
            [(
                "tok".to_string(),
                "jsonpath \"$.data.access_token\"".to_string()
            )]
        );
    }

    /// The status has a line of its own in Hurl; adding it to `[Asserts]` too
    /// would be a second claim that can disagree with the first.
    #[test]
    fn the_status_sets_the_http_line() {
        let mut app = app_with_response("{}");
        let mut b = builder(&app, "{}");
        let probe = b
            .subjects
            .iter()
            .find(|p| matches!(p.subject, Subject::Status))
            .unwrap()
            .clone();
        b.verbs = probe::verbs_for(&probe, None);
        b.chosen = Some(probe);
        assert!(apply(&mut app, &b, &b.verbs[0], ""));
        let entry = &app.session.collections[0].entries[0];
        assert_eq!(entry.expected_status, Some(201));
        assert!(entry.asserts.is_empty());
    }

    /// The dialog stays browsable after a click resolves one value, and the
    /// filter reads both halves of a row.
    #[test]
    fn the_filter_matches_the_path_or_the_value() {
        let body = r#"{"token":"abcdef","user":{"id":7}}"#;
        let app = app_with_response(body);
        let mut b = builder(&app, body);
        b.filter = "abcdef".into();
        assert_eq!(
            b.visible()
                .iter()
                .map(|p| subject_label(&p.subject))
                .collect::<Vec<_>>(),
            ["$.token"]
        );
        b.filter = "user".into();
        assert!(
            b.visible()
                .iter()
                .any(|p| subject_label(&p.subject) == "$.user.id")
        );
    }

    /// A request the user switched away from must not receive the assert: the
    /// dialog outlives the frame that opened it.
    #[test]
    fn a_closed_collection_is_not_written_to() {
        let body = r#"{"a":1}"#;
        let mut app = app_with_response(body);
        let mut b = builder(&app, body);
        let probe = b
            .subjects
            .iter()
            .find(|p| subject_label(&p.subject) == "$.a")
            .unwrap()
            .clone();
        b.verbs = probe::verbs_for(&probe, None);
        b.chosen = Some(probe);
        b.collection_id = u64::MAX;
        assert!(!apply(&mut app, &b, &b.verbs[0], ""));
        assert!(app.session.collections[0].entries[0].asserts.is_empty());
    }

    /// A row of the list carries the value beside the path, so it settles "is
    /// this the field I mean?" without opening it.
    #[test]
    fn a_row_shows_the_value_beside_the_path() {
        let row = subject_row(&Probe {
            subject: Subject::Json("$.token".into()),
            value: Some(serde_json::json!("abcdef")),
        });
        assert!(row.contains("$.token"), "{row}");
        assert!(row.contains("abcdef"), "{row}");
    }
}
