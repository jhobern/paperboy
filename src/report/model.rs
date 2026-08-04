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
    /// The **structural path** to the row: one `(loop node index within its
    /// block, iteration index)` pair per enclosing loop. Unlike [`key`](Self::key)
    /// (which holds loop *values* and can repeat), the path is guaranteed unique
    /// and, sorted lexicographically, reproduces the canonical row order — so a
    /// streaming front-end can match a live row to its pre-built grid slot even
    /// when a `PARALLEL` loop delivers rows out of order. Empty for the single
    /// row of a loop-free flow. Not part of the persisted/exported model (it's a
    /// run-time coordinate); baseline-snapshot rows carry an empty path.
    pub path: Vec<(usize, usize)>,
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
    /// Summary statistics requested per output-column *header* by a
    /// `REPORT … AS <header> STATISTICS(…)` statement. Merged into the resolved
    /// columns at render time (a `columns:` directive's own `STATISTICS(…)`
    /// takes precedence for a column that carries both). Empty by default.
    pub column_stats: std::collections::HashMap<String, Vec<StatKind>>,
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
        let mut columns = match header.columns() {
            Some(spec) => parse_columns(spec),
            None => self
                .column_order
                .iter()
                .map(|k| OutputColumn {
                    header: k.clone(),
                    sources: vec![k.clone()],
                    stats: Vec::new(),
                })
                .collect(),
        };
        // Merge in per-header statistics requested by `REPORT … STATISTICS(…)`
        // statements — but never override stats a `columns:` spec set inline.
        if !self.column_stats.is_empty() {
            for col in &mut columns {
                if col.stats.is_empty()
                    && let Some(stats) = self.column_stats.get(&col.header)
                {
                    col.stats = stats.clone();
                }
            }
        }
        columns
    }

    /// The summary rows to append after the data rows: one row per requested
    /// non-distribution statistic (with a value only in each column that asked
    /// for it), then, for every column that requested `DISTRIBUTION`, one row
    /// per distinct value carrying its count. The leading (first-column) cell of
    /// each row holds the row's label unless the first column itself carries the
    /// statistic's value. Returns an empty vec when no column requested stats.
    pub fn summary_rows(&self, columns: &[OutputColumn]) -> Vec<SummaryRow> {
        if columns.iter().all(|c| c.stats.is_empty()) {
            return Vec::new();
        }
        // The coalesced, render-ready value list for a column (skipping empties
        // and the no-match marker) — what the statistic is computed over.
        let column_values = |col: &OutputColumn| -> Vec<String> {
            self.rows
                .iter()
                .map(|row| col.value(row, &self.no_match_marker))
                .filter(|v| !v.trim().is_empty() && *v != self.no_match_marker)
                .collect()
        };

        let mut out = Vec::new();
        for stat in StatKind::SUMMARY_ORDER {
            let requested: Vec<usize> = columns
                .iter()
                .enumerate()
                .filter(|(_, c)| c.stats.contains(&stat))
                .map(|(i, _)| i)
                .collect();
            if requested.is_empty() {
                continue;
            }
            let mut cells = vec![None; columns.len()];
            for &ci in &requested {
                let values = column_values(&columns[ci]);
                let numeric = column_numeric(&values);
                if let Some(text) = compute_stat(stat, &values) {
                    cells[ci] = Some(StatValue {
                        text,
                        stat,
                        numeric,
                        match_value: None,
                    });
                }
            }
            if cells.iter().any(Option::is_some) {
                out.push(SummaryRow {
                    label: stat.label().to_string(),
                    cells,
                });
            }
        }
        // Distribution: one row per distinct value, per requesting column.
        for (ci, col) in columns.iter().enumerate() {
            if !col.stats.contains(&StatKind::Distribution) {
                continue;
            }
            for (value, count) in distinct_counts(&column_values(col)) {
                let mut cells = vec![None; columns.len()];
                cells[ci] = Some(StatValue {
                    text: count.to_string(),
                    stat: StatKind::Distribution,
                    numeric: true,
                    match_value: Some(value.clone()),
                });
                out.push(SummaryRow {
                    label: format!("{} = {}", col.header, value),
                    cells,
                });
            }
        }
        out
    }
}

