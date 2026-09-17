//! Binding a report's declared [`PARAM`](crate::report::flow::ParamDecl)s
//! against the values chosen for one particular run.
//!
//! A parameter's value comes from one of two places: what the run was given, or
//! the default written in the `.trail` file. Nothing else — in particular a
//! chosen value is never written back into the file, so a report under version
//! control keeps meaning the same thing to everyone who opens it.
//!
//! The checks here are the ones that can only be made once a value exists, so
//! they deliberately overlap with [`super::validate`]: that pass judges the
//! *declaration* (is this default one of the choices?), this one judges the
//! *value* (is what you picked one of the choices?). A report can pass
//! validation and still be run with nonsense.

use std::collections::HashMap;

use super::flow::{ParamDecl, ParamKind};
use crate::i18n::{Strings, fill};

/// The values chosen for a run, keyed by the parameter's raw name.
///
/// Keyed by name rather than by prompt on purpose: the name is the parameter's
/// identity everywhere (`{{NAME}}`, `--param NAME=…`, the remembered values), so
/// relabelling a parameter never orphans a value.
pub type ParamValues = HashMap<String, String>;

/// Parse one `--param NAME=VALUE` argument.
///
/// Split at the *first* `=` only, and the value is taken verbatim — not
/// trimmed, not unquoted. A parameter is most often a path, and a path is
/// exactly the kind of value that may legitimately contain an `=` or end in a
/// space; the shell has already done the one round of quote removal there is to
/// do, so anything further here would be this code editing the user's value
/// behind their back. `check` trims before judging, so a padded value is still
/// judged on what it says.
///
/// An empty value (`--param NAME=`) is accepted and means "empty", which is not
/// the same as leaving the parameter out (that takes the report's default).
pub fn parse_assignment(arg: &str) -> Result<(String, String), String> {
    let Some((name, value)) = arg.split_once('=') else {
        return Err(format!(
            "expected NAME=VALUE, got `{arg}` (no `=`; e.g. --param CASES_DIR=./cases)"
        ));
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("`{arg}` has no parameter name before the `=`"));
    }
    Ok((name.to_string(), value.to_string()))
}

/// The supplied names that no `PARAM` in the report declares.
///
/// Worth its own error rather than a shrug: a caller scripting PaperBoy writes
/// the `--param` line once and then never looks at it again, so a parameter
/// renamed in the `.trail` would otherwise go on being "supplied" to nothing,
/// and the run would quietly use the default instead of the value the caller
/// believes it passed.
pub fn undeclared<'a>(decls: &[&ParamDecl], chosen: &'a ParamValues) -> Vec<&'a str> {
    let mut names: Vec<&str> = chosen
        .keys()
        .filter(|name| !decls.iter().any(|d| &&d.name == name))
        .map(String::as_str)
        .collect();
    // A HashMap's order is arbitrary and this text is read by a human (and by
    // tests), so it is sorted rather than left to vary run to run.
    names.sort_unstable();
    names
}

/// The value a parameter takes for this run, or why it can't take one.
///
/// A failure is reported rather than guessed at: substituting an empty string
/// for a required parameter produces a report full of plausible-looking rows
/// built from a URL with a hole in it, which is worse than not running.
pub fn value_for(decl: &ParamDecl, chosen: &ParamValues, s: &Strings) -> Result<String, String> {
    let value = match chosen.get(&decl.name) {
        Some(v) => v.clone(),
        None => match &decl.default {
            Some(d) => d.clone(),
            // No default and nothing supplied: the parameter is required.
            None => return Err(fill(s.param_required, &[&decl.prompt(), &decl.name])),
        },
    };
    check(decl, &value, s)?;
    Ok(value)
}

/// The value every declared parameter takes for this run — what was chosen, or
/// the declaration's own default — as a substitution map.
///
/// Used wherever a *name in the script* may be written as `{{PARAM}}` rather
/// than spelled out (an `ENVS` clause's environments, for one). Only parameters
/// are in it: an `ENVS` clause is read before anything has run, so resolving it
/// against captures or assignments would mean a name whose meaning depends on
/// where the run had got to.
pub fn effective(decls: &[&ParamDecl], chosen: &ParamValues) -> ParamValues {
    decls
        .iter()
        .filter_map(|d| {
            let v = chosen.get(&d.name).or(d.default.as_ref())?;
            Some((d.name.clone(), v.clone()))
        })
        .collect()
}

/// One row of a run settings form: a declared parameter, the value this run
/// will use for it, and what (if anything) is wrong with that value.
///
/// Lives here rather than in either front-end because both build the same
/// form from the same declarations, and a question that is worded one way in
/// the terminal and another in the window is two different reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamRow {
    /// The raw name — what `{{NAME}}` reads, what `--param NAME=…` sets and
    /// what the value is remembered under.
    pub name: String,
    /// What the row asks for, in words (the `LABEL`, or derived from the name).
    pub prompt: String,
    pub kind: ParamKind,
    pub value: String,
    /// Why this value wouldn't be accepted, if it wouldn't.
    pub problem: Option<String>,
}

/// Build the run settings form for `decls` against the values chosen so far.
///
/// A row's value falls back to the declaration's default, so a form opened
/// before anything is chosen already shows what the report would run with
/// today. The complaint is the same check that would stop the run, made while
/// there is still someone here to fix it.
pub fn rows(decls: &[&ParamDecl], chosen: &ParamValues, s: &Strings) -> Vec<ParamRow> {
    decls
        .iter()
        .map(|p| {
            let value = chosen
                .get(&p.name)
                .cloned()
                .or_else(|| p.default.clone())
                .unwrap_or_default();
            ParamRow {
                name: p.name.clone(),
                prompt: p.prompt(),
                kind: p.kind.clone(),
                problem: check(p, &value, s).err().or_else(|| {
                    (value.trim().is_empty() && p.default.is_none())
                        .then(|| s.param_row_required.to_string())
                }),
                value,
            }
        })
        .collect()
}

