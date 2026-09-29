//! The JUnit XML export: a report as a test suite, for CI.
//!
//! Every CI system on the planet — GitHub Actions, GitLab, Jenkins, Buildkite,
//! Azure — knows how to read JUnit XML and will draw the failures in its own
//! UI, annotate the pull request, and keep the history of which case has been
//! flaky. None of them will do any of that for a CSV. So this is not a sixth
//! way to render the same table: it is what makes a post-deploy PaperTrail run
//! a *test result* rather than an artefact someone has to open.
//!
//! The mapping follows JUnit's own distinction rather than inventing one, since
//! every consumer already shades the two differently:
//!
//! - **`<error>`** — the request itself went wrong (refused connection, an
//!   unexpected status, a failed `[Asserts]` entry). In JUnit's vocabulary that
//!   is an exception: the test could not deliver a verdict.
//! - **`<failure>`** — the request answered, and the answer was wrong against
//!   the report's ground truth (`TRUTH "…"`, the `Correct` column). That is an
//!   assertion that did not hold, which is what a failure is.
//! - **`<skipped>`** — a dry run. Nothing was sent, so every case is skipped
//!   rather than passed; a preview that reported a green suite would be the
//!   same lie the report file used to tell (see [`super::model::ReportResult`]).
//!
//! A case gets **one** of those, in that order of seniority: a row whose
//! request failed is an `<error>` even if its ground truth also came out
//! wrong, because the wrong answer is a consequence of the failure and the
//! stricter JUnit schemas allow a case only one outcome. The demoted verdict
//! is not lost — it is written into the case's `<system-out>`.
//!
//! A row with neither is a pass. Note what that means for a report with no
//! `TRUTH` column and a service that answers: every case passes, which is
//! exactly right — that report is a smoke test, and the requests were fine.

use super::compare::CORRECT_COLUMN;
use super::flow::Header;
use super::model::{OutputColumn, ReportResult, ReportRow, Verdict};
use super::writer::ReportWriter;
use std::collections::HashMap;

/// How much of a cell's text goes into `<system-out>`. A report can carry a
/// whole response body per row; a CI viewer that has to page through a megabyte
/// of JSON to find the one line that matters is no better than no context at
/// all, and the full value is in the `-o` report next to it.
const MAX_CELL: usize = 1000;

/// Writes a report as JUnit XML: one `<testsuite>`, one `<testcase>` per row.
pub struct JunitWriter;

