//! The row model behind the *variables* views, shared by both front-ends so the
//! terminal UI and the GUI answer the same question the same way.
//!
//! There are two different questions about a captured value, and conflating
//! them is the whole reason this module exists:
//!
//! - **"What did *this* request capture?"** — answered by
//!   [`ApiResponse::captures`](crate::http::ApiResponse::captures), a snapshot
//!   taken when that request last ran. Shown in the Response pane's Captures
//!   section.
//! - **"What is `{{ VAR }}` worth *right now*?"** — answered here, from the
//!   live pool. Shown in the variables view (the terminal UI's `v` popup, the
//!   GUI's Environments panel).
//!
//! The second question had no honest answer before this module. Substitution
//! reads [`crate::request::collection_vars`], which is the environment's
//! variables **overridden by** the capture pool, but both front-ends listed the
//! environment's own rows and nothing else. A request that captured
//! `access_token` therefore left the environment row sitting there displaying
//! the *old* value — not unlisted, but actively wrong, since that is not what
//! the next request sends. [`capture_rows`] supplies the missing rows and
//! [`env_var_overridden`] marks the misleading ones.

use std::collections::HashSet;

use crate::collection::Collection;
use crate::environment::{Environment, SECRET_MASK};

/// One row of the live capture pool.
pub struct CaptureRow {
    pub key: String,
    pub value: String,
    /// This capture shadows an environment variable of the same name. The
    /// environment's row is still on screen showing its own value, and that
    /// value is not the one being sent — see [`env_var_overridden`].
    pub shadows_env: bool,
}

/// Every name computed by some entry's `# [Gen]` block.
///
/// These reach the pool like captures do (`CaptureUpdate::values` merges them,
/// so the next request can substitute a `nonce` this one computed), but they
/// must never be *displayed*: a computed value may be an HMAC of a secret, and
/// the figure on hand is in any case the previous send's, while the next send
/// computes a fresh one. `request::subst_map` refuses them for exactly this
/// reason and renders `{{name}}` in the computed colour instead; every variables
/// view has to refuse them too, or the pool becomes the leak that the request
/// preview was careful not to be.
fn generated_names(col: &Collection) -> HashSet<&str> {
    col.entries
        .iter()
        .flat_map(|e| e.generators.iter().map(|(name, _)| name.as_str()))
        .collect()
}

/// The live capture pool as display rows: sorted by name, computed values
/// removed, each marked if it is shadowing an environment variable.
///
/// Sorted because [`Collection::captures`] is a `HashMap`, whose iteration
/// order is not merely unspecified but *differs between iterations* — listed
/// raw, the rows would visibly reshuffle from frame to frame while the reader
/// tried to look at them.
pub fn capture_rows(col: &Collection, env: Option<&Environment>) -> Vec<CaptureRow> {
    let computed = generated_names(col);
    let mut rows: Vec<CaptureRow> = col
        .captures
        .iter()
        .filter(|(k, _)| !computed.contains(k.as_str()))
        .map(|(k, v)| CaptureRow {
            key: k.clone(),
            value: v.clone(),
            shadows_env: env.is_some_and(|e| e.vars.iter().any(|var| var.key == *k)),
        })
        .collect();
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    rows
}

/// Whether an environment variable named `key` is currently being overridden by
/// a capture, making the value displayed beside it not the value that is sent.
///
/// Computed names are excluded for the same reason [`capture_rows`] drops them:
/// a `# [Gen]` name in the pool is not something the reader can be shown or
/// told about here, and the request preview already colours it as computed.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn env_var_overridden(col: &Collection, key: &str) -> bool {
    col.captures.contains_key(key) && !generated_names(col).contains(key)
}

/// Whether a capture recorded on a *response* has since been overwritten, so
/// what the Response pane is showing is a historical snapshot rather than the
/// value in force.
///
/// A response keeps its captures for as long as the request keeps its
/// `last_response`, which is indefinitely; run any other request that captures
/// the same name and the old figure stays on screen, still looking current.
/// `true` here is what lets a front-end say so.
pub fn is_superseded(col: &Collection, key: &str, value: &str) -> bool {
    col.captures.get(key).is_some_and(|live| live != value)
}

