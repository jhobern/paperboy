//! Filesystem/data **producers** — the sources a `FOR … IN <producer>` loop
//! iterates. Everything that yields multiple items is a producer:
//! - `[ … ]`                literal (handled inline in [`super::run`]),
//! - `FILES "dir" [MATCH …]`  file paths (glob; `**` recurses),
//! - `FOLDERS "dir" [WITH r="glob", …]`  subfolders, one file per role,
//! - `TUPLES FROM "file"`   one tuple per CSV/TSV/JSON row,
//! - `ZIP(a, b, …)`         positional N-tuples (equal length required).
//!
//! Each yields an ordered list of [`ProducerItem`]s. An item carries its
//! *positional* `values` (matched against the loop's destructuring pattern and
//! forming the row key) and any *named* fields (FOLDERS roles, CSV headers) that
//! bind directly by name — so metadata columns are never lost to arity.
//!
//! These helpers are pure and front-end agnostic; [`super::run::Exec`]
//! orchestrates them (resolving `LIST` names, substituting `{{var}}`s in paths,
//! applying the `# root:` base directory).

use std::path::{Path, PathBuf};

/// One item produced by a loop source: its positional `values` (for pattern
/// destructuring + row key) and any `named` fields that bind by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProducerItem {
    pub values: Vec<String>,
    pub named: Vec<(String, String)>,
}

impl ProducerItem {
    /// A single positional value (the common arity-1 case: `FILES`, `FOLDERS`).
    pub fn scalar(v: impl Into<String>) -> Self {
        ProducerItem {
            values: vec![v.into()],
            named: Vec::new(),
        }
    }
}

/// Resolve a possibly-relative path against `root` (the report's directory or
/// the `# root:` override). Absolute paths are returned unchanged.
pub fn resolve_path(root: Option<&Path>, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else if let Some(root) = root {
        root.join(path)
    } else {
        path.to_path_buf()
    }
}

/// Match a glob (`*` = any run, `?` = one char, otherwise literal) against a
/// single filename (no path separators). Case-sensitive, matching the shell.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    fn m(p: &[u8], n: &[u8]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some(b'*') => {
                // Collapse consecutive `*` and try every split.
                m(&p[1..], n) || (!n.is_empty() && m(p, &n[1..]))
            }
            Some(b'?') => !n.is_empty() && m(&p[1..], &n[1..]),
            Some(&c) => !n.is_empty() && n[0] == c && m(&p[1..], &n[1..]),
        }
    }
    m(pattern.as_bytes(), name.as_bytes())
}

/// List files under `dir` for a `FILES "dir" [MATCH glob]` producer. When the
/// glob contains `**` the walk recurses into subdirectories; the filename is
/// matched against the glob's last path segment. Results are sorted for
/// deterministic, reproducible ordering.
pub fn list_files(dir: &Path, glob: Option<&str>) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Err(format!("directory not found: {}", dir.display()));
    }
    let recursive = glob.is_some_and(|g| g.contains("**"));
    let file_pat: Option<String> = glob.map(|g| g.rsplit('/').next().unwrap_or(g).to_string());
    let mut out = Vec::new();
    collect_files(dir, recursive, file_pat.as_deref(), &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_files(
    dir: &Path,
    recursive: bool,
    file_pat: Option<&str>,
    out: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                collect_files(&path, recursive, file_pat, out)?;
            }
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let matches = match file_pat {
            Some(pat) => glob_match(pat, &name),
            None => true,
        };
        if matches {
            out.push(path);
        }
    }
    Ok(())
}

/// List immediate subfolders of `dir` (sorted) for a `FOLDERS` producer.
pub fn list_folders(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Err(format!("directory not found: {}", dir.display()));
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// For one `FOLDERS … WITH role="glob", …` subfolder, resolve each role to the
/// single file in the folder matching its glob. Exactly one match per role is
/// required (0 or >1 is an error, so a mis-shaped group fails loudly).
pub fn folder_roles(
    folder: &Path,
    roles: &[(String, String)],
) -> Result<Vec<(String, String)>, String> {
    let mut named = Vec::new();
    for (role, glob) in roles {
        let mut matches = list_files(folder, Some(glob))?;
        match matches.len() {
            1 => named.push((
                role.clone(),
                matches.remove(0).to_string_lossy().into_owned(),
            )),
            0 => {
                return Err(format!(
                    "role '{role}' matched no file in {} (glob {glob:?})",
                    folder.display()
                ));
            }
            n => {
                return Err(format!(
                    "role '{role}' matched {n} files in {} (glob {glob:?}); expected exactly one",
                    folder.display()
                ));
            }
        }
    }
    Ok(named)
}

/// Read a `TUPLES FROM "file"` manifest into items. `.csv`/`.tsv` treat the
/// first row as headers (each item's `named` fields), `.json` accepts an array
/// of arrays (positional) or an array of objects (named + positional in key
/// order). Every item exposes both positional `values` and, where available,
/// `named` fields.
pub fn read_tuples(path: &Path) -> Result<Vec<ProducerItem>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "json" => read_tuples_json(&text),
        "tsv" => Ok(read_delimited(&text, '\t')),
        _ => Ok(read_delimited(&text, ',')),
    }
}

