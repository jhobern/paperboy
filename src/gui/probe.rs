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
    /// The entry's index *and* its stable [`HurlEntry::uid`]. The id is what
    /// the write actually addresses (see [`apply`]); the index is only a
    /// fallback for an entry that has never been stamped (uid 0). Pinning by
    /// index alone let a reload or reorder that renumbers the requests land the
    /// assert on whatever now sits at that position.
    pub(super) entry: usize,
    pub(super) entry_uid: u64,
    /// Everything the response offers, unfiltered.
    pub(super) subjects: Vec<Probe>,
    pub(super) filter: String,
    /// The subject chosen so far; `None` while still choosing one.
    pub(super) chosen: Option<Probe>,
    /// The verbs for `chosen`.
    pub(super) verbs: Vec<Verb>,
    /// The variable name being typed, once "keep it in a variable" is chosen.
    pub(super) capture_name: Option<String>,
    /// Set when Add was pressed on an empty name: the name step shows why the
    /// row was not written instead of the dialog vanishing silently.
    pub(super) name_required: bool,
    /// Text selected in the body when the dialog opened — the only possible
    /// literal for a `body contains` on a reply that isn't JSON.
    pub(super) selection: Option<String>,
    /// Which row of the value list is picked out, as an index into
    /// [`Self::visible`]. Picking and *acting* are separate: a list where the
    /// first click commits gives no way to look at a row before choosing it,
    /// and no way to change your mind.
    pub(super) selected: usize,
    /// The same for the verb list.
    pub(super) selected_verb: usize,
    /// The subject the response body is currently highlighting. Kept so the
    /// highlight is written only when it changes: rewriting the field's
    /// selection every frame would fight anything else that touches it.
    pub(super) highlighted: Option<Subject>,
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
    let entry_uid = col.entries.get(entry).map(|e| e.uid).unwrap_or(0);
    let mut builder = ProbeBuilder {
        collection_id: col.id,
        entry,
        entry_uid,
        subjects,
        filter: String::new(),
        chosen: None,
        verbs: Vec::new(),
        capture_name: None,
        name_required: false,
        selection: selection.clone(),
        selected: 0,
        selected_verb: 0,
        highlighted: None,
    };
    if let Some(probe) = pointed_at {
        builder.verbs = probe::verbs_for(&probe, selection.as_deref());
        builder.chosen = Some(probe);
    }
    app.dialog = Some(Dialog::ProbeBuilder(Box::new(builder)));
}

/// The egui id the response body `TextEdit` is drawn under — and read back
/// from. It carries the selected entry's identity so a selection made in one
/// reply cannot be read back as the *next* reply's: egui keeps cursor state per
/// widget id, and a constant id let one request's selection outlive it and be
/// sliced out of another request's body.
///
/// Keyed on the stable [`HurlEntry::uid`] where it exists, and on the index as
/// a fallback for an entry that has never been stamped (uid 0); the index is
/// included regardless so even two unstamped entries get distinct ids.
pub(super) fn body_field_id(app: &GuiApp) -> egui::Id {
    let (uid, idx) = app
        .session
        .collections
        .get(app.active_ci())
        .map(|c| {
            (
                c.entries.get(c.selected_entry).map(|e| e.uid).unwrap_or(0),
                c.selected_entry,
            )
        })
        .unwrap_or((0, 0));
    egui::Id::new(("resp_body", uid, idx))
}

/// Whatever is selected in the response body field, when it is a single line
/// of it. Used as the literal for `body contains …`, and as the dialog's
/// opening filter.
fn body_selection(app: &GuiApp, ctx: &egui::Context) -> Option<String> {
    let state = egui::TextEdit::load_state(ctx, body_field_id(app))?;
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
        .map(|r| r.body.clone())?;
    // The field is handed the *compacted* text when Compact is on, so the char
    // range indexes that, not the raw body. Slicing the raw body with those
    // offsets diverges from the first elided literal onwards and quotes the
    // wrong text. Translate the range back through the compaction map so the
    // literal is the untruncated value that actually occurs in the reply — the
    // same expansion the terminal UI does on copy (`resp_full_selected_parts`).
    let text = if app.response_compact {
        selection_from_compacted(&body, range.start.0, range.end.0)?
    } else {
        body.chars()
            .skip(range.start.0)
            .take(range.end.0 - range.start.0)
            .collect()
    };
    let text = text.trim().to_string();
    (!text.is_empty() && !text.contains('\n')).then_some(text)
}

