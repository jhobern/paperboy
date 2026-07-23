//! The front-end-agnostic runtime [`Report`]: an in-memory `.report` document
//! (a display name, its PaperTrail source text, and file provenance) plus
//! local load/save. It deliberately mirrors [`crate::collection::Collection`]
//! so the TUI (and a future GUI) can treat a report tab like a collection tab.
//!
//! The **source of truth is the raw text** (the raw-text editor edits it
//! directly); the parsed [`ReportFlow`] is derived on demand via [`Report::flow`].
//! This keeps editing cheap and matches how `.report` files round-trip through
//! [`super::parser::parse_flow`] / [`ReportFlow::to_text`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::git_remote::GitOrigin;

use super::flow::ReportFlow;
use super::parser::{ParseError, parse_flow};

/// The default body inserted into a brand-new scratch report — a minimal,
/// self-documenting skeleton the user can edit. Kept intentionally tiny; the
/// `collection:` line is left blank so validation prompts the user to BIND one.
pub const SCRATCH_TEMPLATE: &str = "\
# collection:
# output: csv

";

static NEXT_REPORT_ID: AtomicU64 = AtomicU64::new(1);

/// A process-unique id for a report (tab identity; not persisted).
pub fn next_report_id() -> u64 {
    NEXT_REPORT_ID.fetch_add(1, Ordering::Relaxed)
}

/// An in-memory `.report` document.
#[derive(Debug, Clone)]
pub struct Report {
    /// Process-unique identity (mirrors `Collection::id`).
    pub id: u64,
    /// Display name (tab title). Derived from the header `name:` directive or
    /// the file stem on load, but independent thereafter.
    pub name: String,
    /// The `.report` source (comment header + flow body) — the editing source of
    /// truth. Parsed on demand via [`Self::flow`].
    pub text: String,
    /// Local file path this report was last loaded from / saved to, if any.
    /// `None` for an unsaved scratch report.
    pub path: Option<PathBuf>,
    /// Git provenance, if the report was loaded from / saved to a remote.
    pub git_origin: Option<GitOrigin>,
    /// Whether `text` has unsaved edits (set on edit, cleared on save/load).
    pub dirty: bool,
}

impl Report {
    /// A brand-new, unsaved scratch report with the starter [`SCRATCH_TEMPLATE`].
    pub fn scratch(name: impl Into<String>) -> Self {
        Self {
            id: next_report_id(),
            name: name.into(),
            text: SCRATCH_TEMPLATE.to_string(),
            path: None,
            git_origin: None,
            dirty: false,
        }
    }

    /// Build a report from already-loaded source text, deriving its display name
    /// from the header `name:` directive, else `fallback_name`.
    pub fn from_text(fallback_name: impl Into<String>, text: impl Into<String>) -> Self {
        let text = text.into();
        let name = header_name(&text).unwrap_or_else(|| fallback_name.into());
        Self {
            id: next_report_id(),
            name,
            text,
            path: None,
            git_origin: None,
            dirty: false,
        }
    }

    /// Parse the current source into a [`ReportFlow`] (used for validation,
    /// dry-run expansion, and running). Errors are surfaced to the user.
    pub fn flow(&self) -> Result<ReportFlow, ParseError> {
        parse_flow(&self.text)
    }

    /// The bound collection reference from the header `collection:` directive, if
    /// any. Empty/absent → `None` (validation reports it as unbound). Parsing is
    /// tolerant: a malformed body still yields whatever header directives parsed.
    pub fn collection_ref(&self) -> Option<String> {
        header_directive(&self.text, "collection").filter(|v| !v.is_empty())
    }