/// A summary statistic requested for an output column via `STATISTICS(…)` (on a
/// `REPORT … AS …` statement or a `columns:` column-spec). Numeric statistics
/// require the column to be numeric; `Mode`/`Count`/`Distribution` work on any
/// column. Rendered as extra summary rows appended after the data (see
/// [`ReportResult::summary_rows`]); the xlsx writer turns the numeric ones into
/// live spreadsheet formulas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatKind {
    Mean,
    Median,
    Mode,
    Min,
    Max,
    Sum,
    Count,
    StdDev,
    /// The count of each distinct value — one summary row per value. Meant for
    /// categorical columns (e.g. a verdict that is only ever "Low"/"High").
    Distribution,
}

impl StatKind {
    /// The order summary rows are emitted in (Distribution is handled
    /// separately, per column, so it is not listed here).
    pub const SUMMARY_ORDER: [StatKind; 8] = [
        StatKind::Count,
        StatKind::Sum,
        StatKind::Mean,
        StatKind::Median,
        StatKind::Mode,
        StatKind::Min,
        StatKind::Max,
        StatKind::StdDev,
    ];

    /// Parse a `STATISTICS(…)` keyword (case-insensitive), accepting a few
    /// common aliases (`AVG`, `STDEV`, `DIST`).
    pub fn parse(s: &str) -> Option<StatKind> {
        match s.trim().to_ascii_uppercase().as_str() {
            "MEAN" | "AVG" | "AVERAGE" => Some(StatKind::Mean),
            "MEDIAN" => Some(StatKind::Median),
            "MODE" => Some(StatKind::Mode),
            "MIN" | "MINIMUM" => Some(StatKind::Min),
            "MAX" | "MAXIMUM" => Some(StatKind::Max),
            "SUM" | "TOTAL" => Some(StatKind::Sum),
            "COUNT" => Some(StatKind::Count),
            "STDDEV" | "STDEV" | "STD" => Some(StatKind::StdDev),
            "DISTRIBUTION" | "DIST" => Some(StatKind::Distribution),
            _ => None,
        }
    }

    /// The canonical keyword used when serializing back to report source.
    pub fn keyword(self) -> &'static str {
        match self {
            StatKind::Mean => "MEAN",
            StatKind::Median => "MEDIAN",
            StatKind::Mode => "MODE",
            StatKind::Min => "MIN",
            StatKind::Max => "MAX",
            StatKind::Sum => "SUM",
            StatKind::Count => "COUNT",
            StatKind::StdDev => "STDDEV",
            StatKind::Distribution => "DISTRIBUTION",
        }
    }

    /// The label shown in the summary row's leading (label) cell.
    pub fn label(self) -> &'static str {
        match self {
            StatKind::Mean => "Mean",
            StatKind::Median => "Median",
            StatKind::Mode => "Mode",
            StatKind::Min => "Min",
            StatKind::Max => "Max",
            StatKind::Sum => "Sum",
            StatKind::Count => "Count",
            StatKind::StdDev => "Std dev",
            StatKind::Distribution => "Distribution",
        }
    }
}

/// One computed statistic cell in a [`SummaryRow`]: its rendered `text` (used by
/// CSV/JSON/HTML/TUI and as the xlsx fallback), the `stat` that produced it (so
/// the xlsx writer can emit a live formula instead), whether the source column
/// is `numeric` (a numeric formula is only emitted then), and — for a
/// `Distribution` cell — the `match_value` its `COUNTIF` counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatValue {
    pub text: String,
    pub stat: StatKind,
    pub numeric: bool,
    pub match_value: Option<String>,
}

/// One appended summary row: a leading `label` (shown in the first column when
/// that column has no value of its own) and one optional [`StatValue`] per
/// output column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRow {
    pub label: String,
    pub cells: Vec<Option<StatValue>>,
}