/// Translate a char range taken from the *compacted* body view back to the
/// slice of the raw body it stands for. A position inside a shortened literal
/// maps to the start of the elided run, so a selection that spans a whole
/// compacted literal expands to that literal's full, untruncated text.
fn selection_from_compacted(raw: &str, start: usize, end: usize) -> Option<String> {
    let (compacted, maps) = crate::shared_utils::compact_long_strings_mapped(raw);
    let full_start = compacted_to_raw_offset(&compacted, &maps, raw, start);
    let full_end = compacted_to_raw_offset(&compacted, &maps, raw, end);
    if full_start >= full_end {
        return None;
    }
    Some(
        raw.chars()
            .skip(full_start)
            .take(full_end - full_start)
            .collect(),
    )
}

/// A flat char offset into the compacted text, translated to a flat char offset
/// into the raw body. Compaction never adds or removes newlines, so the line
/// index is shared and only the column needs mapping.
fn compacted_to_raw_offset(compacted: &str, maps: &[Vec<usize>], raw: &str, pos: usize) -> usize {
    let (line, col) = flat_to_line_col(compacted, pos);
    let full_col = match maps.get(line) {
        Some(map) if !map.is_empty() => map[col.min(map.len() - 1)],
        _ => col,
    };
    line_col_to_flat(raw, line, full_col)
}

/// A flat char offset into `text` as a `(line, column)` pair.
fn flat_to_line_col(text: &str, offset: usize) -> (usize, usize) {
    let mut line = 0;
    let mut col = 0;
    for (count, ch) in text.chars().enumerate() {
        if count == offset {
            return (line, col);
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// A `(line, column)` pair in `text` as a flat char offset.
fn line_col_to_flat(text: &str, target_line: usize, target_col: usize) -> usize {
    let mut offset = 0;
    for (i, line) in text.split('\n').enumerate() {
        if i == target_line {
            return offset + target_col;
        }
        offset += line.chars().count() + 1;
    }
    offset
}

/// The raw body of the response the builder was opened on.
fn builder_body(app: &GuiApp) -> Option<std::sync::Arc<str>> {
    app.session
        .collections
        .get(app.active_ci())
        .and_then(|c| c.entries.get(c.selected_entry))
        .and_then(|e| e.last_response.as_ref())
        .map(|r| r.body.clone())
}

/// Show, in the response body itself, which value the builder is talking about.
///
/// A dialog naming `$.data[0].token` beside a body holding six plausible tokens
/// is a puzzle the user has to solve by reading. Highlighting is done by
/// setting the body field's own selection rather than painting over it, so the
/// highlight is a real selection: Ctrl+C copies exactly the value being
/// asserted on, which is the thing anyone looking at it wants next.
///
/// A no-op for subjects that are not written in the body (the status, a
/// header, the body as a whole) and while the value is off in a part of the
/// text the compact view rewrites beyond recognition.
pub(super) fn highlight(app: &GuiApp, ctx: &egui::Context, subject: &Subject) {
    let Some(body) = builder_body(app) else {
        return;
    };
    let Some(range) = value_char_range(&body, subject, app.response_compact) else {
        return;
    };
    let id = body_field_id(app);
    let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
        return;
    };
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(range.0),
            egui::text::CCursor::new(range.1),
        )));
    state.store(ctx, id);
}

/// The char range of `subject`'s value *as the body field currently shows it* —
/// which is the compacted text when Compact is on, so the range has to be
/// mapped through the same compaction map the selection reader uses in reverse.
fn value_char_range(body: &str, subject: &Subject, compact: bool) -> Option<(usize, usize)> {
    char_range_of(body, probe::span_of(body, subject)?, compact)
}

/// The same for the field's *name*, where it has one.
fn key_char_range(body: &str, subject: &Subject, compact: bool) -> Option<(usize, usize)> {
    char_range_of(body, probe::key_span_of(body, subject)?, compact)
}

fn char_range_of(
    body: &str,
    span: std::ops::Range<usize>,
    compact: bool,
) -> Option<(usize, usize)> {
    let start = body[..span.start].chars().count();
    let end = start + body[span.clone()].chars().count();
    if !compact {
        return Some((start, end));
    }
    let (compacted, maps) = crate::shared_utils::compact_long_strings_mapped(body);
    Some((
        raw_to_compacted_offset(&compacted, &maps, body, start),
        raw_to_compacted_offset(&compacted, &maps, body, end),
    ))
}

