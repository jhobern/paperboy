//! The row model behind the Environments panel, shared by both front-ends so
//! the terminal UI and the GUI list the same things in the same order.
//!
//! The panel is not simply "the loaded environments". When a Workspace folder
//! is open it also lists every environment *file* in that folder, whether or
//! not it has been opened yet — a workspace of a few hundred exports is
//! otherwise invisible until each file is hunted down in the tree and opened
//! one at a time. A workspace file that *has* been loaded is shown once, as
//! the loaded environment it became, rather than twice.
//!
//! Both kinds are filterable by name, which is the point of the whole exercise:
//! with hundreds of environments, finding the one you want by scrolling is
//! hopeless.

use std::path::{Path, PathBuf};

use crate::environment::Environment;

/// Which source(s) the Environments panel should list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum EnvSource {
    #[default]
    Both,
    Global,
    Workspace,
}

impl EnvSource {
    pub fn next(self) -> Self {
        match self {
            Self::Both => Self::Global,
            Self::Global => Self::Workspace,
            Self::Workspace => Self::Both,
        }
    }

    fn includes_workspace(self) -> bool {
        matches!(self, Self::Both | Self::Workspace)
    }

    fn includes_global(self) -> bool {
        matches!(self, Self::Both | Self::Global)
    }
}

/// What a panel row points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvRowKind {
    /// A loaded environment, by [`Environment::id`]. Everything the panel can
    /// do — activate, link, rename, edit, save, delete — applies to these.
    Loaded(u64),
    /// An environment file in the open workspace that hasn't been loaded yet.
    /// Selecting it loads it, at which point it becomes a `Loaded` row.
    File(PathBuf),
}

/// One row of the Environments panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvRow {
    /// The name to display: the environment's name, or an unloaded file's stem.
    pub name: String,
    pub kind: EnvRowKind,
    /// True when this row is (or would be) an environment from the open
    /// Workspace folder, as opposed to one loaded from anywhere else. Marked in
    /// both front-ends so the two sources are distinguishable at a glance.
    pub workspace: bool,
}

impl EnvRow {
    /// The loaded environment's id, or `None` for a workspace file that hasn't
    /// been opened yet.
    pub fn env_id(&self) -> Option<u64> {
        match self.kind {
            EnvRowKind::Loaded(id) => Some(id),
            EnvRowKind::File(_) => None,
        }
    }

    /// The workspace file this row would load, or `None` if it is already
    /// loaded.
    pub fn file(&self) -> Option<&Path> {
        match &self.kind {
            EnvRowKind::File(p) => Some(p.as_path()),
            EnvRowKind::Loaded(_) => None,
        }
    }
}

/// The name an unloaded environment file is listed under: its file stem, the
/// same name [`crate::session::Session::open_workspace_environment`] gives the
/// environment once it is loaded, so a row doesn't rename itself on opening.
pub fn file_display_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("env")
        .to_string()
}

/// Whether `name` matches `filter` — a case-insensitive substring, with an
/// empty filter matching everything.
pub fn matches(name: &str, filter: &str) -> bool {
    let filter = filter.trim();
    filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase())
}

