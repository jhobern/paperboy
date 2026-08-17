//! Field-by-field comparison of two JSON documents.
//!
//! A comparison run answers "did this cell change?" by comparing the whole
//! cell, which for a 4 KB response body is an honest but useless answer: the
//! reader still has to eyeball two blobs to find the one number that moved.
//! This module flattens both sides to `path → leaf` maps and unions the keys,
//! so the drill-down can say *which field* changed.
//!
//! The one non-obvious rule is copied deliberately from the Streamlit tool this
//! feature is modelled on: **a list of `{key, value}` objects is keyed by its
//! `key`, not by its position.** Engines routinely emit their per-check results
//! as such a list and routinely emit them in a different order between
//! environments; indexing by position would then report every check as changed
//! and hide the one that actually did.

use serde_json::Value;
use std::collections::BTreeMap;

/// One field of a flattened-and-unioned pair of documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDiff {
    /// The dotted path to the leaf, e.g. `checks.match.score`.
    pub path: String,
    /// The baseline's value, or `None` where the baseline had no such field.
    pub baseline: Option<String>,
    /// The candidate's value, or `None` where the candidate has no such field.
    pub candidate: Option<String>,
}

impl FieldDiff {
    /// Whether the two sides disagree. A field missing from one side counts as
    /// a difference: an absent field and an empty one are not the same answer.
    pub fn differs(&self) -> bool {
        self.baseline != self.candidate
    }
}

/// Flatten `value` to a map of dotted path to leaf text.
///
/// Leaves are rendered as their JSON text minus the quotes on strings, because
/// the drill-down is read by a person: `"pass"` and `pass` are the same answer
/// and showing the quotes only adds noise to every row.
pub fn flatten(value: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    walk("", value, &mut out);
    out
}

fn walk(prefix: &str, value: &Value, out: &mut BTreeMap<String, String>) {
    let join = |k: &str| {
        if prefix.is_empty() {
            k.to_string()
        } else {
            format!("{prefix}.{k}")
        }
    };
    match value {
        Value::Object(map) => {
            if map.is_empty() {
                out.insert(prefix.to_string(), "{}".to_string());
                return;
            }
            for (k, v) in map {
                walk(&join(k), v, out);
            }
        }
        Value::Array(items) => {
            if items.is_empty() {
                out.insert(prefix.to_string(), "[]".to_string());
                return;
            }
            // Keyed by `key` only when *every* element offers one, so a
            // half-keyed list falls back to positions rather than silently
            // losing the elements that had no key.
            let keyed: Option<Vec<(String, &Value)>> = items
                .iter()
                .map(|item| {
                    let obj = item.as_object()?;
                    let k = scalar_text(obj.get("key")?)?;
                    Some((k, obj.get("value").unwrap_or(item)))
                })
                .collect();
            match keyed {
                Some(pairs) => {
                    for (k, v) in pairs {
                        walk(&join(&k), v, out);
                    }
                }
                None => {
                    for (i, v) in items.iter().enumerate() {
                        walk(&format!("{prefix}[{i}]"), v, out);
                    }
                }
            }
        }
        other => {
            out.insert(prefix.to_string(), leaf_text(other));
        }
    }
}

/// A scalar rendered as a map key, or `None` for anything that isn't scalar.
fn scalar_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn leaf_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// The field-by-field diff of two JSON texts, or `None` if either side isn't
/// JSON.
///
/// Resolving by *shape* rather than by a declaration is the same rule `IMAGE`
/// already follows: a column that happens to hold JSON on both sides gets the
/// richer rendering without anyone having to say so, and one that doesn't falls
/// back to the plain before/after text.
pub fn diff_texts(baseline: &str, candidate: &str) -> Option<Vec<FieldDiff>> {
    let b: Value = serde_json::from_str(baseline.trim()).ok()?;
    let c: Value = serde_json::from_str(candidate.trim()).ok()?;
    Some(diff(&b, &c))
}

/// The field-by-field diff of two parsed documents, in path order.
pub fn diff(baseline: &Value, candidate: &Value) -> Vec<FieldDiff> {
    let b = flatten(baseline);
    let c = flatten(candidate);
    let mut paths: Vec<&String> = b.keys().chain(c.keys()).collect();
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .map(|p| FieldDiff {
            path: p.clone(),
            baseline: b.get(p).cloned(),
            candidate: c.get(p).cloned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nested_objects_flatten_to_dotted_paths() {
        let f = flatten(&json!({"a": {"b": 1, "c": "x"}, "d": true}));
        assert_eq!(f.get("a.b").map(String::as_str), Some("1"));
        assert_eq!(f.get("a.c").map(String::as_str), Some("x"));
        assert_eq!(f.get("d").map(String::as_str), Some("true"));
    }

    /// The reason this module exists: engines emit per-check results as a list
    /// of `{key, value}` objects and reorder them freely between environments.
    /// Positional indexing would call every check changed and hide the real one.
    #[test]
    fn key_value_lists_align_regardless_of_order() {
        let a = json!({"checks": [{"key": "face", "value": "pass"},
                                  {"key": "doc", "value": "fail"}]});
        let b = json!({"checks": [{"key": "doc", "value": "fail"},
                                  {"key": "face", "value": "warn"}]});
        let d = diff(&a, &b);
        let changed: Vec<&FieldDiff> = d.iter().filter(|f| f.differs()).collect();
        assert_eq!(changed.len(), 1, "only the one check that moved: {d:?}");
        assert_eq!(changed[0].path, "checks.face");
        assert_eq!(changed[0].baseline.as_deref(), Some("pass"));
        assert_eq!(changed[0].candidate.as_deref(), Some("warn"));
    }

    /// A list only half of whose elements carry a key can't be aligned by key
    /// without dropping the rest, so it falls back to positions.
    #[test]
    fn a_half_keyed_list_falls_back_to_positions() {
        let f = flatten(&json!({"xs": [{"key": "a", "value": 1}, {"nope": 2}]}));
        assert_eq!(f.get("xs[0].key").map(String::as_str), Some("a"));
        assert_eq!(f.get("xs[1].nope").map(String::as_str), Some("2"));
    }

    /// A field present on one side only is a difference; reporting it as
    /// "unchanged and empty" would hide a dropped field entirely.
    #[test]
    fn a_field_missing_from_one_side_counts_as_a_difference() {
        let d = diff(&json!({"a": 1}), &json!({"a": 1, "b": 2}));
        let b = d.iter().find(|f| f.path == "b").expect("the new field");
        assert!(b.differs());
        assert_eq!(b.baseline, None);
        assert_eq!(b.candidate.as_deref(), Some("2"));
    }

    /// Empty containers are leaves: dropping them would make "the list went
    /// empty" invisible.
    #[test]
    fn empty_containers_are_reported_as_leaves() {
        let f = flatten(&json!({"xs": [], "o": {}}));
        assert_eq!(f.get("xs").map(String::as_str), Some("[]"));
        assert_eq!(f.get("o").map(String::as_str), Some("{}"));
    }

    /// Not-JSON is not an error, it just isn't diffable — the caller falls back
    /// to plain before/after text.
    #[test]
    fn non_json_text_yields_no_diff() {
        assert!(diff_texts("hello", "{\"a\":1}").is_none());
        assert!(diff_texts("{\"a\":1}", "not json").is_none());
        assert!(diff_texts(" {\"a\":1} ", "{\"a\":2}").is_some());
    }
}