/// The inverse of [`compacted_to_raw_offset`]: a flat char offset into the raw
/// body, as one into the compacted view. A raw position inside an elided run
/// maps to the compacted position that run was shortened to, so a highlight
/// over a shortened literal covers the whole of what is shown of it.
fn raw_to_compacted_offset(compacted: &str, maps: &[Vec<usize>], raw: &str, pos: usize) -> usize {
    let (line, col) = flat_to_line_col(raw, pos);
    let compact_col = match maps.get(line) {
        Some(map) if !map.is_empty() => {
            // The map is ascending (compacted column -> raw column), so the
            // last entry at or before this raw column is where it now sits.
            map.iter()
                .rposition(|raw_col| *raw_col <= col)
                .unwrap_or(map.len() - 1)
        }
        _ => col,
    };
    line_col_to_flat(compacted, line, compact_col)
}

/// The exact text of the value a probe is about, for copying: the raw bytes as
/// the server wrote them for anything in the body, and the observed value
/// otherwise.
///
/// The raw slice rather than a re-serialised value because "copy this section"
/// means the section on screen — the same quoting, the same number spelling,
/// the same key order.
pub(super) fn value_text(app: &GuiApp, probe: &Probe) -> Option<String> {
    match builder_body(app) {
        Some(body) => raw_value_text(&body, probe),
        None => plain_value_text(probe),
    }
}

/// [`value_text`] against a body already in hand — the response panel's own
/// closures hold it, and re-reading it through the app would need a second
/// borrow of something they have already borrowed.
pub(super) fn raw_value_text(body: &str, probe: &Probe) -> Option<String> {
    if let Some(span) = probe::span_of(body, &probe.subject) {
        return Some(body[span].to_string());
    }
    if matches!(probe.subject, Subject::Body) {
        return Some(body.to_string());
    }
    plain_value_text(probe)
}

/// A subject with nothing in the body to point at -- a header, the status --
/// copies the value that was observed. A string copies unquoted: what is wanted
/// on the clipboard is the token, not a JSON literal of it.
fn plain_value_text(probe: &Probe) -> Option<String> {
    match probe.value.as_ref()? {
        serde_json::Value::String(s) => Some(s.clone()),
        v => Some(v.to_string()),
    }
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
pub(super) fn pointed_in(
    ctx: &egui::Context,
    body: &str,
    compact: bool,
    id: egui::Id,
) -> Option<Probe> {
    if compact {
        return None;
    }
    let state = egui::TextEdit::load_state(ctx, id)?;
    let range = state.cursor.char_range()?.as_sorted_char_range();
    let offset = body
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(body.len()))
        .nth(range.start.0)?;
    probe::probe_at(body, offset)
}

/// The value the *mouse* is over in the laid-out body, as a subject.
///
/// The caret answers "what did I click on"; this answers "what am I about to
/// click on", which is what a highlight has to follow to be worth anything.
/// Same refusal in the compact view as [`pointed_in`], and for the same
/// reason: the shown text is not the body, so an offset into it names the
/// wrong field.
pub(super) fn hovered_in(
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    pointer: egui::Pos2,
    body: &str,
    compact: bool,
) -> Option<Probe> {
    if compact {
        return None;
    }
    let cursor = galley.cursor_from_pos(pointer - galley_pos);
    let offset = char_to_byte(body, cursor.index.0)?;
    probe::probe_at(body, offset)
}

/// The byte offset of char number `n`, or the end of the string for the one
/// position past the last char.
fn char_to_byte(body: &str, n: usize) -> Option<usize> {
    body.char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(body.len()))
        .nth(n)
}