/// Build the Environments panel's rows.
///
/// `workspace_files` is every environment file in the open Workspace folder
/// (see [`crate::collection::Collection::workspace_env_files`]), or empty when
/// no workspace is open. Workspace rows come first, in tree order, followed by the loaded
/// environments that didn't come from the workspace — so opening a workspace
/// doesn't reshuffle the environments already in the list.
///
/// A loaded environment is matched to a workspace file by its `path`, which is
/// how it is distinguished from an identically-named environment loaded from
/// elsewhere.
pub fn rows(
    envs: &[Environment],
    workspace_files: &[PathBuf],
    filter: &str,
    source: EnvSource,
) -> Vec<EnvRow> {
    let mut out = Vec::new();

    if source.includes_workspace() {
        for path in workspace_files {
            let loaded = envs.iter().find(|e| e.path.as_deref() == Some(path));
            let (name, kind) = match loaded {
                Some(env) => (env.name.clone(), EnvRowKind::Loaded(env.id)),
                None => (file_display_name(path), EnvRowKind::File(path.clone())),
            };
            if matches(&name, filter) {
                out.push(EnvRow {
                    name,
                    kind,
                    workspace: true,
                });
            }
        }
    }

    if source.includes_global() {
        for env in envs {
            let in_workspace = env
                .path
                .as_ref()
                .is_some_and(|p| workspace_files.contains(p));
            if in_workspace || !matches(&env.name, filter) {
                continue;
            }
            out.push(EnvRow {
                name: env.name.clone(),
                kind: EnvRowKind::Loaded(env.id),
                workspace: false,
            });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::parse_vars_pending;

    fn env(name: &str, path: Option<&str>) -> Environment {
        let (mut e, _) = parse_vars_pending(name.to_string(), "K=v");
        e.path = path.map(PathBuf::from);
        e
    }

    #[test]
    fn loaded_environments_list_when_no_workspace_is_open() {
        let envs = vec![env("staging", None), env("prod", None)];
        let rows = rows(&envs, &[], "", EnvSource::Both);
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["staging", "prod"]
        );
        assert!(rows.iter().all(|r| !r.workspace));
        assert_eq!(rows[0].env_id(), Some(envs[0].id));
    }

    /// A workspace file that hasn't been opened still gets a row, so the whole
    /// folder is browsable from the panel rather than only via the tree.
    #[test]
    fn unloaded_workspace_files_are_listed_alongside_loaded_environments() {
        let envs = vec![env("hand-made", None)];
        let files = vec![
            PathBuf::from("/ws/Prod AU.json"),
            PathBuf::from("/ws/dev.vars"),
        ];
        let rows = rows(&envs, &files, "", EnvSource::Both);
        assert_eq!(
            rows.iter()
                .map(|r| (r.name.as_str(), r.workspace, r.env_id().is_some()))
                .collect::<Vec<_>>(),
            vec![
                ("Prod AU", true, false),
                ("dev", true, false),
                ("hand-made", false, true),
            ],
            "workspace files come first, in tree order, then the loaded rest"
        );
        assert_eq!(rows[0].file(), Some(Path::new("/ws/Prod AU.json")));
    }

    /// Once opened, a workspace file is the loaded environment — it must not
    /// appear a second time in the global section.
    #[test]
    fn an_opened_workspace_file_is_listed_once_as_the_loaded_environment() {
        let envs = vec![env("dev", Some("/ws/dev.vars"))];
        let files = vec![PathBuf::from("/ws/dev.vars")];
        let rows = rows(&envs, &files, "", EnvSource::Both);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].workspace, "it is still a workspace environment");
        assert_eq!(rows[0].env_id(), Some(envs[0].id));
    }

    /// An environment loaded from outside the workspace keeps its own row even
    /// when it shares a name with one inside it.
    #[test]
    fn a_same_named_environment_from_elsewhere_is_not_folded_into_the_workspace_row() {
        let envs = vec![env("dev", Some("/elsewhere/dev.vars"))];
        let files = vec![PathBuf::from("/ws/dev.vars")];
        let rows = rows(&envs, &files, "", EnvSource::Both);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].workspace, rows[1].workspace), (true, false));
    }

    #[test]
    fn the_filter_matches_case_insensitively_on_any_part_of_the_name() {
        let envs = vec![env("Westpac Prod", None), env("Bendigo Staging", None)];
        let files = vec![PathBuf::from("/ws/Westpac NZ Staging.json")];

        let matched = rows(&envs, &files, "staging", EnvSource::Both);
        assert_eq!(
            matched.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["Westpac NZ Staging", "Bendigo Staging"]
        );

        assert_eq!(
            rows(&envs, &files, "  ", EnvSource::Both).len(),
            3,
            "a blank filter is no filter"
        );
        assert!(rows(&envs, &files, "zzz", EnvSource::Both).is_empty());
    }

    #[test]
    fn the_source_filter_limits_rows_to_global_workspace_or_both() {
        let envs = vec![
            env("hand-made", None),
            env("dev", Some("/ws/dev.vars")),
            env("outside-dev", Some("/elsewhere/dev.vars")),
        ];
        let files = vec![PathBuf::from("/ws/dev.vars")];

        assert_eq!(
            rows(&envs, &files, "", EnvSource::Both)
                .iter()
                .map(|r| (r.name.as_str(), r.workspace))
                .collect::<Vec<_>>(),
            vec![("dev", true), ("hand-made", false), ("outside-dev", false)]
        );
        assert_eq!(
            rows(&envs, &files, "", EnvSource::Workspace)
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            vec!["dev"]
        );
        assert_eq!(
            rows(&envs, &files, "", EnvSource::Global)
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            vec!["hand-made", "outside-dev"]
        );
    }

    #[test]
    fn the_source_filter_composes_with_the_name_filter() {
        let envs = vec![env("prod global", None), env("stage global", None)];
        let files = vec![
            PathBuf::from("/ws/prod workspace.vars"),
            PathBuf::from("/ws/stage workspace.vars"),
        ];

        assert_eq!(
            rows(&envs, &files, "prod", EnvSource::Workspace)
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            vec!["prod workspace"]
        );
        assert_eq!(
            rows(&envs, &files, "prod", EnvSource::Global)
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            vec!["prod global"]
        );
    }
}