impl ReportWriter for JunitWriter {
    fn write(&self, result: &ReportResult, header: &Header) -> Result<Vec<u8>, String> {
        let columns = result.resolved_columns(header);
        let suite = suite_name(header);
        let mut cases: Vec<Case> = result
            .rows
            .iter()
            .enumerate()
            .map(|(r, row)| Case::of(r, row, result, &columns))
            .collect();
        disambiguate(&mut cases);

        // Run errors that no row is responsible for: an empty glob, a producer
        // that failed before any row existed, a `CLEANUP` that broke after the
        // last row was emitted. Without this they would vanish — and a report
        // that produced no rows *because* something was wrong would be a green
        // suite of zero tests, which is the most dangerous shape a CI result
        // can take.
        //
        // Ownership is structural (`ReportRow::errors`), never a comparison of
        // the formatted text: the row's `…Error` cell and the run's error list
        // are worded differently, so matching on the sentence reported every
        // failed row twice — once as itself and once as an unexplained run
        // error.
        let unexplained: Vec<&String> = result
            .errors
            .iter()
            .filter(|e| !result.rows.iter().any(|row| row.errors.contains(e)))
            .collect();

        // What a reader needs to know about the run that isn't about any one
        // case: the caveats the other formats carry as banners, plus the
        // teardown warnings and the steps that never ran.
        let mut notes: Vec<String> = Vec::new();
        if result.dry_run {
            notes.push("DRY RUN: no requests were sent — these cases are projected".to_string());
        }
        if let Some(p) = &result.partial {
            notes.push(format!(
                "PARTIAL: the run was stopped — {} of {} rows ran",
                p.rows_completed, p.rows_planned
            ));
        }
        notes.extend(result.skipped.iter().map(|s| format!("skipped: {s}")));
        notes.extend(result.warnings.iter().map(|w| format!("warning: {w}")));

        // The run-level case exists when there is something to say that belongs
        // to no row. It carries the notes as well as the orphan errors because
        // a suite-level `<system-err>` is legal but *invisible* in GitLab,
        // which reads those elements only under a `<testcase>` — and "the run
        // was stopped half way" is not a caveat a CI reader can afford to miss.
        let run_case = (!unexplained.is_empty() || !notes.is_empty()).then(|| Case {
            name: suite.clone(),
            errors: unexplained.iter().map(|e| (*e).clone()).collect(),
            failures: Vec::new(),
            context: notes.iter().map(|n| (String::new(), n.clone())).collect(),
            seconds: 0.0,
        });

        // Counted off the status each case is actually *emitted* with, so the
        // totals can never describe a document other than this one: a dry run
        // writes `<skipped>` and nothing else, and a case demoted from failure
        // to error is counted once, as an error.
        // The run case is counted with `dry_run = false` because it is emitted
        // that way: it is *about* the run rather than a projected case, so a
        // dry run must not count it among the skipped.
        let all = cases
            .iter()
            .map(|c| (c, result.dry_run))
            .chain(run_case.iter().map(|c| (c, false)));
        let tests = cases.len() + usize::from(run_case.is_some());
        let (mut failures, mut errors, mut skipped) = (0, 0, 0);
        for (c, dry) in all {
            match c.status(dry) {
                Status::Skipped => skipped += 1,
                Status::Error => errors += 1,
                Status::Failure => failures += 1,
                Status::Passed => {}
            }
        }
        let time = positive(cases.iter().map(|c| c.seconds).sum());

        let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        out.push_str(&format!(
            "<testsuites name=\"{}\" tests=\"{tests}\" failures=\"{failures}\" \
             errors=\"{errors}\" skipped=\"{skipped}\" time=\"{time:.3}\">\n",
            esc(&suite)
        ));
        out.push_str(&format!(
            "  <testsuite name=\"{}\" tests=\"{tests}\" failures=\"{failures}\" \
             errors=\"{errors}\" skipped=\"{skipped}\" time=\"{time:.3}\">\n",
            esc(&suite)
        ));

        for case in &cases {
            push_case(&mut out, case, &suite, result.dry_run);
        }
        if let Some(case) = &run_case {
            // Never skipped, whatever the run was: this case is *about* the
            // run, and a dry run's own note is the thing it exists to carry.
            push_case(&mut out, case, &suite, false);
        }

        // Kept as well as on the run case, for the consumers that do read it.
        if !notes.is_empty() {
            out.push_str(&format!(
                "    <system-err>{}</system-err>\n",
                esc(&notes.join("\n"))
            ));
        }

        out.push_str("  </testsuite>\n</testsuites>\n");
        Ok(out.into_bytes())
    }
}

/// What a case is reported as. One per case: the stricter JUnit schemas
/// (Ant/Windy Road) allow a `<testcase>` only one of these, and a case counted
/// in two totals makes every consumer's arithmetic disagree with the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Skipped,
    Error,
    Failure,
    Passed,
}

/// One row, reduced to what JUnit needs to say about it.
struct Case {
    name: String,
    /// The run errors this row is responsible for, taken from
    /// [`ReportRow::errors`] — structural ownership recorded where the failure
    /// happened, rather than re-derived here from an `…Error` cell that a
    /// declared field or a `HIDE(Error)` may have removed.
    errors: Vec<String>,
    /// Ground-truth verdicts that came out wrong, as `column: expected … got …`.
    failures: Vec<String>,
    /// Name/value pairs for `<system-out>`; an empty name writes the value
    /// alone (the run case's notes, which are sentences rather than cells).
    context: Vec<(String, String)>,
    /// Wall time for the row, in seconds.
    seconds: f64,
}