/// Wash over `subject`'s value where the body field has laid it out.
///
/// Painted rather than selected: [`highlight`] sets the field's real selection
/// so the value can be copied, which is right for the value the *dialog* is
/// discussing but wrong for one the mouse is merely passing over -- stealing
/// the selection on hover would destroy a selection the user made by hand, and
/// do it on every mouse move.
///
/// Returns the rows it painted so a caller can tell whether anything was shown.
pub(super) fn paint_span(
    painter: &egui::Painter,
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    body: &str,
    subject: &Subject,
    compact: bool,
    colour: egui::Color32,
) -> usize {
    // The field's name, washed as strongly as its value. A row lit only from
    // the colon rightwards reads as a highlight that stopped short of what the
    // pointer is on, and a fainter wash on the name was hard enough to see
    // that it gave neither the whole field nor a clean value.
    //
    // Nor is the subject only ever the value: `exists`, `isEmpty` and `count`
    // are statements about the *field*, and half of them are still true of a
    // field whose value is missing. The pair is what is being pointed at, so
    // the pair is what is lit -- the same promise the headers tab makes with
    // its whole row. Copying still takes the value alone.
    if let Some((start, end)) = key_char_range(body, subject, compact)
        && end > start
    {
        paint_char_range(painter, galley, galley_pos, start, end, colour);
    }
    let Some((start, end)) = value_char_range(body, subject, compact) else {
        return 0;
    };
    if end <= start {
        return 0;
    }
    paint_char_range(painter, galley, galley_pos, start, end, colour)
}

/// Fill the rows a char range occupies in a laid-out galley.
fn paint_char_range(
    painter: &egui::Painter,
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    start: usize,
    end: usize,
    colour: egui::Color32,
) -> usize {
    let a = galley.layout_from_cursor(egui::text::CCursor::new(start));
    let b = galley.layout_from_cursor(egui::text::CCursor::new(end));
    let mut painted = 0;
    for r in a.row..=b.row {
        let Some(row) = galley.rows.get(r) else { break };
        // A value can span rows (an object, an array, a wrapped string), so
        // each row is filled from where the span enters it to where it leaves:
        // the row's own edges in between.
        let left = if r == a.row {
            galley.pos_from_layout_cursor(&a).left()
        } else {
            row.rect().left()
        };
        let right = if r == b.row {
            galley.pos_from_layout_cursor(&b).left()
        } else {
            row.rect().right()
        };
        if right <= left {
            continue;
        }
        let rect = egui::Rect::from_min_max(
            egui::pos2(left, row.rect().top()),
            egui::pos2(right, row.rect().bottom()),
        )
        .translate(galley_pos.to_vec2());
        painter.rect_filled(rect, 2.0, colour);
        painted += 1;
    }
    painted
}

/// Write the chosen verb onto the request the response came from.
///
/// The collection *and* the entry are re-resolved rather than trusted: the
/// user can switch request, reorder tabs, reload the file or close the
/// collection while the dialog is up, and writing an assert into whatever now
/// sits at that position would be silent corruption. The collection is matched
/// by runtime id and the entry by its stable [`HurlEntry::uid`] (falling back
/// to the index only for an entry that was never stamped), so a reload or
/// reorder that renumbers the requests can't redirect the write.
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
    let Some(target) = find_entry_mut(col, builder.entry_uid, builder.entry) else {
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
        target.mark_edited();
        col.invalidate_request_json();
        app.session.save();
    }
    changed
}

/// Resolve the entry the builder targets by its stable [`HurlEntry::uid`],
/// falling back to the index only for an unstamped entry (uid 0). A uid that no
/// longer resolves to exactly one entry (deleted, or duplicated by a clone)
/// declines rather than guess.
fn find_entry_mut(
    col: &mut crate::collection::Collection,
    uid: u64,
    idx: usize,
) -> Option<&mut crate::hurl::HurlEntry> {
    if uid != 0 {
        return match col.entries.iter().filter(|e| e.uid == uid).count() {
            1 => col.entries.iter_mut().find(|e| e.uid == uid),
            _ => None,
        };
    }
    col.entries.get_mut(idx)
}

/// The immutable twin of [`find_entry_mut`], for read-only lookups.
fn find_entry<'a>(
    col: &'a crate::collection::Collection,
    uid: u64,
    idx: usize,
) -> Option<&'a crate::hurl::HurlEntry> {
    if uid != 0 {
        return match col.entries.iter().filter(|e| e.uid == uid).count() {
            1 => col.entries.iter().find(|e| e.uid == uid),
            _ => None,
        };
    }
    col.entries.get(idx)
}