    /// Replace the source text, marking the report dirty and refreshing the
    /// display name from the (possibly changed) header `name:` directive.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        if let Some(n) = header_name(&self.text) {
            self.name = n;
        }
        self.dirty = true;
    }

    /// Load a `.report` from a local file. The display name comes from the header
    /// `name:` directive, else the file stem.
    pub fn load_local(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let fallback = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "report".into());
        let name = header_name(&text).unwrap_or(fallback);
        Ok(Self {
            id: next_report_id(),
            name,
            text,
            path: Some(path.to_path_buf()),
            git_origin: None,
            dirty: false,
        })
    }

    /// Save the source to a local file, recording it as the report's path and
    /// clearing the dirty flag.
    pub fn save_local(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        std::fs::write(path, &self.text).map_err(|e| format!("{}: {e}", path.display()))?;
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        Ok(())
    }
}

/// The `name:` header directive value (trimmed, non-empty), if present.
fn header_name(text: &str) -> Option<String> {
    header_directive(text, "name").filter(|v| !v.is_empty())
}

/// Scan the comment header for a `# key: value` directive, without a full parse
/// (so a body with syntax errors still yields its header). Only scans the
/// leading run of comment/blank lines — the header ends at the first statement,
/// matching [`super::parser`].
fn header_directive(text: &str, key: &str) -> Option<String> {
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let Some(rest) = line.strip_prefix('#') else {
            // First non-comment, non-blank line — the header is over.
            break;
        };
        if let Some((k, v)) = rest.split_once(':')
            && k.trim().eq_ignore_ascii_case(key)
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_is_unsaved_and_uses_the_template() {
        let r = Report::scratch("Untitled");
        assert_eq!(r.name, "Untitled");
        assert!(r.path.is_none());
        assert!(!r.dirty);
        assert!(r.text.contains("# collection:"));
        // An empty `collection:` is treated as unbound.
        assert_eq!(r.collection_ref(), None);
    }

    #[test]
    fn name_and_collection_come_from_the_header() {
        let text = "# name: Nightly DFA\n# collection: ./dfa.hurl\n\nREQUEST Oauth\n";
        let r = Report::from_text("fallback", text);
        assert_eq!(r.name, "Nightly DFA");
        assert_eq!(r.collection_ref(), Some("./dfa.hurl".to_string()));
    }

    #[test]
    fn from_text_falls_back_when_no_name_directive() {
        let r = Report::from_text("fallback", "# collection: c.hurl\nREQUEST x\n");
        assert_eq!(r.name, "fallback");
    }

    #[test]
    fn header_is_readable_even_with_a_malformed_body() {
        // A body that won't parse must still yield the header directives.
        let text = "# collection: c.hurl\nFOR X IN\n";
        let r = Report::from_text("fallback", text);
        assert!(r.flow().is_err(), "body is intentionally malformed");
        assert_eq!(r.collection_ref(), Some("c.hurl".to_string()));
    }

    #[test]
    fn set_text_marks_dirty_and_refreshes_name() {
        let mut r = Report::scratch("Untitled");
        r.set_text("# name: Renamed\n# collection: c.hurl\nREQUEST x\n");
        assert!(r.dirty);
        assert_eq!(r.name, "Renamed");
    }

    #[test]
    fn local_save_then_load_round_trips() {
        let dir = std::env::temp_dir().join(format!("pb-report-{}", next_report_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nightly.report");

        let mut r = Report::from_text("nightly", "# name: N\n# collection: c.hurl\nREQUEST x\n");
        r.dirty = true;
        r.save_local(&path).unwrap();
        assert!(!r.dirty, "save clears the dirty flag");
        assert_eq!(r.path.as_deref(), Some(path.as_path()));

        let loaded = Report::load_local(&path).unwrap();
        assert_eq!(loaded.name, "N");
        assert_eq!(loaded.text, r.text);
        assert_eq!(loaded.path.as_deref(), Some(path.as_path()));
        assert!(!loaded.dirty);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_local_derives_name_from_file_stem_without_name_directive() {
        let dir = std::env::temp_dir().join(format!("pb-report-{}", next_report_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("my-report.report");
        std::fs::write(&path, "# collection: c.hurl\nREQUEST x\n").unwrap();

        let loaded = Report::load_local(&path).unwrap();
        assert_eq!(loaded.name, "my-report");

        std::fs::remove_dir_all(&dir).ok();
    }
}