impl Case {
    fn of(r: usize, row: &ReportRow, result: &ReportResult, columns: &[OutputColumn]) -> Self {
        let errors = row.errors.clone();

        let mut failures = Vec::new();
        for col in columns.iter().filter(|c| c.truth.is_some()) {
            if result.verdicts.get(&(r, col.header.clone())) == Some(&Verdict::Incorrect) {
                let expected = result
                    .truths
                    .get(&(r, col.header.clone()))
                    .cloned()
                    .unwrap_or_default();
                let got = col.value(row, &result.no_match_marker);
                failures.push(format!(
                    "{}: expected {expected:?}, got {got:?}",
                    col.header
                ));
            }
        }
        // The row can also be rolled up as wrong without this writer being able
        // to name the column that did it — a snapshot row, or a result built by
        // something other than a live run. A vague failure beats a lost one.
        if failures.is_empty()
            && row.cells.get(CORRECT_COLUMN).map(String::as_str)
                == Some(Verdict::Incorrect.as_str())
        {
            failures.push("the row's answer did not match its ground truth".to_string());
        }

        // Only the *totals*: `…Time` under its own name, plus whatever a field
        // aliased the `Time` intrinsic to (recorded per run, since the alias is
        // the user's name and says nothing about what it holds). Adding the
        // setup/wait/download slices as well would count the same milliseconds
        // two or three times over.
        let mut seen: Vec<&String> = Vec::new();
        let mut ms = 0.0;
        for (k, v) in &row.cells {
            let is_total =
                k.as_str() == "Time" || k.ends_with(".Time") || result.duration_columns.contains(k);
            if !is_total || seen.contains(&k) {
                continue;
            }
            seen.push(k);
            // A duration that is negative, infinite or NaN is not a duration;
            // JUnit's `time` is a plain non-negative number and some consumers
            // reject the document outright over one.
            if let Ok(n) = v.trim().parse::<f64>()
                && n.is_finite()
                && n >= 0.0
            {
                ms += n;
            }
        }

        let context = columns
            .iter()
            .map(|c| {
                (
                    c.header.clone(),
                    truncate(&c.value(row, &result.no_match_marker)),
                )
            })
            .filter(|(_, v)| !v.is_empty())
            .collect();

        Self {
            name: case_name(r, row),
            errors,
            failures,
            context,
            seconds: positive(ms / 1000.0),
        }
    }

    /// The one status this case is reported with. A failed request outranks a
    /// wrong answer: the answer was wrong *because* the request was, and a case
    /// that claimed both would be counted twice and rejected by the stricter
    /// schemas.
    fn status(&self, dry_run: bool) -> Status {
        if dry_run {
            Status::Skipped
        } else if !self.errors.is_empty() {
            Status::Error
        } else if !self.failures.is_empty() {
            Status::Failure
        } else {
            Status::Passed
        }
    }
}

/// Make every case name unique within the suite.
///
/// A row *key* can legitimately repeat — two manifest lines with the same
/// values, the same key under two comparison clauses — while `row.path` is
/// guaranteed unique. CI consumers key a case on `(classname, name)` and
/// GitLab, for one, keeps only the first of a duplicated pair: a colliding name
/// does not merely read badly, it silently *hides* the second row's failure.
/// The path is appended only where there is a collision, so ordinary reports
/// keep the readable names that make a history worth having.
fn disambiguate(cases: &mut [Case]) {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for c in cases.iter() {
        *counts.entry(c.name.as_str()).or_default() += 1;
    }
    let dupes: Vec<String> = counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(k, _)| k.to_string())
        .collect();
    if dupes.is_empty() {
        return;
    }
    let mut nth: HashMap<String, usize> = HashMap::new();
    for c in cases.iter_mut() {
        if !dupes.contains(&c.name) {
            continue;
        }
        let n = nth.entry(c.name.clone()).or_default();
        *n += 1;
        c.name = format!("{} #{}", c.name, *n);
    }
}

/// Turn a negative zero into a plain one.
///
/// Rust sums `f64` from an identity of `-0.0` (so that a sum of negative zeros
/// keeps its sign), which means a row carrying no `…Time` cell at all comes out
/// as `-0.0` and formats as `time="-0.000"`. Some JUnit parsers refuse a
/// negative duration outright, and every human who sees one stops to work out
/// what went backwards.
fn positive(v: f64) -> f64 {
    v + 0.0
}

/// A row's name in the suite: its **key** — the loop values that identify it
/// (`batch-07 / case-a`) — because that is the identity CI has to keep stable
/// between runs to track a case's history. The row's index would rename every
/// case the moment a manifest gained a line, which is exactly when a reader
/// most wants the history.
fn case_name(r: usize, row: &ReportRow) -> String {
    let mut name = row.key.join(" / ");
    if name.trim().is_empty() {
        name = format!("row {}", r + 1);
    }
    // An `ENVS` comparison runs the same key against several targets; without
    // the target they would be one case reported twice.
    match &row.target {
        Some(t) if !t.is_empty() => format!("{name} [{t}]"),
        _ => name,
    }
}