/// A capture value as it should be shown. Captures are masked by default:
/// unlike an environment variable, which is marked secret at its source, a
/// capture has no such marking and is very often a bearer token — that is what
/// `[Captures]` is mostly *for* — so the safe default is the only defensible
/// one in a pane that gets screen-shared and screenshotted.
///
/// Display-only. Copying yields the real value, following the Response pane's
/// compact toggle, which is likewise a view over text the copy paths ignore:
/// a masked value nobody can retrieve would defeat the point of listing it.
pub fn shown_value(value: &str, revealed: bool) -> String {
    if revealed {
        value.to_string()
    } else {
        SECRET_MASK.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::{EnvVar, ValueSource};

    fn var(key: &str) -> EnvVar {
        EnvVar {
            key: key.into(),
            value: "from-env".into(),
            source: ValueSource::Literal,
            resolved: true,
            loading: false,
            original_value: "from-env".into(),
            modified: false,
            user_added: false,
            raw: String::new(),
        }
    }

    fn env_with(keys: &[&str]) -> Environment {
        Environment {
            id: 1,
            name: "e".into(),
            vars: keys.iter().map(|k| var(k)).collect(),
            path: None,
            git_origin: None,
        }
    }

    fn col_with(captures: &[(&str, &str)]) -> Collection {
        let mut col = Collection::new("c".to_string(), Vec::new());
        for (k, v) in captures {
            col.captures.insert((*k).to_string(), (*v).to_string());
        }
        col
    }

    #[test]
    fn capture_rows_come_back_sorted_by_name() {
        let col = col_with(&[("zeta", "1"), ("alpha", "2"), ("mid", "3")]);
        let keys: Vec<String> = capture_rows(&col, None)
            .into_iter()
            .map(|r| r.key)
            .collect();
        assert_eq!(
            keys,
            ["alpha", "mid", "zeta"],
            "a HashMap reshuffles between iterations; the view must not"
        );
    }

    /// The reason the view exists: substitution gives a capture precedence over
    /// an environment variable of the same name, so the environment's row is
    /// showing a value that will not be sent.
    #[test]
    fn a_capture_over_an_env_var_is_flagged_on_both_sides() {
        let col = col_with(&[("access_token", "from-capture")]);
        let env = env_with(&["access_token", "base_url"]);

        let rows = capture_rows(&col, Some(&env));
        assert_eq!(rows.len(), 1);
        assert!(rows[0].shadows_env);

        assert!(env_var_overridden(&col, "access_token"));
        assert!(!env_var_overridden(&col, "base_url"));
    }

    #[test]
    fn a_capture_with_no_env_var_of_that_name_shadows_nothing() {
        let col = col_with(&[("session", "abc")]);
        let env = env_with(&["base_url"]);
        assert!(!capture_rows(&col, Some(&env))[0].shadows_env);
        // With no environment at all there is nothing to shadow, and the row
        // still has to be listed: a capture does not need an environment to
        // exist, which is why the view opens without one.
        assert!(!capture_rows(&col, None)[0].shadows_env);
    }

    /// A computed value may be an HMAC of a secret, and the one in the pool is
    /// the *last* send's while the next send computes a fresh one. It is
    /// refused everywhere it could be read.
    #[test]
    fn a_generated_value_never_reaches_a_variables_view() {
        let mut col = col_with(&[("nonce", "deadbeef"), ("token", "t")]);
        col.entries = vec![crate::hurl::HurlEntry {
            generators: vec![("nonce".to_string(), "uuid".to_string())],
            ..Default::default()
        }];

        let keys: Vec<String> = capture_rows(&col, None)
            .into_iter()
            .map(|r| r.key)
            .collect();
        assert_eq!(keys, ["token"], "the computed name must not be listed");

        // Nor may an env var of that name be labelled "overridden": saying so
        // would announce a computed value's existence and point at the row
        // holding it.
        assert!(!env_var_overridden(&col, "nonce"));
        assert!(env_var_overridden(&col, "token"));
    }

    #[test]
    fn a_response_capture_is_superseded_only_once_the_pool_moves_on() {
        let col = col_with(&[("token", "second")]);
        assert!(
            is_superseded(&col, "token", "first"),
            "the response is showing a value another run has replaced"
        );
        assert!(!is_superseded(&col, "token", "second"));
        assert!(
            !is_superseded(&col, "gone", "first"),
            "a name no longer in the pool has nothing newer to be superseded by"
        );
    }

    #[test]
    fn values_are_masked_until_revealed() {
        assert_eq!(shown_value("secret-token", false), SECRET_MASK);
        assert_eq!(shown_value("secret-token", true), "secret-token");
    }
}
