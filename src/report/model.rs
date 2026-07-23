//! The output model a PaperTrail run produces: a **wide** table of rows, each a
//! map of column → cell, plus the machinery that turns REPORT statements and the
//! `columns:` directive into concrete, ordered columns.
//!
//! The model is front-end agnostic: [`super::run`] fills it, [`super::writer`]
//! serializes it (CSV in v1), and the TUI grid renders it. Rows carry both the
//! REPORT-produced `cells` (namespaced, e.g. `proc.status`) and a snapshot of the
//! in-scope `vars` at emission, so the `columns:` directive can reference either
//! a produced cell or a raw loop/assign variable (`FILE`, `TARGET`, …).

use std::collections::HashMap;

use super::flow::Header;

/// The reserved column name that carries the ENVS comparison target (the
/// environment name for the row). Excluded from the row *key* (it is the
/// comparison axis, not a row axis) but available as a column source.
pub const TARGET_COLUMN: &str = "TARGET";

/// One output row: one innermost-loop iteration (or the single row of a
/// loop-free flow). A row is created at *plan* time (see the streaming/slot
/// model) and its cells are filled as the run progresses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportRow {
    /// REPORT-produced cells, keyed by resolved (possibly namespaced) column
    /// name — `proc.status`, `proc.Time`, `note`, `FILE`, …
    pub cells: HashMap<String, String>,
    /// Snapshot of the in-scope loop/assign variables when the row was emitted,
    /// so `columns:` can reference a raw variable that was never `REPORT`ed.
    pub vars: HashMap<String, String>,
    /// The row key: the in-scope FILES/list loop-variable values (ENVS
    /// excluded), in binding order. Two rows with the same key across different
    /// ENVS targets are the same logical row (the P11 comparison axis).
    pub key: Vec<String>,
    /// The ENVS target (environment name) this row was produced under, if the
    /// flow loops over `ENVS`. `None` for a flow with no `ENVS` loop.
    pub target: Option<String>,
}

/// A whole run's output: the rows plus the first-seen order of produced column
/// keys (the default column order when there is no `columns:` directive), the
/// effective no-match marker, and any diagnostics collected while running.
#[derive(Debug, Clone, Default)]
pub struct ReportResult {
    pub rows: Vec<ReportRow>,
    /// Produced cell-column keys in first-seen order — the default column set.
    pub column_order: Vec<String>,
    /// The table-wide marker rendered for a cell that resolved to nothing (the
    /// effective `PRELUDE_NO_MATCH_MARKER`, empty by default). Applied once, at
    /// render time, by [`OutputColumn::value`].
    pub no_match_marker: String,
    /// Non-fatal problems encountered during the run (a request that failed, a
    /// producer that matched nothing, …). Every issue still leaves a row.
    pub errors: Vec<String>,
}

impl ReportResult {
    /// Record a produced column key, preserving first-seen order (so the default
    /// CSV column order is stable and matches authoring order).
    pub fn note_column(&mut self, key: &str) {
        if !self.column_order.iter().any(|c| c == key) {
            self.column_order.push(key.to_string());
        }
    }

    /// The resolved output columns: `(header, [source keys])`. Driven by the
    /// `columns:` header directive when present (honouring `|` coalescing and
    /// `AS` renames), else the produced columns in first-seen order (each its
    /// own single-source column with an identity header).
    pub fn resolved_columns(&self, header: &Header) -> Vec<OutputColumn> {
        match header.columns() {
            Some(spec) => parse_columns(spec),
            None => self
                .column_order
                .iter()
                .map(|k| OutputColumn {
                    header: k.clone(),
                    sources: vec![k.clone()],
                })
                .collect(),
        }
    }
}

/// One resolved output column: a display `header` and the ordered `sources` to
/// coalesce (first non-empty wins) when producing its cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputColumn {
    pub header: String,
    pub sources: Vec<String>,
}

impl OutputColumn {
    /// The cell value for this column in `row`, coalescing sources left-to-right
    /// (first non-empty wins). A source resolves against the row's produced
    /// `cells` first, then its variable snapshot (so `columns: FILE` works even
    /// without an explicit `REPORT (FILE)`), then the special `TARGET`. Returns
    /// `no_match` when nothing resolves.
    pub fn value(&self, row: &ReportRow, no_match: &str) -> String {
        for src in &self.sources {
            if let Some(v) = row.cells.get(src)
                && !v.is_empty()
            {
                return v.clone();
            }
            if let Some(v) = row.vars.get(src)
                && !v.is_empty()
            {
                return v.clone();
            }
            if src == TARGET_COLUMN
                && let Some(t) = &row.target
                && !t.is_empty()
            {
                return t.clone();
            }
        }
        no_match.to_string()
    }
}