/// The suite's name: the report's `# name:`, else a constant — never the file
/// name, which is the one thing a CI job can already see.
fn suite_name(header: &Header) -> String {
    match header.get("name").map(str::trim) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => "PaperTrail report".to_string(),
    }
}

fn push_case(out: &mut String, case: &Case, suite: &str, dry_run: bool) {
    out.push_str(&format!(
        "    <testcase name=\"{}\" classname=\"{}\" time=\"{:.3}\"",
        esc(&case.name),
        esc(suite),
        case.seconds
    ));
    let status = case.status(dry_run);
    // `<system-out>` carries the row's cells, and (for a case demoted from
    // failure to error) the verdict that lost the tie — so nothing a reader
    // needs is dropped by the one-status rule.
    let mut out_lines: Vec<String> = Vec::new();
    if status == Status::Error {
        out_lines.extend(case.failures.iter().cloned());
    }
    out_lines.extend(case.context.iter().map(|(k, v)| {
        if k.is_empty() {
            v.clone()
        } else {
            format!("{k}: {v}")
        }
    }));
    if status == Status::Passed && out_lines.is_empty() {
        out.push_str("/>\n");
        return;
    }
    out.push_str(">\n");
    // Element order follows the JUnit XSD's content model —
    // `skipped?, error*, failure*, system-out*, system-err*` — which the
    // stricter validators enforce.
    match status {
        Status::Skipped => {
            out.push_str("      <skipped message=\"dry run — no request was sent\"/>\n");
        }
        Status::Error => {
            for e in &case.errors {
                out.push_str(&format!(
                    "      <error message=\"{}\" type=\"request\">{}</error>\n",
                    esc(&first_line(e)),
                    esc(e)
                ));
            }
        }
        Status::Failure => {
            for f in &case.failures {
                out.push_str(&format!(
                    "      <failure message=\"{}\" type=\"truth\">{}</failure>\n",
                    esc(&first_line(f)),
                    esc(f)
                ));
            }
        }
        Status::Passed => {}
    }
    if !out_lines.is_empty() {
        out.push_str(&format!(
            "      <system-out>{}</system-out>\n",
            esc(&out_lines.join("\n"))
        ));
    }
    out.push_str("    </testcase>\n");
}

/// The first line of `s`, for an attribute: a `message` is rendered inline by
/// every consumer, and a response body pasted into one makes the suite
/// unreadable in all of them.
fn first_line(s: &str) -> String {
    truncate(s.lines().next().unwrap_or("").trim())
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= MAX_CELL {
        return s.to_string();
    }
    let kept: String = s.chars().take(MAX_CELL).collect();
    format!("{kept}…")
}

/// XML-escape `s`, and replace every character XML 1.0 cannot represent.
///
/// A raw response body reaches this — a `\x00` from a binary payload, an
/// `0x1b` from an ANSI-coloured error, a `U+FFFF` from a fuzzer — and a single
/// one of them makes the whole file unparseable, which would take every *other*
/// failure in the suite down with it. The legal set is XML 1.0's `Char`
/// production: tab/LF/CR, `U+0020..=U+D7FF`, `U+E000..=U+FFFD`, and
/// `U+10000..=U+10FFFF`. (Surrogates cannot occur in a Rust `str`, so the gap
/// between D7FF and E000 is unreachable — it is written out anyway because the
/// rule, not the reachability, is what the next reader needs to see.)
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if is_xml_char(c) => out.push(c),
            // Replaced rather than dropped: a body that is mostly control
            // bytes should still look like something went wrong with it.
            _ => out.push(' '),
        }
    }
    out
}

