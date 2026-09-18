//! What a step produced, and how each value came to be.
//!
//! A value and its provenance used to be two maps side by side, kept in step
//! by nothing but the fact that the two lines that wrote them sat next to each
//! other. That held — but it had already failed once across a fork, where a
//! region step's values travelled back to the enclosing block and their
//! provenance did not, and every teardown for a step inside a region was gated
//! on the wrong question.
//!
//! So the two live behind one type, in a module of their own. The fields are
//! private and the only ways in are paired, which is what makes "written
//! together" a fact about the program rather than a habit: a caller cannot
//! record a value without saying where it came from, and cannot move the
//! values across a fork while leaving the provenance behind.

use std::collections::{HashMap, HashSet};

/// Where a value came from, which is what decides when a teardown holding it
/// may run.
///
/// A captured value is a fact about what came *back*: it exists only if the
/// response carried it, so a teardown on one waits for the step to have
/// succeeded. A generated value is a fact about what was *sent* — a `[Gen]`
/// row is evaluated before the request leaves — so a teardown on one runs on
/// having been produced, even when the send then failed. That asymmetry is the
/// entire reason this type exists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provenance {
    Captured,
    Generated,
}

/// One step's output: its values, each remembering how it came to be.
#[derive(Clone, Default, Debug)]
pub struct Produced {
    values: HashMap<String, String>,
    /// The subset of `values` that a `[Gen]` row minted. Held as a set rather
    /// than a `Provenance` per entry so the common read — "what is the value" —
    /// stays a plain map lookup.
    generated: HashSet<String>,
}

impl Produced {
    /// Record a value read back from the response.
    ///
    /// This clears any generated mark: a request may declare a `[Gen]` row and
    /// a capture for the same name — mint an id, then read the server's
    /// canonical one back — and when the capture fires it is the answer, so the
    /// name stops being a generated one.
    pub fn record_captured(&mut self, name: &str, value: &str) {
        self.values.insert(name.to_string(), value.to_string());
        self.generated.remove(name);
    }

    /// Record a value minted before the send.
    pub fn record_generated(&mut self, name: &str, value: &str) {
        self.values.insert(name.to_string(), value.to_string());
        self.generated.insert(name.to_string());
    }

    pub fn get(&self, name: &str) -> Option<&String> {
        self.values.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    /// Whether the live value for `name` was minted rather than read back.
    ///
    /// False for a name with no value at all: nothing was produced, so there is
    /// no provenance to report.
    pub fn is_generated(&self, name: &str) -> bool {
        self.generated.contains(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.values.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capture_that_fires_clears_the_generated_mark() {
        // The pair has to move together in both directions. Recording the
        // capture without clearing the mark would leave a value the server
        // supplied still claiming to have been minted here, and a teardown
        // holding it would run against a resource the failed step never made.
        let mut p = Produced::default();
        p.record_generated("sid", "MINTED");
        assert!(p.is_generated("sid"));
        p.record_captured("sid", "SERVER");
        assert_eq!(p.get("sid").map(String::as_str), Some("SERVER"));
        assert!(
            !p.is_generated("sid"),
            "the capture answered, so it is not minted"
        );
    }

    #[test]
    fn a_name_nothing_produced_has_no_provenance() {
        let p = Produced::default();
        assert!(!p.contains("sid"));
        assert!(!p.is_generated("sid"));
    }

    #[test]
    fn a_generated_value_that_a_capture_never_answered_stays_generated() {
        // The case the gating rule turns on: the send failed, so the capture
        // never fired, and the minted value is still the live one.
        let mut p = Produced::default();
        p.record_generated("sid", "MINTED");
        p.record_captured("other", "X");
        assert!(p.is_generated("sid"));
    }
}
