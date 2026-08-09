//! Label classes — the `# labels:` header directive.
//!
//! A ground truth is compared against a reported value, and the two rarely
//! spell the same verdict the same way: a truth file says `real` where an
//! engine answers `Low Risk`, a folder is called `fake` where the response says
//! `REJECT`. Rather than force the flow author to write conditionals to
//! translate one vocabulary into the other, a report declares the vocabulary
//! once:
//!
//! ```text
//! # labels: Pass = pass, ok, low risk, real
//! # labels: Fail = fail, reject, high risk, fake
//! ```
//!
//! Each line declares one *class*: a canonical label (used for display, and as
//! the axis order of a confusion matrix) and the synonyms that mean it. The
//! directive is repeatable, exactly like `collection:`.
//!
//! Two values match when they land in the same class. Values in no declared
//! class still compare — as themselves, case- and space-insensitively — so the
//! directive is entirely optional: a report whose truth file and responses
//! already agree needs none of it.
//!
//! Nothing here fails a run. An unparseable line contributes no class rather
//! than raising an error, so a half-typed directive in an editor still
//! round-trips; `validate` is where the user is told about it.

use std::collections::HashMap;

/// The label classes a report declares, in the order they were written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelMap {
    /// The canonical label of each class, in declaration order. This is the
    /// axis order of a confusion matrix, so it is a `Vec` and not a set.
    classes: Vec<String>,
    /// Canonicalised synonym → index into [`Self::classes`]. The canonical
    /// label of a class is always one of its own synonyms.
    lookup: HashMap<String, usize>,
    /// Synonyms declared by more than one class, as `(synonym, the class that
    /// keeps it, the class that asked for it second)`. Recorded rather than
    /// resolved silently: two classes claiming one word is a mistake in the
    /// directive that would otherwise show up as unexplained scoring.
    conflicts: Vec<(String, String, String)>,
    /// Directive lines that declared nothing, kept verbatim so validation can
    /// quote back exactly what was typed.
    malformed: Vec<String>,
}

impl LabelMap {
    /// Parse the raw `# labels:` values (as [`crate::report::flow::Header::labels`]
    /// hands them back) into a map.
    ///
    /// A line without an `=`, or with an empty half, declares nothing. A
    /// synonym already claimed by an earlier class stays with that earlier
    /// class: first declaration wins, so the conflict is visible in the order
    /// the file reads rather than depending on which line was edited last.
    pub fn parse(lines: &[&str]) -> Self {
        let mut map = LabelMap::default();
        for line in lines {
            let Some((name, synonyms)) = line.split_once('=') else {
                if !line.trim().is_empty() {
                    map.malformed.push(line.to_string());
                }
                continue;
            };
            let name = name.trim();
            if name.is_empty() {
                map.malformed.push(line.to_string());
                continue;
            }
            // Re-declaring a class extends it rather than starting a second one
            // with the same name, which would give a confusion matrix two
            // identical axis entries.
            let idx = match map.classes.iter().position(|c| c == name) {
                Some(i) => i,
                None => {
                    map.classes.push(name.to_string());
                    map.classes.len() - 1
                }
            };
            // The canonical label always means itself, even if it is not
            // repeated in the synonym list.
            for syn in std::iter::once(name).chain(synonyms.split(',')) {
                let key = canon(syn);
                if key.is_empty() {
                    continue;
                }
                match map.lookup.get(&key) {
                    Some(&owner) if owner != idx => {
                        let (kept, asked) = (map.classes[owner].clone(), name.to_string());
                        map.conflicts.push((key, kept, asked));
                    }
                    Some(_) => {}
                    None => {
                        map.lookup.insert(key, idx);
                    }
                }
            }
        }
        map
    }

    /// Synonyms that more than one class asked for; see [`Self::conflicts`].
    pub fn conflicts(&self) -> &[(String, String, String)] {
        &self.conflicts
    }

    /// Directive lines that declared no class at all.
    pub fn malformed(&self) -> &[String] {
        &self.malformed
    }

    /// The class `value` belongs to, if any.
    pub fn class_of(&self, value: &str) -> Option<usize> {
        self.lookup.get(&canon(value)).copied()
    }

    /// The canonical labels, in declaration order.
    ///
    /// This is also the axis order of a confusion matrix: the classes are a
    /// `Vec` and not a set precisely so that the order the author wrote them in
    /// is the order the matrix reads in.
    pub fn classes(&self) -> &[String] {
        &self.classes
    }

