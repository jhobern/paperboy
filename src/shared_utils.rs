//! Small cross-cutting helpers that have no single obvious home module.

use std::path::Path;

/// The file stem of `path` (its name without an extension), or `fallback` when
/// the path has no usable stem. Accepts anything path-like (`&str`, `&Path`, …).
pub(crate) fn stem(path: impl AsRef<Path>, fallback: &str) -> String {
    path.as_ref()
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback.to_string())
}
