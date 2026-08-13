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