/// Parse the `columns:` directive value into ordered [`OutputColumn`]s.
///
/// Grammar (see `docs/reports/02-grammar.md`):
/// `columns := column-spec (',' column-spec)*`,
/// `column-spec := source ('|' source)* ['AS' name]`. A quoted `AS` name may
/// contain spaces/commas; sources are bare `IDENT('.'IDENT)?` tokens.
pub fn parse_columns(spec: &str) -> Vec<OutputColumn> {
    split_top_level(spec, ',')
        .into_iter()
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            // Split an optional trailing ` AS <name>` (case-insensitive).
            let (sources_part, header) = split_as(part);
            let sources: Vec<String> = sources_part
                .split('|')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if sources.is_empty() {
                return None;
            }
            let header = header.unwrap_or_else(|| sources[0].clone());
            Some(OutputColumn { header, sources })
        })
        .collect()
}

/// Split `part` on a case-insensitive ` AS ` boundary that is outside quotes,
/// returning `(sources, Some(header))` when an `AS` clause is present (the
/// header is unquoted), else `(part, None)`.
fn split_as(part: &str) -> (&str, Option<String>) {
    let bytes = part.as_bytes();
    let mut in_quote = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '"' {
            in_quote = !in_quote;
            i += 1;
            continue;
        }
        if !in_quote
            && (c == 'A' || c == 'a')
            && i + 3 <= bytes.len()
            && part[i..].len() >= 3
            && part[i..i + 2].eq_ignore_ascii_case("as")
            && i > 0
            && bytes[i - 1].is_ascii_whitespace()
            && bytes.get(i + 2).is_some_and(|b| b.is_ascii_whitespace())
        {
            let sources = part[..i].trim();
            let header = unquote(part[i + 2..].trim());
            return (sources, Some(header));
        }
        i += 1;
    }
    (part, None)
}

/// Split on `sep` at the top level (ignoring `sep` inside double quotes).
fn split_top_level(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                cur.push(c);
            }
            _ if c == sep && !in_quote => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Strip one layer of surrounding double quotes, if present.
fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[(&str, &str)], vars: &[(&str, &str)], target: Option<&str>) -> ReportRow {
        ReportRow {
            cells: cells
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            key: vec![],
            target: target.map(str::to_string),
        }
    }

    #[test]
    fn default_columns_follow_first_seen_order() {
        let mut res = ReportResult::default();
        res.note_column("proc.status");
        res.note_column("proc.Time");
        res.note_column("proc.status"); // dup ignored
        let cols = res.resolved_columns(&Header::default());
        let headers: Vec<&str> = cols.iter().map(|c| c.header.as_str()).collect();
        assert_eq!(headers, vec!["proc.status", "proc.Time"]);
    }

    #[test]
    fn columns_directive_renames_and_reorders() {
        let cols = parse_columns("FILE as Name, proc.status as Status, proc.Time as Time");
        assert_eq!(cols.len(), 3);
        assert_eq!(cols[0].header, "Name");
        assert_eq!(cols[0].sources, vec!["FILE"]);
        assert_eq!(cols[1].header, "Status");
        assert_eq!(cols[2].header, "Time");
    }

    #[test]
    fn columns_directive_supports_quoted_headers_with_spaces() {
        let cols = parse_columns("proc.Response as \"Main Results\"");
        assert_eq!(cols[0].header, "Main Results");
        assert_eq!(cols[0].sources, vec!["proc.Response"]);
    }

    #[test]
    fn columns_directive_coalesces_sources() {
        let cols = parse_columns("a.status | b.status as Status");
        assert_eq!(cols[0].header, "Status");
        assert_eq!(cols[0].sources, vec!["a.status", "b.status"]);
    }

    #[test]
    fn coalesce_takes_first_non_empty_source() {
        let col = OutputColumn {
            header: "Status".into(),
            sources: vec!["a.status".into(), "b.status".into()],
        };
        let r = row(&[("a.status", ""), ("b.status", "ok")], &[], None);
        assert_eq!(col.value(&r, "-"), "ok");
    }

    #[test]
    fn value_falls_back_to_vars_then_no_match_marker() {
        let col = OutputColumn {
            header: "Name".into(),
            sources: vec!["FILE".into()],
        };
        let r = row(&[], &[("FILE", "a.jpg")], None);
        assert_eq!(col.value(&r, "∅"), "a.jpg");
        let empty = row(&[], &[], None);
        assert_eq!(col.value(&empty, "∅"), "∅");
    }

    #[test]
    fn target_is_available_as_a_column_source() {
        let col = OutputColumn {
            header: "Env".into(),
            sources: vec![TARGET_COLUMN.to_string()],
        };
        let r = row(&[], &[], Some("staging-au"));
        assert_eq!(col.value(&r, "-"), "staging-au");
    }
}