    /// How `value` should be displayed and counted: its class's canonical label
    /// when it has one, otherwise the value's own canonical form.
    ///
    /// A value in no declared class is deliberately kept as itself rather than
    /// lumped into an "other" bucket — an answer nobody expected is exactly
    /// what the reader most needs to see, and hiding it would make a confusion
    /// matrix lie about what happened.
    pub fn label_of(&self, value: &str) -> String {
        match self.class_of(value) {
            Some(i) => self.classes[i].clone(),
            None => canon(value),
        }
    }

    /// Whether two values mean the same thing: the same class, or — when
    /// neither is classified — the same text ignoring case and surrounding or
    /// repeated whitespace.
    pub fn same(&self, a: &str, b: &str) -> bool {
        match (self.class_of(a), self.class_of(b)) {
            (Some(x), Some(y)) => x == y,
            // One classified and one not can never be the same class; falling
            // back to text here would let a stray synonym-less value match a
            // class's canonical label by accident.
            (None, None) => canon(a) == canon(b),
            _ => false,
        }
    }
}

/// The comparison form of a label: trimmed, lowercased, and with runs of
/// internal whitespace collapsed to one space.
///
/// This is why `Low Risk`, `low risk` and `low  risk` are one value. It is
/// deliberately conservative — no punctuation stripping, no stemming — because
/// every transformation here is one the user cannot see in the report, and the
/// `# labels:` directive is the visible way to say two spellings are the same.
pub fn canon(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for word in value.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&word.to_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canon_folds_case_and_collapses_whitespace() {
        assert_eq!(canon("  Low   RISK\t"), "low risk");
        assert_eq!(canon(""), "");
        assert_eq!(canon("   "), "");
    }

    #[test]
    fn declared_synonyms_match_across_vocabularies() {
        let map = LabelMap::parse(&[
            "Pass = pass, ok, low risk, real",
            "Fail = fail, reject, high risk, fake",
        ]);
        assert!(map.same("fail", "High Risk"), "and the second class holds");
        assert_eq!(map.classes(), ["Pass".to_string(), "Fail".to_string()]);
        assert_eq!(map.label_of("REJECT"), "Fail", "counted under its class");
        assert!(
            map.same("Low Risk", "real"),
            "an engine verdict matches a folder label through the class"
        );
        assert!(!map.same("Low Risk", "fake"));
        assert_eq!(
            map.class_of("REJECT"),
            map.class_of("high risk"),
            "however it was spelled"
        );
    }

    #[test]
    fn the_canonical_label_means_itself_without_being_repeated() {
        let map = LabelMap::parse(&["Pass = ok"]);
        assert!(map.same("pass", "OK"));
    }

    /// With no directive at all the comparison still works, which is what makes
    /// `# labels:` optional rather than boilerplate.
    #[test]
    fn undeclared_values_compare_as_themselves() {
        let map = LabelMap::parse(&[]);
        assert_eq!(map.class_of("approved"), None);
        assert_eq!(
            map.label_of("Approved"),
            "approved",
            "an unclassified value counts as itself"
        );
        assert!(map.same(" APPROVED ", "approved"));
        assert!(!map.same("approved", "declined"));
    }

    /// A value nobody classified must not match a class by spelling: that would
    /// silently score a row against a label the author never declared.
    #[test]
    fn an_unclassified_value_never_matches_a_classified_one() {
        let map = LabelMap::parse(&["Pass = ok"]);
        assert!(!map.same("Pass", "passing"));
    }

    #[test]
    fn a_synonym_claimed_twice_stays_with_the_first_class_and_is_reported() {
        let map = LabelMap::parse(&["Pass = ok, maybe", "Fail = no, maybe"]);
        assert_eq!(map.class_of("maybe"), map.class_of("ok"));
        assert_eq!(
            map.conflicts(),
            [("maybe".to_string(), "Pass".to_string(), "Fail".to_string())],
            "the clash is recorded for validation rather than resolved silently"
        );
    }

    /// Re-declaring a class extends it; two identical axis entries would make a
    /// confusion matrix unreadable.
    #[test]
    fn redeclaring_a_class_extends_it() {
        let map = LabelMap::parse(&["Pass = ok", "Pass = fine"]);
        assert!(map.same("fine", "ok"));
        assert!(
            map.conflicts().is_empty(),
            "extending a class is not a clash"
        );
    }

    /// A half-typed directive in an editor must not blow anything up.
    #[test]
    fn malformed_lines_declare_nothing() {
        let map = LabelMap::parse(&["", "no equals sign", " = orphan", "Pass ="]);
        assert!(
            map.same("pass", "PASS"),
            "only the last line declared a class"
        );
        assert_eq!(map.class_of("orphan"), None);
        assert_eq!(
            map.malformed(),
            ["no equals sign".to_string(), " = orphan".to_string()],
            "each unusable line is kept verbatim, and a blank one is not a line"
        );
    }
}