/// A name for a capture of this subject that isn't already taken on the entry.
pub(super) fn suggested_name(app: &GuiApp, builder: &ProbeBuilder, subject: &Subject) -> String {
    let taken: Vec<String> = app
        .session
        .collections
        .iter()
        .find(|c| c.id == builder.collection_id)
        .and_then(|c| find_entry(c, builder.entry_uid, builder.entry))
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
            entry_uid: app.session.collections[0].entries[0].uid,
            subjects: probe::probes(201, Some(90), &[], body),
            filter: String::new(),
            chosen: None,
            verbs: Vec::new(),
            capture_name: None,
            name_required: false,
            selection: None,
            selected: 0,
            selected_verb: 0,
            highlighted: None,
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

/// Tests that drive the real response panel with simulated pointer events and
/// read back what was painted — the only way to exercise the caret-resolution
/// and drag-selection this module does, which live entirely in state egui
/// stores as it lays the panel out. Harness in [`crate::gui::probe_test_support`].
#[cfg(test)]
mod paint_tests {
    use super::*;
    use crate::gui::app::Dialog;
    use crate::gui::probe_test_support::*;
    use crate::hurl::HurlEntry;
    use eframe::egui;

    /// The question the whole right-click route rests on: `pointed_in` treats
    /// `CCursor.index` as a *character* index and walks `char_indices()` to
    /// turn it into a byte offset for `probe_at`. If egui were handing back
    /// byte offsets that walk would over-shoot on any multi-byte body. Clicks —
    /// with real pointer events, through the real panel — inside `"abcdef"` on
    /// a body whose earlier fields are full of accents, CJK and an emoji.
    #[test]
    fn the_caret_is_a_char_index_and_the_conversion_is_load_bearing() {
        let body = "{\n  \"note\": \"héllo wörld 🚀 中文中文\",\n  \"token\": \"abcdef\"\n}";
        let target_at = body.find("abcdef").unwrap();
        let chars_before = body[..target_at].chars().count();
        assert!(
            target_at > chars_before,
            "the fixture must have multi-byte text before the target ({target_at} bytes, {chars_before} chars)"
        );

        let mut app = app_with(body, vec![], 200);
        let ctx = themed_ctx();
        panel_frame(&mut app, &ctx, vec![]);
        let painted = panel_frame(&mut app, &ctx, vec![]);
        let (pos, galley) = painted
            .iter()
            .find(|(_, g)| g.text().contains("abcdef"))
            .expect("the body was never painted");
        let aim = body[..target_at + 2].chars().count();
        let target = *pos
            + galley
                .pos_from_cursor(egui::text::CCursor::new(aim))
                .center()
                .to_vec2();

        let (press, release) = click_events(target, egui::PointerButton::Secondary);
        panel_frame(&mut app, &ctx, press);
        panel_frame(&mut app, &ctx, release);

        let id = body_field_id(&app);
        let range = egui::TextEdit::load_state(&ctx, id)
            .and_then(|s| s.cursor.char_range())
            .expect("the click left no caret");
        let index = range.as_sorted_char_range().start.0;
        assert_eq!(
            index, aim,
            "egui handed back a byte offset, not a character index"
        );

        let resolved = pointed_in(&ctx, body, false, id);
        let as_bytes = crate::probe::probe_at(body, index);
        assert_ne!(
            format!("{resolved:?}"),
            format!("{as_bytes:?}"),
            "the two readings agree on this fixture, so it proves nothing"
        );
        let subject = resolved.expect("nothing under the caret").subject;
        assert_eq!(
            crate::probe::subject_label(&subject),
            "$.token",
            "clicked inside \"abcdef\" but the builder targeted a different field"
        );
    }

    /// `pointed_in` refuses to resolve offsets while Compact is on, because
    /// compaction rewrites the text. `body_selection` — which supplies the
    /// literal for `body contains "…"` — must not slice the *raw* body with
    /// offsets taken from the *compacted* one; it translates them back instead.
    #[test]
    fn a_selection_made_in_the_compacted_view_quotes_the_wrong_text() {
        let raw = format!(r#"{{"a":"{}","tail":"WANTED"}}"#, "x".repeat(80));
        let mut app = app_with(&raw, vec![], 200);
        app.response_compact = true;
        let ctx = themed_ctx();
        panel_frame(&mut app, &ctx, vec![]);
        let painted = panel_frame(&mut app, &ctx, vec![]);
        let shown = crate::shared_utils::compact_long_strings(&raw);
        let (pos, galley) = painted
            .iter()
            .find(|(_, g)| g.text() == shown)
            .unwrap_or_else(|| {
                panic!(
                    "the compacted body was never painted: {:?}",
                    texts(&painted)
                )
            });

        let at = shown.find("WANTED").unwrap();
        let start = shown[..at].chars().count();
        let p = |i: usize| {
            *pos + galley
                .pos_from_cursor(egui::text::CCursor::new(i))
                .center()
                .to_vec2()
        };
        let (from, to) = (p(start), p(start + "WANTED".len()));
        let down = egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        };
        let up = egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        };
        panel_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(from), down]);
        panel_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(to)]);
        panel_frame(&mut app, &ctx, vec![up]);

        let selected: String = {
            let range = egui::TextEdit::load_state(&ctx, body_field_id(&app))
                .and_then(|s| s.cursor.char_range())
                .expect("no selection")
                .as_sorted_char_range();
            shown
                .chars()
                .skip(range.start.0)
                .take(range.end.0 - range.start.0)
                .collect()
        };

        open(&mut app, &ctx, None);
        let Some(Dialog::ProbeBuilder(b)) = &app.dialog else {
            panic!("the builder did not open");
        };
        assert_eq!(
            b.selection.as_deref(),
            Some(selected.trim()),
            "the `body contains` literal is not the text that was selected"
        );
    }

    /// The response body field is drawn under a per-request id, so egui's
    /// cursor state does not survive a change of request: a selection made in
    /// one reply must not be applied, unchecked, to the *next* reply's body.
    #[test]
    fn a_selection_survives_into_the_next_request_and_quotes_its_body_instead() {
        let first = r#"{"one":"AAAAAAAAAA","two":"SELECTME"}"#;
        let second = r#"{"one":"BBBBBBBBBB","two":"different"}"#;
        let mut app = app_with(first, vec![], 200);
        let mut other = HurlEntry {
            title: "Other".to_string(),
            ..Default::default()
        };
        other.last_response = Some(crate::http::ApiResponse {
            status: 200,
            body: std::sync::Arc::from(second),
            ..Default::default()
        });
        app.session.collections[0].entries.push(other);
        let ctx = themed_ctx();
        panel_frame(&mut app, &ctx, vec![]);
        let painted = panel_frame(&mut app, &ctx, vec![]);
        let (pos, galley) = painted
            .iter()
            .find(|(_, g)| g.text() == first)
            .expect("body not painted");
        let at = first.find("SELECTME").unwrap();
        let p = |i: usize| {
            *pos + galley
                .pos_from_cursor(egui::text::CCursor::new(i))
                .center()
                .to_vec2()
        };
        let (from, to) = (p(at), p(at + "SELECTME".len()));
        let down = egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        };
        let up = egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        };
        panel_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(from), down]);
        panel_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(to)]);
        panel_frame(&mut app, &ctx, vec![up]);

        app.session.collections[0].selected_entry = 1;
        panel_frame(&mut app, &ctx, vec![]);
        open(&mut app, &ctx, None);
        let Some(Dialog::ProbeBuilder(b)) = &app.dialog else {
            panic!("the builder did not open");
        };
        assert_eq!(
            b.selection, None,
            "a selection made in another request's body was offered as this one's literal"
        );
    }

    /// The collection is pinned by runtime id because the dialog outlives the
    /// frame that opened it — and the entry within it is pinned by its stable
    /// `uid`, so a reorder (or a reload of the file in a different order) while
    /// the builder is up cannot send the assert to a different request.
    #[test]
    fn reordering_requests_while_the_builder_is_open_writes_the_assert_elsewhere() {
        let body = r#"{"token":"abc"}"#;
        let mut app = app_with(body, vec![], 200);
        let first = HurlEntry {
            title: "Ping".to_string(),
            ..Default::default()
        };
        app.session.collections[0].entries.insert(0, first);
        app.session.collections[0].selected_entry = 1;
        let ctx = themed_ctx();
        let target = crate::probe::probes(200, None, &[], body)
            .into_iter()
            .find(|p| crate::probe::subject_label(&p.subject) == "$.token")
            .unwrap();
        open(&mut app, &ctx, Some(target));
        let Some(Dialog::ProbeBuilder(b)) = app.dialog.take() else {
            panic!("no builder");
        };
        app.session.collections[0].entries.swap(0, 1);
        let verb = b.verbs[0].clone();
        apply(&mut app, &b, &verb, "");
        let ping = &app.session.collections[0].entries[1];
        assert!(
            ping.asserts.is_empty(),
            "the assert landed on {:?}, which never made that request",
            ping.title
        );
    }
}