fn read_delimited(text: &str, delim: char) -> Vec<ProducerItem> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let headers: Vec<String> = match lines.next() {
        Some(h) => split_delimited(h, delim),
        None => return Vec::new(),
    };
    lines
        .map(|line| {
            let values = split_delimited(line, delim);
            let named = headers
                .iter()
                .cloned()
                .zip(values.iter().cloned())
                .collect();
            ProducerItem { values, named }
        })
        .collect()
}

/// Split one delimited line, honouring double-quoted fields (RFC 4180 style:
/// `""` is an escaped quote inside a quoted field).
fn split_delimited(line: &str, delim: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quote && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => in_quote = !in_quote,
            _ if c == delim && !in_quote => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter().map(|s| s.trim().to_string()).collect()
}

fn read_tuples_json(text: &str) -> Result<Vec<ProducerItem>, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("invalid JSON manifest: {e}"))?;
    let arr = value
        .as_array()
        .ok_or("JSON manifest must be an array of rows")?;
    let mut items = Vec::new();
    for row in arr {
        match row {
            serde_json::Value::Array(cells) => items.push(ProducerItem {
                values: cells.iter().map(json_cell).collect(),
                named: Vec::new(),
            }),
            serde_json::Value::Object(map) => {
                let named: Vec<(String, String)> =
                    map.iter().map(|(k, v)| (k.clone(), json_cell(v))).collect();
                let values = named.iter().map(|(_, v)| v.clone()).collect();
                items.push(ProducerItem { values, named });
            }
            other => items.push(ProducerItem::scalar(json_cell(other))),
        }
    }
    Ok(items)
}