impl SummaryRow {
    /// The plain-text cell for output `column`: the statistic's value if this
    /// row carries one there, else the row `label` in the first column, else
    /// empty. This is what CSV/JSON/HTML/the TUI grid render (the xlsx writer
    /// may substitute a live formula for a numeric value cell).
    pub fn text_cell(&self, column: usize) -> String {
        match self.cells.get(column).and_then(|c| c.as_ref()) {
            Some(v) => v.text.clone(),
            None if column == 0 => self.label.clone(),
            None => String::new(),
        }
    }
}

/// Whether every value in `values` parses as a spreadsheet number (and there is
/// at least one), i.e. the column is numeric.
fn column_numeric(values: &[String]) -> bool {
    !values.is_empty()
        && values
            .iter()
            .all(|v| crate::report::writer::parse_report_number(v).is_some())
}

/// The numeric values in `values` (skipping any that don't parse).
fn numeric_values(values: &[String]) -> Vec<f64> {
    values
        .iter()
        .filter_map(|v| crate::report::writer::parse_report_number(v))
        .collect()
}

/// Compute `stat` over the (already non-empty, non-no-match) `values`, returning
/// the rendered text or `None` when it doesn't apply (e.g. a numeric stat on a
/// column with no numbers, or any stat on no values). `Distribution` is not
/// computed here (it expands to several rows) and always returns `None`.
fn compute_stat(stat: StatKind, values: &[String]) -> Option<String> {
    match stat {
        StatKind::Count => (!values.is_empty()).then(|| values.len().to_string()),
        StatKind::Mode => mode(values),
        StatKind::Distribution => None,
        _ => {
            let nums = numeric_values(values);
            if nums.is_empty() {
                return None;
            }
            let v = match stat {
                StatKind::Mean => nums.iter().sum::<f64>() / nums.len() as f64,
                StatKind::Sum => nums.iter().sum::<f64>(),
                StatKind::Min => nums.iter().copied().fold(f64::INFINITY, f64::min),
                StatKind::Max => nums.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                StatKind::Median => median(&nums),
                StatKind::StdDev => std_dev(&nums),
                _ => unreachable!(),
            };
            Some(format_number(v))
        }
    }
}

/// The median of a non-empty numeric slice (mean of the two middles for an even
/// count).
fn median(nums: &[f64]) -> f64 {
    let mut sorted = nums.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// The population standard deviation of a non-empty numeric slice.
fn std_dev(nums: &[f64]) -> f64 {
    let mean = nums.iter().sum::<f64>() / nums.len() as f64;
    let var = nums.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / nums.len() as f64;
    var.sqrt()
}

/// The most frequent value (ties broken by first appearance), or `None` for no
/// values. Works on any column (string-based).
fn mode(values: &[String]) -> Option<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut best: Option<(&str, usize, usize)> = None; // (value, count, first_index)
    for (i, v) in values.iter().enumerate() {
        let c = counts.entry(v.as_str()).or_insert(0);
        *c += 1;
        let count = *c;
        let better = match best {
            None => true,
            Some((_, bc, bi)) => count > bc || (count == bc && i < bi),
        };
        if better {
            best = Some((v.as_str(), count, i));
        }
    }
    best.map(|(v, _, _)| v.to_string())
}

/// The count of each distinct value, in first-appearance order.
fn distinct_counts(values: &[String]) -> Vec<(String, usize)> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for v in values {
        if !counts.contains_key(v) {
            order.push(v.clone());
        }
        *counts.entry(v.clone()).or_insert(0) += 1;
    }
    order
        .into_iter()
        .map(|v| {
            let c = counts[&v];
            (v, c)
        })
        .collect()
}