/// Whether `c` is legal in an XML 1.0 document at all (excluding the
/// tab/LF/CR handled by the caller).
fn is_xml_char(c: char) -> bool {
    let u = c as u32;
    (0x20..=0xD7FF).contains(&u)
        || (0xE000..=0xFFFD).contains(&u)
        || (0x10000..=0x10FFFF).contains(&u)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::model::{ReportResult, ReportRow};
    use std::collections::HashMap;

    fn row(key: &[&str], cells: &[(&str, &str)]) -> ReportRow {
        ReportRow {
            errors: Vec::new(),
            role: crate::report::model::RowRole::default(),
            cells: cells
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            vars: HashMap::new(),
            key: key.iter().map(|k| k.to_string()).collect(),
            path: Vec::new(),
            comparison: None,
            target: None,
        }
    }

    /// A row whose request failed. The errors are the run's own wording (what
    /// `ReportResult::errors` holds); the `…Error` cell is the report's, and
    /// the two are deliberately different — which is why ownership is carried
    /// structurally rather than matched on the text.
    fn failed_row(key: &[&str], cells: &[(&str, &str)], errors: &[&str]) -> ReportRow {
        ReportRow {
            errors: errors.iter().map(|e| e.to_string()).collect(),
            ..row(key, cells)
        }
    }

    fn xml(result: &ReportResult) -> String {
        String::from_utf8(JunitWriter.write(result, &Header::default()).unwrap()).unwrap()
    }

    /// The smoke-test shape: a request that came back broken is an `<error>`,
    /// and the suite counts it, so CI paints the step red and shows the reason.
    #[test]
    fn a_failed_request_is_an_error_case() {
        let res = ReportResult {
            column_order: vec!["Ping.HttpStatus".into(), "Ping.Error".into()],
            rows: vec![
                row(&["a"], &[("Ping.HttpStatus", "200"), ("Ping.Error", "")]),
                failed_row(
                    &["b"],
                    &[
                        ("Ping.HttpStatus", "500"),
                        ("Ping.Error", "Expected status 200 but got 500"),
                    ],
                    &["Ping: Expected status 200 but got 500"],
                ),
            ],
            ..Default::default()
        };
        let out = xml(&res);
        assert!(out.contains("tests=\"2\""), "{out}");
        assert!(out.contains("errors=\"1\""), "{out}");
        assert!(out.contains("failures=\"0\""), "{out}");
        assert!(
            out.contains("name=\"b\"") && out.contains("but got 500"),
            "the failing case is named by its key: {out}"
        );
    }

    /// A wrong *answer* is a `<failure>`, not an `<error>`: the request worked.
    /// Every CI viewer shades the two differently, and the difference is the
    /// one a reader acts on — "the service is down" versus "the model is worse".
    #[test]
    fn a_wrong_answer_is_a_failure_with_both_sides_of_the_comparison() {
        let mut res = ReportResult {
            column_order: vec!["Verdict".into()],
            rows: vec![row(&["doc-1"], &[("Verdict", "fake")])],
            column_truths: [("Verdict".to_string(), "real".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        res.verdicts
            .insert((0, "Verdict".into()), Verdict::Incorrect);
        res.truths.insert((0, "Verdict".into()), "real".into());
        let out = xml(&res);
        assert!(out.contains("failures=\"1\""), "{out}");
        assert!(out.contains("errors=\"0\""), "{out}");
        assert!(
            out.contains("expected \\\"real\\\"") || out.contains("expected &quot;real&quot;"),
            "the message carries what was expected: {out}"
        );
        assert!(
            out.contains("got &quot;fake&quot;"),
            "and what came back: {out}"
        );
    }

    /// A dry run is a suite of skipped cases. A preview that reported green
    /// would be the same lie the report file used to tell.
    #[test]
    fn a_dry_run_skips_every_case_rather_than_passing_it() {
        let res = ReportResult {
            column_order: vec!["Ping.HttpStatus".into()],
            rows: vec![row(&["a"], &[("Ping.HttpStatus", "")])],
            dry_run: true,
            ..Default::default()
        };
        let out = xml(&res);
        assert!(out.contains("skipped=\"1\""), "{out}");
        assert!(out.contains("<skipped"), "{out}");
        assert!(out.contains("DRY RUN"), "the suite says why: {out}");
    }

    /// The dangerous shape: a run that produced no rows *because* something was
    /// wrong. An empty suite is a green suite in every CI system there is, so
    /// the run's own errors become a case of their own.
    #[test]
    fn a_run_error_that_belongs_to_no_row_still_fails_the_suite() {
        let res = ReportResult {
            errors: vec!["FILES \"cases/*.json\" matched nothing".into()],
            ..Default::default()
        };
        let out = xml(&res);
        assert!(out.contains("tests=\"1\""), "{out}");
        assert!(out.contains("errors=\"1\""), "{out}");
        assert!(out.contains("matched nothing"), "{out}");
    }

    /// A stopped run says so where a JUnit reader will see it, for the same
    /// reason every other format carries the caveat.
    #[test]
    fn a_partial_run_carries_its_caveat_on_the_suite() {
        let res = ReportResult {
            rows: vec![row(&["a"], &[("Ping.HttpStatus", "200")])],
            partial: Some(crate::report::model::Partial {
                rows_completed: 1,
                rows_planned: 9,
            }),
            ..Default::default()
        };
        let out = xml(&res);
        assert!(out.contains("PARTIAL"), "{out}");
        assert!(out.contains("1 of 9"), "{out}");
    }

    /// One malformed byte in a response body must not cost the whole file: an
    /// unparseable suite takes every *other* failure down with it.
    #[test]
    fn control_characters_and_markup_cannot_break_the_document() {
        let res = ReportResult {
            column_order: vec!["Body".into()],
            rows: vec![row(
                &["<script>"],
                &[("Body", "a\u{0}b & <tag> \"quoted\"")],
            )],
            ..Default::default()
        };
        let out = xml(&res);
        assert!(!out.contains('\u{0}'), "the NUL is gone: {out:?}");
        assert!(out.contains("&lt;script&gt;"), "{out}");
        assert!(out.contains("&amp;"), "{out}");
        assert!(!out.contains("<tag>"), "no raw markup from a body: {out}");
    }

    /// A row that carries no timing at all is not a row that took negative
    /// time. Rust's `f64` sum starts from `-0.0`, so this used to render
    /// `time="-0.000"` — a duration some JUnit parsers reject and every reader
    /// has to stop and think about.
    #[test]
    fn a_row_with_no_timing_takes_no_time_rather_than_negative_time() {
        let res = ReportResult {
            column_order: vec!["Ping.status".into()],
            rows: vec![row(&["a"], &[("Ping.status", "200")])],
            ..Default::default()
        };
        let out = xml(&res);
        assert!(!out.contains("-0.000"), "no negative durations: {out}");
        assert!(out.contains("time=\"0.000\""), "{out}");
    }

    /// Every shape this writer can produce, parsed by a real XML parser.
    ///
    /// The assertions elsewhere in this module are substring checks, and a
    /// substring check cannot see the thing that matters most here: whether a
    /// CI system can read the file at all. One illegal character or one
    /// unbalanced element makes the whole suite unreadable — including every
    /// failure it was written to report.
    #[test]
    fn every_document_this_writer_produces_parses_as_xml() {
        let awkward = ReportResult {
            column_order: vec!["Body".into(), "Ping.Time".into()],
            rows: vec![
                // U+FFFE and U+FFFF are legal Rust `char`s and illegal XML;
                // a fuzzer, a bad encoding guess or a binary body produces
                // them, and one is enough to make the document unparseable.
                failed_row(
                    &["odd \u{fffe}\u{ffff}\u{0}\u{1b}"],
                    &[("Body", "]]> & <b> \"q\" '\u{7f}'"), ("Ping.Time", "12")],
                    &["Ping: broken \u{ffff} pipe"],
                ),
                row(&["dup"], &[("Body", "one")]),
                row(&["dup"], &[("Body", "two")]),
                row(&[], &[("Body", "")]),
            ],
            errors: vec!["FILES \"x\" matched <nothing>".into()],
            warnings: vec!["CLEANUP failed & kept going".into()],
            skipped: vec!["Teardown".into()],
            ..Default::default()
        };
        let header = Header {
            lines: vec![crate::report::flow::HeaderLine::Directive {
                key: "name".into(),
                value: "A & B <report>".into(),
            }],
        };

        for (what, result, head) in [
            ("awkward", awkward, header),
            ("empty", ReportResult::default(), Header::default()),
            (
                "dry run",
                ReportResult {
                    rows: vec![row(&["a"], &[("Ping.HttpStatus", "")])],
                    dry_run: true,
                    errors: vec!["never sent".into()],
                    ..Default::default()
                },
                Header::default(),
            ),
        ] {
            let doc = String::from_utf8(JunitWriter.write(&result, &head).unwrap()).unwrap();
            let mut reader = quick_xml::Reader::from_str(&doc);
            let mut depth = 0i32;
            loop {
                match reader.read_event() {
                    Ok(quick_xml::events::Event::Start(_)) => depth += 1,
                    Ok(quick_xml::events::Event::End(_)) => depth -= 1,
                    Ok(quick_xml::events::Event::Eof) => break,
                    Ok(_) => {}
                    Err(e) => panic!("the {what} document is not XML: {e}\n{doc}"),
                }
            }
            assert_eq!(
                depth, 0,
                "unbalanced elements in the {what} document:\n{doc}"
            );
            for illegal in ['\u{0}', '\u{1b}', '\u{fffe}', '\u{ffff}'] {
                assert!(
                    !doc.contains(illegal),
                    "{illegal:?} survived into the {what} document"
                );
            }
        }
    }

    /// Two rows can legitimately share a key — a manifest with a repeated line,
    /// the same key under two comparison clauses. CI consumers key a case on
    /// `(classname, name)`, and GitLab keeps only the first of a colliding
    /// pair: a duplicate name does not merely read badly, it *hides* the second
    /// row's failure, which is the one shape a CI report must never take.
    #[test]
    fn colliding_row_keys_become_separate_cases() {
        let res = ReportResult {
            column_order: vec!["Body".into()],
            rows: vec![
                row(&["same"], &[("Body", "first")]),
                failed_row(&["same"], &[("Body", "second")], &["Ping: down"]),
                row(&["other"], &[("Body", "third")]),
            ],
            ..Default::default()
        };
        let out = xml(&res);
        assert!(out.contains("name=\"same #1\""), "{out}");
        assert!(out.contains("name=\"same #2\""), "{out}");
        // The row that did not collide keeps the readable name that makes a
        // case history worth having.
        assert!(out.contains("name=\"other\""), "{out}");
        assert!(out.contains("errors=\"1\""), "{out}");
    }

    /// A failed request is an error even when the report does not *show* the
    /// error: `HIDE(Error)`, or a `# columns:` directive, or a declared field
    /// list that never asked for one. Ownership is recorded where the failure
    /// happened, not re-read from a cell that may not exist.
    #[test]
    fn a_hidden_error_column_still_fails_its_case() {
        let res = ReportResult {
            column_order: vec!["Ping.HttpStatus".into()],
            rows: vec![failed_row(
                &["a"],
                &[("Ping.HttpStatus", "500")],
                &["Ping: Expected status 200 but got 500"],
            )],
            ..Default::default()
        };
        let out = xml(&res);
        assert!(out.contains("errors=\"1\""), "{out}");
        assert!(out.contains("but got 500"), "{out}");
        // Exactly one case, and exactly one error in it: the run's own copy of
        // the same failure must not be reported a second time as an orphan.
        assert_eq!(out.matches("<testcase").count(), 1, "{out}");
        assert_eq!(out.matches("<error").count(), 1, "{out}");
    }

    /// A run error that a row owns is that row's, and nobody else's. Before
    /// ownership was structural this was matched on the formatted text — which
    /// never matched, so every failed row was reported twice: once as itself
    /// and once as an unexplained run error.
    #[test]
    fn a_rows_error_is_not_also_reported_against_the_run() {
        let res = ReportResult {
            column_order: vec!["Ping.HttpStatus".into()],
            rows: vec![failed_row(
                &["a"],
                &[("Ping.HttpStatus", "500")],
                &["Ping: connection refused".to_string().as_str()],
            )],
            errors: vec!["Ping: connection refused".into()],
            ..Default::default()
        };
        let out = xml(&res);
        assert!(out.contains("tests=\"1\""), "one case, not two: {out}");
        assert!(out.contains("errors=\"1\""), "{out}");
    }

    /// A row whose request failed *and* whose answer was wrong is one case with
    /// one status. The stricter JUnit schemas allow a case only one outcome,
    /// and a case counted in two totals makes every consumer's arithmetic
    /// disagree with the document it is reading.
    #[test]
    fn a_failed_request_outranks_its_wrong_answer() {
        let mut res = ReportResult {
            column_order: vec!["Ping.status".into()],
            rows: vec![failed_row(
                &["a"],
                &[
                    ("Ping.status", "500"),
                    (crate::report::compare::CORRECT_COLUMN, "incorrect"),
                ],
                &["Ping: Expected status 200 but got 500"],
            )],
            ..Default::default()
        };
        res.verdicts
            .insert((0, "Ping.status".into()), Verdict::Incorrect);
        let out = xml(&res);
        assert!(out.contains("errors=\"1\""), "{out}");
        assert!(out.contains("failures=\"0\""), "{out}");
        assert_eq!(out.matches("<failure").count(), 0, "one status only: {out}");
        // The demoted verdict is not lost, only moved.
        assert!(out.contains("<system-out>"), "{out}");
    }

    /// The `Time` intrinsic under a user's own name. The field alias is
    /// recorded per run because the name says nothing about what it holds —
    /// and a case with no `time` is a case a CI viewer cannot chart.
    #[test]
    fn a_renamed_time_field_is_still_the_cases_duration() {
        let res = ReportResult {
            column_order: vec!["Ping.took".into()],
            rows: vec![row(&["a"], &[("Ping.took", "2500")])],
            duration_columns: ["Ping.took".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let out = xml(&res);
        assert!(out.contains("time=\"2.500\""), "{out}");
    }

    /// `time` is a plain non-negative number in the format, and some consumers
    /// reject the whole document over one that is not. A report can hold
    /// anything in a cell — a marker, a stale value, whatever a capture
    /// produced — so the writer reads only what is actually a duration.
    #[test]
    fn a_nonsense_timing_is_no_timing() {
        let res = ReportResult {
            column_order: vec!["A.Time".into(), "B.Time".into(), "C.Time".into()],
            rows: vec![row(
                &["a"],
                &[("A.Time", "-5"), ("B.Time", "NaN"), ("C.Time", "—")],
            )],
            ..Default::default()
        };
        let out = xml(&res);
        assert!(out.contains("time=\"0.000\""), "{out}");
        assert!(
            !out.contains("NaN") || !out.contains("time=\"NaN\""),
            "{out}"
        );
    }

    /// The `ENVS` shape: one key run against several targets is several cases,
    /// and a target name is as free-form as anything else a user types.
    #[test]
    fn comparison_targets_separate_the_cases_they_belong_to() {
        let mut staging = row(&["a"], &[("Ping.status", "200")]);
        staging.target = Some("staging <1>".into());
        let mut prod = row(&["a"], &[("Ping.status", "500")]);
        prod.target = Some("prod & co".into());
        let res = ReportResult {
            column_order: vec!["Ping.status".into()],
            rows: vec![staging, prod],
            ..Default::default()
        };
        let out = xml(&res);
        assert!(out.contains("name=\"a [staging &lt;1&gt;]\""), "{out}");
        assert!(out.contains("name=\"a [prod &amp; co]\""), "{out}");
    }

    /// A cell is truncated by characters, not by bytes: slicing a multibyte
    /// character in half produces a `String` that is not valid UTF-8 text for
    /// any consumer, and the boundary is exactly where a long response body
    /// lands.
    #[test]
    fn a_long_multibyte_cell_is_cut_on_a_character_boundary() {
        let body: String = "é".repeat(MAX_CELL + 10);
        let res = ReportResult {
            column_order: vec!["Body".into()],
            rows: vec![row(&["a"], &[("Body", &body)])],
            ..Default::default()
        };
        let out = xml(&res);
        assert!(out.contains('…'), "the cut is marked: {out:?}");
        assert_eq!(out.matches('é').count(), MAX_CELL, "{}", out.len());
    }

    /// The time a CI viewer charts per case: every `…Time` cell the row
    /// carries, in seconds, because milliseconds is not what the format means.
    #[test]
    fn a_cases_time_is_the_rows_own_milliseconds_in_seconds() {
        let res = ReportResult {
            column_order: vec!["Ping.Time".into(), "Pong.Time".into()],
            rows: vec![row(&["a"], &[("Ping.Time", "1200"), ("Pong.Time", "300")])],
            ..Default::default()
        };
        let out = xml(&res);
        assert!(out.contains("time=\"1.500\""), "{out}");
    }
}