/// Stringify a JSON cell for a manifest: strings unwrapped, everything else
/// compact JSON.
fn json_cell(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Zip already-expanded producer item-lists into positional N-tuples: item `i`
/// concatenates each input's `values` and merges their `named` fields. Equal
/// length is required; a mismatch is reported and the shortest length is used.
pub fn zip_items(lists: Vec<Vec<ProducerItem>>) -> Result<Vec<ProducerItem>, String> {
    if lists.is_empty() {
        return Ok(Vec::new());
    }
    let min = lists.iter().map(Vec::len).min().unwrap_or(0);
    let max = lists.iter().map(Vec::len).max().unwrap_or(0);
    let mut items = Vec::with_capacity(min);
    for i in 0..min {
        let mut values = Vec::new();
        let mut named = Vec::new();
        for list in &lists {
            values.extend(list[i].values.iter().cloned());
            named.extend(list[i].named.iter().cloned());
        }
        items.push(ProducerItem { values, named });
    }
    if min != max {
        return Err(format!(
            "ZIP inputs have unequal lengths ({min}..{max}); zipped to {min}"
        ));
    }
    Ok(items)
}

/// Concatenate already-expanded producer item-lists end-to-end into one longer
/// stream (the runtime side of `CONCAT(a, b, …)`). Unlike [`zip_items`], the
/// inputs need not be equal length — they are appended in order — but every
/// item must share the same positional arity (so the loop's destructuring
/// pattern and row key stay well-defined). A mismatch is a hard error. Each
/// item keeps its own `named` fields; the output layer unions differing field
/// sets across inputs (blank where absent).
pub fn concat_items(lists: Vec<Vec<ProducerItem>>) -> Result<Vec<ProducerItem>, String> {
    let mut items: Vec<ProducerItem> = Vec::new();
    let mut arity: Option<usize> = None;
    for list in lists {
        for item in list {
            match arity {
                None => arity = Some(item.values.len()),
                Some(a) if a != item.values.len() => {
                    return Err(format!(
                        "CONCAT inputs have mismatched arity ({a} vs {}); all inputs \
                         must yield the same number of values per item",
                        item.values.len()
                    ));
                }
                Some(_) => {}
            }
            items.push(item);
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "paperboy_prod_{tag}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn glob_matches_star_and_question() {
        assert!(glob_match("*.jpg", "a.jpg"));
        assert!(glob_match("*.jpg", ".jpg"));
        assert!(!glob_match("*.jpg", "a.png"));
        assert!(glob_match("img_?.png", "img_3.png"));
        assert!(!glob_match("img_?.png", "img_33.png"));
        assert!(glob_match("*", "anything"));
    }

    #[test]
    fn list_files_filters_and_sorts() {
        let d = tmpdir("files");
        for n in ["b.jpg", "a.jpg", "c.png"] {
            fs::write(d.join(n), "x").unwrap();
        }
        let files = list_files(&d, Some("*.jpg")).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.jpg", "b.jpg"]);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn list_files_recurses_on_double_star() {
        let d = tmpdir("rec");
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("top.jpg"), "x").unwrap();
        fs::write(d.join("sub/deep.jpg"), "x").unwrap();
        let files = list_files(&d, Some("**/*.jpg")).unwrap();
        assert_eq!(files.len(), 2);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn folder_roles_require_exactly_one_match() {
        let d = tmpdir("roles");
        fs::write(d.join("scan_front.jpg"), "x").unwrap();
        fs::write(d.join("scan_back.jpg"), "x").unwrap();
        let named = folder_roles(
            &d,
            &[
                ("FRONT".into(), "*_front.jpg".into()),
                ("BACK".into(), "*_back.jpg".into()),
            ],
        )
        .unwrap();
        assert_eq!(named[0].0, "FRONT");
        assert!(named[0].1.ends_with("scan_front.jpg"));
        // A role matching nothing is an error.
        let err = folder_roles(&d, &[("LABEL".into(), "*.pdf".into())]).unwrap_err();
        assert!(err.contains("LABEL"));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn read_tuples_csv_exposes_values_and_headers() {
        let d = tmpdir("csv");
        let f = d.join("docs.csv");
        fs::write(&f, "front,back\nf1.jpg,b1.jpg\nf2.jpg,b2.jpg\n").unwrap();
        let items = read_tuples(&f).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].values, vec!["f1.jpg", "b1.jpg"]);
        assert_eq!(
            items[0].named,
            vec![
                ("front".into(), "f1.jpg".into()),
                ("back".into(), "b1.jpg".into())
            ]
        );
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn read_tuples_json_array_of_objects() {
        let d = tmpdir("json");
        let f = d.join("docs.json");
        fs::write(&f, r#"[{"front":"f1","back":"b1"}]"#).unwrap();
        let items = read_tuples(&f).unwrap();
        assert_eq!(items[0].named.len(), 2);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn zip_requires_equal_lengths() {
        let a = vec![ProducerItem::scalar("a1"), ProducerItem::scalar("a2")];
        let b = vec![ProducerItem::scalar("b1"), ProducerItem::scalar("b2")];
        let z = zip_items(vec![a, b]).unwrap();
        assert_eq!(z[0].values, vec!["a1", "b1"]);
        let short = vec![ProducerItem::scalar("x")];
        let long = vec![ProducerItem::scalar("y1"), ProducerItem::scalar("y2")];
        assert!(zip_items(vec![short, long]).is_err());
    }

    #[test]
    fn concat_appends_items_in_order() {
        let a = vec![ProducerItem::scalar("a1"), ProducerItem::scalar("a2")];
        let b = vec![ProducerItem::scalar("b1")];
        let c = concat_items(vec![a, b]).unwrap();
        let flat: Vec<&str> = c.iter().map(|i| i.values[0].as_str()).collect();
        assert_eq!(flat, vec!["a1", "a2", "b1"]);
    }

    #[test]
    fn concat_allows_unequal_lengths_and_empty_inputs() {
        let a = vec![ProducerItem::scalar("only")];
        let empty: Vec<ProducerItem> = Vec::new();
        let c = concat_items(vec![empty.clone(), a, empty]).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].values, vec!["only"]);
    }

    #[test]
    fn concat_rejects_mismatched_arity() {
        let ones = vec![ProducerItem::scalar("a")];
        let pairs = vec![ProducerItem {
            values: vec!["x".into(), "y".into()],
            named: Vec::new(),
        }];
        assert!(concat_items(vec![ones, pairs]).is_err());
    }
}