/// Format a computed number for display: an integral value prints without a
/// decimal point; otherwise up to six decimals with trailing zeros trimmed.
pub(crate) fn format_number(n: f64) -> String {
    if !n.is_finite() {
        return n.to_string();
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        return format!("{}", n as i64);
    }
    let s = format!("{n:.6}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_string()
}

/// One resolved output column: a display `header`, the ordered `sources` to
/// coalesce (first non-empty wins) when producing its cell, and any summary
/// `stats` requested for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputColumn {
    pub header: String,
    pub sources: Vec<String>,
    /// Summary statistics to append after the data rows (empty = none).
    pub stats: Vec<StatKind>,
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
            // Peel off an optional trailing `STATISTICS(…)` clause first, then
            // the optional ` AS <name>` rename, leaving just the sources.
            let (part, stats) = split_statistics(part);
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
            Some(OutputColumn {
                header,
                sources,
                stats,
            })
        })
        .collect()
}

/// Peel a trailing `STATISTICS(stat, …)` clause (case-insensitive, whole-word,
/// outside quotes) off a column-spec, returning `(remainder, stats)`. Unknown
/// stat keywords inside the clause are dropped. Absent clause → the whole part
/// and an empty vec.
pub(crate) fn split_statistics(part: &str) -> (&str, Vec<StatKind>) {
    let bytes = part.as_bytes();
    let mut in_quote = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' {
            in_quote = !in_quote;
            i += 1;
            continue;
        }
        if !in_quote
            && (c == b's' || c == b'S')
            && (i == 0 || bytes[i - 1].is_ascii_whitespace())
            && part
                .get(i..i + 10)
                .is_some_and(|w| w.eq_ignore_ascii_case("statistics"))
        {
            let after = part[i + 10..].trim_start();
            if let Some(inner) = after.strip_prefix('(')
                && let Some(close) = inner.find(')')
            {
                let stats = inner[..close]
                    .split(',')
                    .filter_map(StatKind::parse)
                    .collect();
                return (part[..i].trim_end(), stats);
            }
        }
        i += 1;
    }
    (part, Vec::new())
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
            && bytes
                .get(i + 1)
                .is_some_and(|b| b.eq_ignore_ascii_case(&b's'))
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