/// Whether `value` is acceptable for `decl`.
///
/// Shared with the front-ends so a run settings form can say "not a number"
/// while it is being typed, using the same rule that would stop the run.
pub fn check(decl: &ParamDecl, value: &str, s: &Strings) -> Result<(), String> {
    let trimmed = value.trim();
    // A value that still has to be interpolated can't be judged yet; the run
    // substitutes it and whatever it becomes stands.
    if trimmed.contains("{{") {
        return Ok(());
    }
    match &decl.kind {
        ParamKind::Choice(options) if !options.is_empty() => {
            if !options.iter().any(|o| o == trimmed) {
                return Err(fill(
                    s.param_not_a_choice,
                    &[&decl.prompt(), trimmed, &options.join(", ")],
                ));
            }
        }
        ParamKind::Number => {
            if trimmed.is_empty() || trimmed.parse::<f64>().is_err() {
                return Err(fill(s.param_not_a_number, &[&decl.prompt(), trimmed]));
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::parser::parse_flow;

    fn decl(src: &str) -> ParamDecl {
        let flow = parse_flow(&format!("{src}\n")).expect("parses");
        match &flow.nodes[0] {
            super::super::flow::FlowNode::Param(p) => p.clone(),
            other => panic!("expected Param, got {other:?}"),
        }
    }

    /// `--param` is how a caller scripting PaperBoy points one report at a
    /// different folder each run, so the value has to survive the trip
    /// untouched: split on the first `=` only, and no trimming of what follows.
    #[test]
    fn an_assignment_keeps_everything_after_the_first_equals() {
        assert_eq!(
            parse_assignment("CASES_DIR=./batch-07"),
            Ok(("CASES_DIR".to_string(), "./batch-07".to_string()))
        );
        // A value may hold `=` (a query string, a base64 tail) — only the first
        // one separates.
        assert_eq!(
            parse_assignment("URL=https://x.test/?a=1&b=2"),
            Ok(("URL".to_string(), "https://x.test/?a=1&b=2".to_string()))
        );
        // The name is trimmed (shells make that easy to do by accident); the
        // value is not, because a path is allowed to look like anything.
        assert_eq!(
            parse_assignment(" CASES = /tmp/a b "),
            Ok(("CASES".to_string(), " /tmp/a b ".to_string()))
        );
        // Explicitly empty is a value, and different from not passing it.
        assert_eq!(
            parse_assignment("CASES="),
            Ok(("CASES".to_string(), String::new()))
        );

        assert!(parse_assignment("CASES_DIR").is_err(), "no `=`");
        assert!(parse_assignment("=/tmp/a").is_err(), "no name");
    }

    /// A name no `PARAM` declares is reported rather than ignored: a command
    /// line that has drifted from the script would otherwise run the report
    /// against the default it thought it had replaced.
    #[test]
    fn undeclared_names_are_listed_and_declared_ones_are_not() {
        let a = decl("PARAM FOLDER CASES = \"./x\"");
        let b = decl("PARAM TEXT TICKET");
        let decls = [&a, &b];

        let mut chosen = ParamValues::new();
        chosen.insert("CASES".into(), "./y".into());
        assert!(undeclared(&decls, &chosen).is_empty());

        chosen.insert("CASE_DIR".into(), "./y".into());
        chosen.insert("AAA".into(), "1".into());
        assert_eq!(undeclared(&decls, &chosen), vec!["AAA", "CASE_DIR"]);
    }

    /// A parameter with no default is the report saying "I can't run without
    /// being told this". Filling in an empty string would produce a report of
    /// plausible-looking rows built from a hole.
    #[test]
    fn a_required_parameter_stops_the_run_rather_than_running_empty() {
        let s = Strings::english();
        let d = decl("PARAM TEXT TICKET_REF");
        let err = value_for(&d, &ParamValues::new(), s).unwrap_err();
        assert!(err.contains("Ticket ref"), "{err}");

        let mut chosen = ParamValues::new();
        chosen.insert("TICKET_REF".into(), "T-1".into());
        assert_eq!(value_for(&d, &chosen, s).unwrap(), "T-1");
    }

    #[test]
    fn a_chosen_value_is_held_to_the_same_rules_as_a_default() {
        let s = Strings::english();
        let d = decl("PARAM CHOICE(\"v4.2\", \"v4.3\") VERSION = \"v4.3\"");
        let mut chosen = ParamValues::new();
        chosen.insert("VERSION".into(), "v9".into());
        let err = value_for(&d, &chosen, s).unwrap_err();
        assert!(err.contains("v9") && err.contains("v4.2, v4.3"), "{err}");

        let n = decl("PARAM NUMBER TRIES = \"3\"");
        chosen.insert("TRIES".into(), "lots".into());
        assert!(value_for(&n, &chosen, s).is_err());
        chosen.insert("TRIES".into(), "5".into());
        assert_eq!(value_for(&n, &chosen, s).unwrap(), "5");
    }

    /// The chosen value wins over the file's default; the file is never
    /// rewritten to match.
    #[test]
    fn a_supplied_value_beats_the_declared_default() {
        let s = Strings::english();
        let d = decl("PARAM ENV TARGET = \"staging\"");
        assert_eq!(value_for(&d, &ParamValues::new(), s).unwrap(), "staging");
        let mut chosen = ParamValues::new();
        chosen.insert("TARGET".into(), "prod".into());
        assert_eq!(value_for(&d, &chosen, s).unwrap(), "prod");
    }
}