/// Split on `sep` at the top level (ignoring `sep` inside double quotes or
/// inside parentheses — the latter so a `STATISTICS(a, b)` clause's commas
/// don't split the column-spec).
fn split_top_level(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                cur.push(c);
            }
            '(' if !in_quote => {
                depth += 1;
                cur.push(c);
            }
            ')' if !in_quote => {
                depth = depth.saturating_sub(1);
                cur.push(c);
            }
            _ if c == sep && !in_quote && depth == 0 => {
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
            path: Vec::new(),
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
    fn columns_directive_with_non_ascii_does_not_panic() {
        // `split_as` scans for a ` AS ` boundary byte-by-byte; a multi-byte
        // char right after an `a`/`A` must not trip a non-char-boundary slice.
        for spec in ["año", "naïve", "aé", "café as Name", "Naïve AS Rôle"] {
            let _ = parse_columns(spec); // must not panic
        }
        let cols = parse_columns("café AS Rôle");
        assert_eq!(cols[0].header, "Rôle");
        assert_eq!(cols[0].sources, vec!["café"]);
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
            stats: Vec::new(),
        };
        let r = row(&[("a.status", ""), ("b.status", "ok")], &[], None);
        assert_eq!(col.value(&r, "-"), "ok");
    }

    #[test]
    fn value_falls_back_to_vars_then_no_match_marker() {
        let col = OutputColumn {
            header: "Name".into(),
            sources: vec!["FILE".into()],
            stats: Vec::new(),
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
            stats: Vec::new(),
        };
        let r = row(&[], &[], Some("staging-au"));
        assert_eq!(col.value(&r, "-"), "staging-au");
    }

    #[test]
    fn parse_columns_reads_statistics_clause() {
        let cols = parse_columns("Time STATISTICS(MEAN, MEDIAN), Overall STATISTICS(DISTRIBUTION)");
        assert_eq!(cols[0].header, "Time");
        assert_eq!(cols[0].stats, vec![StatKind::Mean, StatKind::Median]);
        assert_eq!(cols[1].header, "Overall");
        assert_eq!(cols[1].stats, vec![StatKind::Distribution]);
    }

    #[test]
    fn parse_columns_statistics_after_as_rename() {
        let cols = parse_columns("proc.Time AS \"Pretty time\" STATISTICS(MEAN)");
        assert_eq!(cols[0].header, "Pretty time");
        assert_eq!(cols[0].sources, vec!["proc.Time"]);
        assert_eq!(cols[0].stats, vec![StatKind::Mean]);
    }

    #[test]
    fn summary_rows_compute_numeric_stats() {
        let mut res = ReportResult::default();
        res.rows = vec![
            row(&[("Time", "100")], &[], None),
            row(&[("Time", "200")], &[], None),
            row(&[("Time", "300")], &[], None),
        ];
        let cols = parse_columns("Time STATISTICS(MEAN, MEDIAN, SUM, MIN, MAX, COUNT, STDDEV)");
        let summary = res.summary_rows(&cols);
        let get = |label: &str| {
            summary
                .iter()
                .find(|r| r.label == label)
                .map(|r| r.text_cell(0))
        };
        assert_eq!(get("Count").as_deref(), Some("3"));
        assert_eq!(get("Sum").as_deref(), Some("600"));
        assert_eq!(get("Mean").as_deref(), Some("200"));
        assert_eq!(get("Median").as_deref(), Some("200"));
        assert_eq!(get("Min").as_deref(), Some("100"));
        assert_eq!(get("Max").as_deref(), Some("300"));
        // Population std dev of {100,200,300} = ~81.6497.
        assert!(get("Std dev").unwrap().starts_with("81.6"));
    }

    #[test]
    fn summary_rows_distribution_counts_each_value() {
        let mut res = ReportResult::default();
        res.rows = vec![
            row(&[("File", "a"), ("Overall", "Low")], &[], None),
            row(&[("File", "b"), ("Overall", "High")], &[], None),
            row(&[("File", "c"), ("Overall", "Low")], &[], None),
        ];
        let cols = parse_columns("File, Overall STATISTICS(DISTRIBUTION)");
        let summary = res.summary_rows(&cols);
        let low = summary
            .iter()
            .find(|r| r.label == "Overall = Low")
            .expect("Low row");
        assert_eq!(low.text_cell(0), "Overall = Low"); // label in the first column
        assert_eq!(low.text_cell(1), "2"); // count under the Overall column
        let high = summary
            .iter()
            .find(|r| r.label == "Overall = High")
            .expect("High row");
        assert_eq!(high.text_cell(1), "1");
    }

    #[test]
    fn numeric_stats_skipped_on_non_numeric_column() {
        let mut res = ReportResult::default();
        res.rows = vec![
            row(&[("V", "abc")], &[], None),
            row(&[("V", "abc")], &[], None),
            row(&[("V", "def")], &[], None),
        ];
        let cols = parse_columns("V STATISTICS(MEAN, COUNT, MODE)");
        let summary = res.summary_rows(&cols);
        assert!(
            !summary.iter().any(|r| r.label == "Mean"),
            "a numeric stat on a text column produces no row"
        );
        assert_eq!(
            summary
                .iter()
                .find(|r| r.label == "Count")
                .unwrap()
                .text_cell(0),
            "3"
        );
        assert_eq!(
            summary
                .iter()
                .find(|r| r.label == "Mode")
                .unwrap()
                .text_cell(0),
            "abc" // most frequent
        );
    }

    #[test]
    fn report_statement_statistics_merge_into_resolved_columns() {
        let mut res = ReportResult::default();
        res.note_column("Time");
        res.column_stats
            .insert("Time".to_string(), vec![StatKind::Mean]);
        let cols = res.resolved_columns(&Header::default());
        assert_eq!(cols[0].header, "Time");
        assert_eq!(cols[0].stats, vec![StatKind::Mean]);
    }
}
