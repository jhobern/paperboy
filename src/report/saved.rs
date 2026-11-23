//! Private, revisioned raw execution checkpoints. Runtime environments and
//! capture pools deliberately have no serialization implementation.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::flow::{FlowNode, HeaderLine, Producer, ReportFlow};
use super::model::{ImageData, Partial, ReportResult, ReportRow, RowRole};
use super::run::{RowSelection, RunContext};

pub type ExecutionPath = Vec<(usize, usize)>;
pub type Bindings = HashMap<ExecutionPath, HashMap<String, String>>;

#[derive(Debug, Clone, Copy, Default, clap::ValueEnum, PartialEq, Eq)]
pub enum Action {
    #[default]
    Run,
    Inspect,
    Render,
    Rerun,
    Resume,
}

#[derive(Debug, Clone, Default, clap::Args)]
pub struct Options {
    /// Private native state directory (not a table JSON export).
    #[arg(long)]
    pub run_state: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "run", requires = "run_state")]
    pub state_action: Action,
    /// Required revision for rerun/resume; stale callers send no requests.
    #[arg(long, requires = "run_state")]
    pub expected_revision: Option<u64>,
    /// Stable raw execution IDs from inspect; repeatable or comma-separated.
    #[arg(long, value_delimiter = ',', requires = "run_state")]
    pub select: Vec<String>,
    /// Explicitly expand selected executions to all sides of their comparison.
    #[arg(long, requires = "run_state")]
    pub whole_comparison: bool,
    /// JSON execution-scoped replacement bindings (schema 1).
    #[arg(long, requires = "run_state")]
    pub bindings: Option<PathBuf>,
    /// Non-secret source variable identifying a logical case, independent of target.
    #[arg(long, requires = "run_state")]
    pub case_key: Option<String>,
    /// Names allowed as execution-only bindings; immutable after state creation.
    #[arg(long, value_delimiter = ',', requires = "run_state")]
    pub execution_binding: Vec<String>,
    /// Include private report request details in inspect's stdout.
    #[arg(long, requires = "run_state")]
    pub inspect_details: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedRow {
    pub execution_id: String,
    pub case_id: String,
    pub path: ExecutionPath,
    pub key: Vec<String>,
    pub target: Option<String>,
    pub role: RowRole,
    pub comparison: Option<String>,
    pub attempt: u64,
    pub result_attempt: Option<u64>,
    pub binding_revision: u64,
    pub status: Status,
    pub cells: HashMap<String, String>,
    /// Only explicitly rendered source/truth variables, not the scope pool.
    pub render_values: HashMap<String, String>,
    pub errors: Vec<String>,
    pub shared_errors: Vec<super::run::ScopedError>,
    pub skipped: Vec<String>,
    pub warnings: Vec<String>,
    pub details: HashMap<String, String>,
    pub images: HashMap<String, ImageData>,
    pub image_errors: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    InFlight,
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub execution_id: String,
    pub attempt: u64,
    pub revision: u64,
    pub previous_status: Status,
    pub binding_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub schema: u32,
    pub paperboy: String,
    pub run_id: String,
    pub revision: u64,
    pub phase: String,
    /// Rendering directives only; no embedded request/authentication source.
    pub header: String,
    pub source_hash: String,
    pub source_hashes: BTreeMap<String, String>,
    pub shuffle: Option<u64>,
    pub input_hashes: BTreeMap<String, String>,
    pub case_key: String,
    pub execution_bindings: Vec<String>,
    pub rows: Vec<SavedRow>,
    pub column_order: Vec<String>,
    pub planned_columns: HashSet<String>,
    pub timing_columns: HashSet<String>,
    pub duration_columns: HashSet<String>,
    pub column_sources: HashMap<String, String>,
    pub column_stats: HashMap<String, Vec<super::model::StatKind>>,
    pub column_images: HashMap<String, super::flow::ImageSpec>,
    pub column_truths: HashMap<String, String>,
    pub column_details: HashSet<String>,
    pub baseline_show: Vec<String>,
    pub role_targets: HashMap<String, Vec<String>>,
    pub no_match_marker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_max_edge: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_quality: Option<u8>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub skipped: Vec<String>,
    pub history: Vec<Attempt>,
}

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn normalized(value: &str, root: Option<&Path>) -> String {
    root.and_then(|r| Path::new(value).strip_prefix(r).ok())
        .map(|p| format!("@root/{}", p.display()))
        .unwrap_or_else(|| value.to_owned())
}

pub fn case(row: &ReportRow, name: &str, root: Option<&Path>) -> Result<String, String> {
    row.vars
        .get(name)
        .filter(|v| !v.is_empty() && !v.contains("{{"))
        .map(|v| normalized(v, root))
        .ok_or_else(|| {
            format!(
                "native state requires a resolved non-empty case variable '{name}' in every row"
            )
        })
}

fn references(text: &str) -> Vec<String> {
    text.split("{{")
        .skip(1)
        .filter_map(|s| s.split_once("}}"))
        .map(|(s, _)| s.trim().to_owned())
        .collect()
}

fn render_names(shape: &ReportResult, flow: &ReportFlow) -> HashSet<String> {
    shape
        .resolved_columns(&flow.header)
        .iter()
        .flat_map(|c| {
            c.sources
                .iter()
                .cloned()
                .chain(c.truth.as_deref().into_iter().flat_map(references))
        })
        .collect()
}

impl SavedRow {
    pub fn raw_for_images(&self) -> ReportRow {
        self.raw()
    }
    fn raw(&self) -> ReportRow {
        ReportRow {
            path: self.path.clone(),
            key: self.key.clone(),
            target: self.target.clone(),
            role: self.role,
            comparison: self.comparison.clone(),
            cells: self.cells.clone(),
            vars: self.render_values.clone(),
            errors: self.errors.clone(),
        }
    }

    pub fn replace(&mut self, row: &ReportRow, names: &HashSet<String>) {
        self.cells = row
            .cells
            .iter()
            .filter(|(k, _)| !k.starts_with('\0'))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        self.details = row
            .cells
            .iter()
            .filter_map(|(k, v)| {
                k.strip_prefix("\0inspect:")
                    .map(|k| (k.to_owned(), v.clone()))
            })
            .collect();
        self.render_values = row
            .vars
            .iter()
            .filter(|(k, _)| names.contains(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        self.replace_diagnostics(row);
        self.result_attempt = Some(self.attempt);
        self.status = if row.errors.is_empty() && self.skipped.is_empty() {
            Status::Success
        } else {
            Status::Error
        };
    }

    fn replace_diagnostics(&mut self, row: &ReportRow) {
        self.errors = row.errors.clone();
        self.shared_errors = row
            .cells
            .get("\0shared_errors")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        self.skipped = row
            .cells
            .get("\0skipped")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        self.warnings = row
            .cells
            .get("\0warnings")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
    }
}

impl State {
    pub fn create(
        flow: &ReportFlow,
        ctx: &RunContext,
        shape: &ReportResult,
        options: &Options,
    ) -> Result<Self, String> {
        let case_key = options
            .case_key
            .clone()
            .ok_or("--case-key is required for a saved run")?;
        validate_supported(flow, ctx, &case_key, &options.execution_binding)?;
        if !shape.errors.is_empty() || shape.rows.is_empty() {
            return Err("native state requires a non-empty, error-free offline plan; capture-dependent producers are unsupported".into());
        }
        let names = render_names(shape, flow);
        let mut rows = Vec::new();
        let mut unique = HashSet::new();
        for (i, row) in shape.rows.iter().enumerate() {
            let key = vec![case(row, &case_key, ctx.root.as_deref())?];
            let case_id = hash(&serde_json::to_vec(&(&key, &row.comparison)).unwrap());
            if !unique.insert((
                case_id.clone(),
                row.target.clone(),
                format!("{:?}", row.role),
            )) {
                return Err(
                    "ambiguous duplicate logical case/target: choose a unique --case-key".into(),
                );
            }
            let mut saved = SavedRow {
                execution_id: format!("x{:08}", i + 1),
                case_id,
                path: row.path.clone(),
                key,
                target: row.target.clone(),
                role: row.role,
                comparison: row.comparison.clone(),
                attempt: 0,
                status: Status::Pending,
                cells: HashMap::new(),
                render_values: HashMap::new(),
                result_attempt: None,
                binding_revision: 0,
                errors: Vec::new(),
                shared_errors: Vec::new(),
                details: HashMap::new(),
                images: HashMap::new(),
                skipped: Vec::new(),
                warnings: Vec::new(),
                image_errors: Vec::new(),
            };
            saved.replace(row, &names);
            saved.status = Status::Pending;
            saved.result_attempt = None;
            // A projection is not an HTTP outcome.
            saved.details.clear();
            rows.push(saved);
        }
        let mut render_flow = flow.clone();
        render_flow.nodes.clear();
        render_flow.requests = None;
        render_flow.header.lines.retain(|line| {
            matches!(line, HeaderLine::Directive { key, .. }
            if ["name","output","columns","labels"].contains(&key.to_ascii_lowercase().as_str()))
        });
        Ok(Self {
            schema: 1,
            paperboy: env!("CARGO_PKG_VERSION").into(),
            run_id: uuid::Uuid::new_v4().to_string(),
            revision: 0,
            phase: "active".into(),
            header: render_flow.to_text(),
            source_hash: source_hash(flow, ctx),
            input_hashes: input_hashes(flow, ctx, shape)?,
            source_hashes: source_hashes(flow, ctx),
            shuffle: ctx.shuffle,
            case_key,
            execution_bindings: options.execution_binding.clone(),
            rows,
            column_order: shape.column_order.clone(),
            planned_columns: shape.planned_columns.clone(),
            timing_columns: shape.timing_columns.clone(),
            duration_columns: shape.duration_columns.clone(),
            column_sources: shape.column_sources.clone(),
            role_targets: shape.role_targets.clone(),
            column_stats: shape.column_stats.clone(),
            column_images: shape.column_images.clone(),
            column_truths: shape.column_truths.clone(),
            column_details: shape.column_details.clone(),
            baseline_show: super::compare::comparison_roles(flow)
                .map(|r| r.baseline_show)
                .unwrap_or_default(),
            no_match_marker: shape.no_match_marker.clone(),
            image_max_edge: shape.image_max_edge,
            image_quality: shape.image_quality,
            errors: Vec::new(),
            warnings: Vec::new(),
            skipped: Vec::new(),
            history: Vec::new(),
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        if self
            .image_max_edge
            .is_some_and(|v| v < super::image::MIN_EMBED_EDGE)
            || self.image_quality.is_some_and(|v| !(1..=100).contains(&v))
        {
            return Err("invalid saved image settings".into());
        }
        if self.schema != 1 {
            return Err(format!(
                "unsupported run-state schema {}; expected 1",
                self.schema
            ));
        }
        if self.paperboy != env!("CARGO_PKG_VERSION") {
            return Err(format!(
                "state requires PaperBoy {}, running {}",
                self.paperboy,
                env!("CARGO_PKG_VERSION")
            ));
        }
        uuid::Uuid::parse_str(&self.run_id).map_err(|_| "invalid run ID")?;
        if !["active", "interrupted", "complete"].contains(&self.phase.as_str())
            || self.rows.is_empty()
        {
            return Err("invalid saved phase or empty execution plan".into());
        }
        let mut ids = HashSet::new();
        let mut paths = HashSet::new();
        let mut cases = HashSet::new();
        for r in &self.rows {
            if !ids.insert(&r.execution_id)
                || !paths.insert(&r.path)
                || !cases.insert((&r.case_id, &r.target, format!("{:?}", r.role)))
                || r.case_id != hash(&serde_json::to_vec(&(&r.key, &r.comparison)).unwrap())
            {
                return Err("corrupt state: duplicate or mismatched execution identity".into());
            }
            if r.cells.keys().any(|k| k.starts_with('\0'))
                || (self.phase == "complete"
                    && matches!(r.status, Status::Pending | Status::InFlight))
            {
                return Err("corrupt state: invalid raw cells or completion status".into());
            }
            if r.result_attempt.is_some_and(|a| a > r.attempt) || r.binding_revision > self.revision
            {
                return Err("corrupt state: invalid attempt or binding revision".into());
            }
            let mut shared = HashMap::new();
            for (scope, message) in &r.shared_errors {
                let count = shared.entry(message).or_insert(0usize);
                *count += 1;
                if !r.path.starts_with(scope)
                    || *count > r.errors.iter().filter(|e| *e == message).count()
                {
                    return Err("corrupt state: invalid shared diagnostic ownership".into());
                }
            }
        }
        let flow = self.flow()?;
        if !flow.nodes.is_empty() || flow.requests.is_some() || flow.header.baseline().is_some() {
            return Err(
                "saved render directives must not contain executable source or external baselines"
                    .into(),
            );
        }
        Ok(())
    }

    pub fn flow(&self) -> Result<ReportFlow, String> {
        super::parser::parse_flow(&self.header)
            .map_err(|e| format!("invalid saved render directives: {e}"))
    }

    pub fn validate_plan(
        &self,
        flow: &ReportFlow,
        ctx: &RunContext,
        shape: &ReportResult,
    ) -> Result<(), String> {
        validate_supported(flow, ctx, &self.case_key, &self.execution_bindings)?;
        if shape.image_max_edge != self.image_max_edge || shape.image_quality != self.image_quality
        {
            return Err("image settings differ from the saved plan".into());
        }
        if source_hash(flow, ctx) != self.source_hash
            || input_hashes(flow, ctx, shape)? != self.input_hashes
        {
            return Err("saved source/input hash mismatch; restore the original sources/corpus (relocation may change roots, not contents)".into());
        }
        if shape.rows.len() != self.rows.len() {
            return Err("saved plan row count changed".into());
        }
        for (old, new) in self.rows.iter().zip(&shape.rows) {
            if old.path != new.path
                || old.target != new.target
                || old.role != new.role
                || old.comparison != new.comparison
                || old.key != vec![case(new, &self.case_key, ctx.root.as_deref())?]
            {
                return Err("saved plan case/target/order changed".into());
            }
        }
        Ok(())
    }

    pub fn selected(
        &self,
        action: Action,
        ids: &[String],
        wide: bool,
    ) -> Result<Vec<usize>, String> {
        if action == Action::Resume {
            if !ids.is_empty() || wide {
                return Err("resume does not accept selectors/comparison expansion".into());
            }
            return Ok(self
                .rows
                .iter()
                .enumerate()
                .filter(|(_, r)| matches!(r.status, Status::Pending | Status::InFlight))
                .map(|(i, _)| i)
                .collect());
        }
        if ids.is_empty() {
            return Err("rerun requires at least one --select execution ID".into());
        }
        let mut selected = HashSet::new();
        for id in ids {
            let i = self
                .rows
                .iter()
                .position(|r| &r.execution_id == id)
                .ok_or_else(|| format!("unknown execution ID '{id}'"))?;
            if !selected.insert(i) {
                return Err(format!("duplicate selector '{id}'"));
            }
        }
        if wide {
            if selected.iter().any(|&i| {
                self.rows[i].comparison.is_none()
                    || !matches!(self.rows[i].role, RowRole::Baseline | RowRole::Candidate)
            }) {
                return Err(
                    "--whole-comparison requires executions in a declared comparison".into(),
                );
            }
            let cases: HashSet<_> = selected
                .iter()
                .map(|&i| self.rows[i].case_id.clone())
                .collect();
            selected.extend(
                self.rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| cases.contains(&r.case_id))
                    .map(|(i, _)| i),
            );
        }
        Ok((0..self.rows.len())
            .filter(|i| selected.contains(i))
            .collect())
    }

    pub fn raw(&self) -> ReportResult {
        let pending: HashSet<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.status, Status::Pending | Status::InFlight))
            .map(|(i, _)| i)
            .collect();
        ReportResult {
            rows: self.rows.iter().map(SavedRow::raw).collect(),
            partial: (!pending.is_empty() || self.phase != "complete").then_some(Partial {
                rows_completed: self.rows.len() - pending.len(),
                rows_planned: self.rows.len(),
            }),
            pending,
            column_order: self.column_order.clone(),
            planned_columns: self.planned_columns.clone(),
            timing_columns: self.timing_columns.clone(),
            duration_columns: self.duration_columns.clone(),
            column_sources: self.column_sources.clone(),
            role_targets: self.role_targets.clone(),
            column_stats: self.column_stats.clone(),
            column_images: self.column_images.clone(),
            column_truths: self.column_truths.clone(),
            column_details: self.column_details.clone(),
            no_match_marker: self.no_match_marker.clone(),
            image_max_edge: self.image_max_edge,
            image_quality: self.image_quality,
            errors: self
                .errors
                .iter()
                .cloned()
                .chain(
                    self.rows
                        .iter()
                        .flat_map(|r| r.image_errors.iter().cloned()),
                )
                .collect(),
            warnings: self.warnings.clone(),
            skipped: self.skipped.clone(),
            ..Default::default()
        }
    }

    pub fn rendered(&self) -> Result<ReportResult, String> {
        let mut flow = self.flow()?;
        // Live comparison identity is stored on every raw side. Reconstruct
        // roles explicitly rather than depending on secret setup captures.
        let mut result = self.raw();
        if result.pending.is_empty() {
            result.track_baseline = result
                .resolved_columns(&flow.header)
                .iter()
                .any(|c| c.truth.is_some());
            collapse(&mut result, &self.baseline_show);
            // Collapse was already performed; the saved header has no baseline.
            flow.nodes.clear();
            let runner = super::run::LiveRunner { file_root: None };
            let ctx = RunContext {
                entries: &[],
                helpers: &[],
                base_vars: HashMap::new(),
                named_envs: HashMap::new(),
                root: None,
                runner: &runner,
                strings: crate::i18n::Strings::english(),
                params: HashMap::new(),
                sink: None,
                shuffle: None,
                cancel: None,
            };
            super::run::finalize_data(&mut result, &flow, &ctx);
        }
        result.images.clear();
        let columns = result.resolved_columns(&flow.header);
        for (i, row) in result.rows.iter().enumerate() {
            if let Some(saved) = self.rows.iter().find(|s| s.path == row.path) {
                for column in columns.iter().filter(|c| c.image.is_some()) {
                    let value = column.value(row, &result.no_match_marker);
                    if value.is_empty() || value == result.no_match_marker {
                        continue;
                    }
                    let source = column.sources.iter().find(|s| {
                        row.cells
                            .get(*s)
                            .filter(|v| !v.is_empty())
                            .or_else(|| row.vars.get(*s))
                            .is_some_and(|v| !v.is_empty())
                    });
                    let image =
                        if let Some(source) = source.and_then(|s| s.strip_prefix("baseline.")) {
                            self.rows
                                .iter()
                                .find(|r| {
                                    r.role == RowRole::Baseline
                                        && r.key == row.key
                                        && r.comparison == row.comparison
                                })
                                .and_then(|base| {
                                    base.images
                                        .get(&column.header)
                                        .or_else(|| base.images.get(source))
                                })
                        } else {
                            saved.images.get(&column.header)
                        };
                    if let Some(image) = image {
                        result
                            .images
                            .insert((i, column.header.clone()), image.clone());
                    }
                }
            }
        }
        Ok(result)
    }

    pub fn names(&self) -> Result<HashSet<String>, String> {
        Ok(render_names(&self.raw(), &self.flow()?))
    }

    pub fn selection(&self, selected: &[usize], bindings: Bindings) -> RowSelection {
        RowSelection::exact(
            &selected
                .iter()
                .map(|&i| self.rows[i].path.clone())
                .collect::<Vec<_>>(),
        )
        .with_bindings(bindings)
        .with_inspection()
    }

    pub fn merge(&mut self, fresh: &ReportResult, names: &HashSet<String>) -> Result<(), String> {
        for (row, finished) in fresh
            .rows
            .iter()
            .map(|r| (r, true))
            .chain(fresh.unfinished_rows.iter().map(|r| (r, false)))
        {
            let i = self
                .rows
                .iter()
                .position(|r| r.path == row.path)
                .ok_or("live execution diverged from saved plan")?;
            if self.rows[i].target != row.target
                || self.rows[i].role != row.role
                || self.rows[i].comparison != row.comparison
            {
                return Err("live execution target diverged from saved plan".into());
            }
            let mut replacement = self.rows[i].clone();
            if finished {
                replacement.replace(row, names);
            } else {
                replacement.replace_diagnostics(row);
                replacement.status = Status::InFlight;
            }
            self.replace_row(i, replacement);
        }
        // Diagnostics not claimed by a returned row are setup/cleanup/global.
        for e in &fresh.errors {
            if !fresh
                .rows
                .iter()
                .chain(&fresh.unfinished_rows)
                .any(|r| r.errors.contains(e))
                && !self.errors.contains(e)
            {
                self.errors.push(e.clone());
            }
        }
        for w in &fresh.warnings {
            if !self.rows.iter().any(|r| r.warnings.contains(w)) && !self.warnings.contains(w) {
                self.warnings.push(w.clone());
            }
        }
        for s in &fresh.skipped {
            if !self.rows.iter().any(|r| r.skipped.contains(s)) && !self.skipped.contains(s) {
                self.skipped.push(s.clone());
            }
        }
        for c in &fresh.column_order {
            if !c.starts_with('\0') && !self.column_order.contains(c) {
                self.column_order.push(c.clone());
            }
        }
        self.timing_columns.extend(fresh.timing_columns.clone());
        self.duration_columns.extend(fresh.duration_columns.clone());
        self.column_sources.extend(fresh.column_sources.clone());
        self.role_targets.extend(fresh.role_targets.clone());
        Ok(())
    }

    fn replace_row(&mut self, i: usize, replacement: SavedRow) {
        let changed = self.rows[i].errors != replacement.errors
            || self.rows[i].shared_errors != replacement.shared_errors;
        let unowned = changed.then(|| {
            let mut unowned = self.errors.clone();
            for error in owned_errors(&self.rows) {
                if let Some(at) = unowned.iter().position(|e| e == &error) {
                    unowned.remove(at);
                }
            }
            unowned
        });
        self.reconcile_aux(i, &replacement);
        self.rows[i] = replacement;
        if let Some(unowned) = unowned {
            self.errors = owned_errors(&self.rows);
            self.errors.extend(unowned);
        }
    }

    fn reconcile_aux(&mut self, i: usize, replacement: &SavedRow) {
        for warnings in [false, true] {
            let get = |r: &SavedRow| {
                if warnings {
                    r.warnings.clone()
                } else {
                    r.skipped.clone()
                }
            };
            let old = get(&self.rows[i]);
            let new = get(replacement);
            if old == new {
                continue;
            }
            let others: Vec<_> = self
                .rows
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .flat_map(|(_, r)| get(r))
                .collect();
            let flat = if warnings {
                &mut self.warnings
            } else {
                &mut self.skipped
            };
            let messages: HashSet<_> = old.iter().chain(&new).collect();
            for e in messages {
                let count = |xs: &[String]| xs.iter().filter(|s| *s == e).count();
                super::rerun::reconcile_message(flat, e, count(&others), count(&old), count(&new));
            }
        }
    }
}

/// A shared setup failure is counted once per originating scope, not once per
/// child row. Explicit origins are necessary while journaling: unlike an
/// in-memory splice, the final flat diagnostic list does not exist yet.
fn owned_errors(rows: &[SavedRow]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashMap::new();
    for row in rows {
        let mut counts = HashMap::new();
        let mut inherited = HashMap::new();
        for origin in &row.shared_errors {
            let n = counts.entry(origin).or_insert(0);
            *n += 1;
            let seen = seen.entry(origin).or_insert(0);
            if *seen < *n {
                out.push(origin.1.clone());
                *seen = *n;
            }
            *inherited.entry(&origin.1).or_insert(0usize) += 1;
        }
        for error in &row.errors {
            let inherited = inherited.entry(error).or_insert(0);
            if *inherited > 0 {
                *inherited -= 1;
            } else {
                out.push(error.clone());
            }
        }
    }
    out
}

fn collapse(result: &mut ReportResult, show: &[String]) {
    // Reuse the comparison core, retaining its role/comparison scoping.
    let baseline: Vec<_> = result
        .rows
        .iter()
        .filter(|r| r.role == RowRole::Baseline)
        .filter_map(|r| r.target.clone())
        .collect();
    let candidates: Vec<_> = result
        .rows
        .iter()
        .filter(|r| r.role == RowRole::Candidate)
        .filter_map(|r| r.target.clone())
        .collect();
    if !baseline.is_empty() || !candidates.is_empty() {
        let roles = super::compare::Roles {
            baseline: baseline.into_iter().collect(),
            comparisons: candidates,
            baseline_show: show.to_vec(),
        };
        super::compare::apply(result, &roles);
    }
}

fn source_hash(flow: &ReportFlow, ctx: &RunContext) -> String {
    let mut source = flow.clone();
    source.header.lines.retain(|line| {
        !matches!(line,HeaderLine::Directive{key,..}
        if ["root","collection","environment"].contains(&key.to_ascii_lowercase().as_str()))
    });
    let entries: Vec<_> = ctx.entries.iter().map(|e| e.to_hurl()).collect();
    let helpers: Vec<_> = ctx
        .helpers
        .iter()
        .map(|h| {
            (
                &h.alias,
                h.entries.iter().map(|e| e.to_hurl()).collect::<Vec<_>>(),
            )
        })
        .collect();
    let params: BTreeMap<_, _> = ctx
        .params
        .iter()
        .map(|(k, v)| (k, normalized(v, ctx.root.as_deref())))
        .collect();
    hash(&serde_json::to_vec(&(source.to_text(), entries, helpers, params, ctx.shuffle)).unwrap())
}

fn source_hashes(flow: &ReportFlow, ctx: &RunContext) -> BTreeMap<String, String> {
    let mut sources = BTreeMap::from([
        ("trail".into(), hash(flow.to_text().as_bytes())),
        (
            "collection".into(),
            hash(
                ctx.entries
                    .iter()
                    .map(|e| e.to_hurl())
                    .collect::<String>()
                    .as_bytes(),
            ),
        ),
    ]);
    for helper in ctx.helpers {
        sources.insert(
            format!("helper:{}", helper.alias),
            hash(
                helper
                    .entries
                    .iter()
                    .map(|e| e.to_hurl())
                    .collect::<String>()
                    .as_bytes(),
            ),
        );
    }
    sources
}

fn dynamic_names(flow: &ReportFlow, ctx: &RunContext) -> HashSet<String> {
    let mut captured: HashSet<String> = ctx
        .entries
        .iter()
        .chain(ctx.helpers.iter().flat_map(|h| &h.entries))
        .flat_map(|e| {
            e.captures
                .iter()
                .chain(e.generators.iter())
                .map(|(n, _)| n.clone())
        })
        .collect();
    fn branch_assignments(nodes: &[FlowNode], inside: bool, out: &mut HashSet<String>) {
        for node in nodes {
            match node {
                FlowNode::Assign { key, .. } if inside => {
                    out.insert(key.clone());
                }
                FlowNode::Branch {
                    body,
                    alternative,
                    kind,
                } => {
                    if let super::flow::BranchKind::Try { error_var, .. } = kind {
                        out.insert(error_var.clone());
                    }
                    branch_assignments(body, true, out);
                    branch_assignments(alternative, true, out);
                }
                FlowNode::ForEach { body, .. }
                | FlowNode::ForEnvs { body, .. }
                | FlowNode::Graph { body, .. } => branch_assignments(body, inside, out),
                FlowNode::Cache { body, persist, .. } => {
                    out.extend(persist.iter().cloned());
                    branch_assignments(body, true, out);
                }
                _ => {}
            }
        }
    }
    branch_assignments(&flow.nodes, false, &mut captured);
    captured
}

fn validate_supported(
    flow: &ReportFlow,
    ctx: &RunContext,
    case_key: &str,
    bindings: &[String],
) -> Result<(), String> {
    if flow.header.baseline().is_some() {
        return Err(
            "native state does not yet support external baseline snapshots; use live ENVS sides"
                .into(),
        );
    }
    let captured = dynamic_names(flow, ctx);
    if captured
        .iter()
        .any(|name| name.starts_with("PRELUDE_IMAGE_"))
    {
        return Err(
            "native state requires image settings independent of captures and branches".into(),
        );
    }
    if captured.contains(case_key) {
        return Err("native state requires a case key independent of captures and branches".into());
    }
    fn walk(nodes: &[FlowNode], captured: &HashSet<String>) -> Result<(), String> {
        fn graph_reports(nodes: &[FlowNode]) -> bool {
            nodes.iter().any(|n| match n {
                FlowNode::Report(_) => true,
                FlowNode::Graph { body, .. } => graph_reports(body),
                FlowNode::Branch {
                    body, alternative, ..
                } => graph_reports(body) || graph_reports(alternative),
                _ => false,
            })
        }
        let has_loop = nodes
            .iter()
            .any(|n| matches!(n, FlowNode::ForEach { .. } | FlowNode::ForEnvs { .. }));
        if has_loop && graph_reports(nodes) {
            return Err("native checkpointing requires REPORT statements in leaf blocks, including GRAPH reports".into());
        }
        for n in nodes {
            match n {
                FlowNode::Report(_) if has_loop => return Err("native checkpointing requires REPORT statements in leaf blocks; outer broadcasts cannot be durably completed per row".into()),
                FlowNode::ForEach{body,..}|FlowNode::ForEnvs{body,..}|FlowNode::Graph{body,..} => walk(body,captured)?,
                // Keep saved row identity and source discovery independent of
                // live conditions. Report the final result after the branch.
                FlowNode::Branch { body, alternative, kind } => {
                    if has_loop {
                        return Err("native state requires IF/TRY in leaf blocks so interrupted diagnostics belong to an execution".into());
                    }
                    let retry = matches!(kind, super::flow::BranchKind::Try { retries, .. } if *retries > 0);
                    if graph_reports(alternative) || (graph_reports(body) && !retry) {
                        return Err("native state requires REPORT outside IF/TRY; branch on requests, then report the final result".into());
                    }
                    walk(body, captured)?;
                    walk(alternative, captured)?;
                }
                _=>{}
            }
        }
        // Captures may feed requests, but never the expansion of the saved plan.
        for n in nodes {
            if matches!(
                n,
                FlowNode::ForEach { .. }
                    | FlowNode::ForEnvs { .. }
                    | FlowNode::ListDecl { .. }
                    | FlowNode::Assign { .. }
                    | FlowNode::Param(_)
            ) {
                let f = ReportFlow {
                    nodes: vec![n.clone()],
                    ..Default::default()
                };
                // Restrict this check to the node's declaration, not its body.
                let line = f.to_text().lines().next().unwrap_or_default().to_owned();
                if references(&line)
                    .iter()
                    .any(|r| r.contains('.') || captured.contains(r))
                {
                    return Err("native state does not support capture-derived producers/assignments/target identity; supply explicit input bindings".into());
                }
                if matches!(n, FlowNode::ForEnvs { .. }) && line.contains("FILE(") {
                    return Err("native state does not support ENVS snapshot files".into());
                }
            }
        }
        Ok(())
    }
    walk(&flow.nodes, &captured)?;
    // Name the entries that could not be parsed. The condition is a property of
    // the collection, not of anything the report says, so without the names the
    // author is left bisecting a file they did not necessarily write — and the
    // usual cause is a handful of unused requests in a large export.
    let unreadable: Vec<&str> = ctx
        .entries
        .iter()
        .chain(ctx.helpers.iter().flat_map(|h| &h.entries))
        .filter(|e| e.unparsed.is_some())
        .map(|e| {
            if e.title.is_empty() {
                "<untitled>"
            } else {
                e.title.as_str()
            }
        })
        .collect();
    if !unreadable.is_empty() {
        return Err(format!(
            "native state requires fully parsed request sources; \
             these could not be parsed: {}",
            unreadable.join(", ")
        ));
    }
    let names = render_names(
        &ReportResult {
            column_truths: flow.column_truths(),
            ..Default::default()
        },
        flow,
    );
    let mut unique = HashSet::new();
    fn env_binding(nodes: &[FlowNode], name: &str) -> bool {
        nodes.iter().any(|node| match node {
            FlowNode::Branch {
                body, alternative, ..
            } => env_binding(body, name) || env_binding(alternative, name),
            FlowNode::ForEnvs { var, body, .. } => var == name || env_binding(body, name),
            FlowNode::ForEach { body, .. }
            | FlowNode::Graph { body, .. }
            | FlowNode::Cache { body, .. } => env_binding(body, name),
            _ => false,
        })
    }
    fn validate_using(nodes: &[FlowNode], ctx: &RunContext, name: &str) -> Result<(), String> {
        use super::flow::{OverrideTarget, ReportStmt, UsingItem};
        for node in nodes {
            let call = match node {
                FlowNode::Branch {
                    body, alternative, ..
                } => {
                    validate_using(body, ctx, name)?;
                    validate_using(alternative, ctx, name)?;
                    None
                }
                FlowNode::Request { name, using, .. }
                | FlowNode::Cleanup { name, using, .. }
                | FlowNode::Report(ReportStmt::Request { name, using, .. }) => Some((name, using)),
                FlowNode::Cache { files, body, .. } => {
                    if files.iter().any(|file| {
                        references(file)
                            .iter()
                            .any(|r| r.rsplit('.').next() == Some(name))
                    }) {
                        return Err(format!("binding '{name}' changes a cache input file"));
                    }
                    validate_using(body, ctx, name)?;
                    None
                }
                FlowNode::ForEach { body, .. }
                | FlowNode::ForEnvs { body, .. }
                | FlowNode::Graph { body, .. } => {
                    validate_using(body, ctx, name)?;
                    None
                }
                _ => None,
            };
            if let Some((request, using)) = call {
                let entry = super::run::resolve_qualified(ctx.entries, ctx.helpers, request);
                for item in using {
                    if let UsingItem::Override { target, value } = item {
                        let reads = |text: &str| {
                            references(text)
                                .iter()
                                .any(|r| r.rsplit('.').next() == Some(name))
                        };
                        let forbidden = match target {
                            OverrideTarget::Url => {
                                let authority = value
                                    .split("://")
                                    .nth(1)
                                    .unwrap_or(value)
                                    .split(['/', '?', '#'])
                                    .next()
                                    .unwrap_or("");
                                reads(authority)
                            }
                            OverrideTarget::Option(_) => reads(value),
                            OverrideTarget::Body => {
                                value.trim_start().starts_with("file,") && reads(value)
                            }
                            OverrideTarget::Multipart(key) | OverrideTarget::Form(key) => {
                                reads(value)
                                    && entry.is_some_and(|e| {
                                        e.form_fields
                                            .iter()
                                            .any(|f| &f.key == key && f.kind.is_multipart())
                                    })
                            }
                            _ => false,
                        };
                        if forbidden {
                            return Err(format!(
                                "binding '{name}' changes an endpoint, execution option or input file through USING"
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }
    for name in bindings {
        if name == case_key
            || name == "TARGET"
            || env_binding(&flow.nodes, name)
            || name.starts_with("PRELUDE_")
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || name.is_empty()
            || !unique.insert(name)
            || names.contains(name)
        {
            return Err(format!(
                "'{name}' is not a valid execution-only binding (identity/truth/render variables are immutable)"
            ));
        }
        validate_using(&flow.nodes, ctx, name)?;
        // An endpoint replacement is not a repaired session binding.
        for entry in ctx
            .entries
            .iter()
            .chain(ctx.helpers.iter().flat_map(|h| &h.entries))
        {
            let authority = entry
                .url
                .split("://")
                .nth(1)
                .unwrap_or(&entry.url)
                .split(['/', '?', '#'])
                .next()
                .unwrap_or("");
            if references(authority).contains(name)
                || entry.generators.iter().any(|(n, _)| n == name)
                || entry.options.iter().any(|o| {
                    o.enabled
                        && !o.key.eq_ignore_ascii_case("variable")
                        && references(&o.value).contains(name)
                })
            {
                return Err(format!(
                    "binding '{name}' controls an endpoint or generator and cannot be replaced"
                ));
            }
            if entry
                .form_fields
                .iter()
                .any(|f| f.enabled && f.kind.is_multipart() && references(&f.value).contains(name))
                || entry.body_src.as_deref().is_some_and(|b| {
                    b.trim_start().starts_with("file,") && references(b).contains(name)
                })
            {
                return Err(format!(
                    "binding '{name}' controls an input file and cannot be replaced"
                ));
            }
        }
    }
    Ok(())
}

fn input_hashes(
    flow: &ReportFlow,
    ctx: &RunContext,
    shape: &ReportResult,
) -> Result<BTreeMap<String, String>, String> {
    fn producers<'a>(nodes: &'a [FlowNode], out: &mut Vec<&'a Producer>) {
        for n in nodes {
            match n {
                FlowNode::ForEach { producer, body, .. } => {
                    out.push(producer);
                    producers(body, out);
                }
                FlowNode::ListDecl { producer, .. } => out.push(producer),
                FlowNode::ForEnvs { body, .. }
                | FlowNode::Graph { body, .. }
                | FlowNode::Cache { body, .. } => producers(body, out),
                _ => {}
            }
        }
    }
    #[derive(Clone, Hash, PartialEq, Eq)]
    enum Source {
        Files(Option<String>),
        Folders(Option<String>),
        Manifest,
    }
    fn paths(p: &Producer, out: &mut Vec<(String, Source)>) {
        match p {
            Producer::Files { dir, glob } => out.push((dir.clone(), Source::Files(glob.clone()))),
            Producer::Folders { dir, glob, .. } => {
                out.push((dir.clone(), Source::Folders(glob.clone())))
            }
            Producer::Tuples { path } => out.push((path.clone(), Source::Manifest)),
            Producer::Zip(ps) | Producer::Concat(ps) => {
                for p in ps {
                    paths(p, out)
                }
            }
            _ => {}
        }
    }
    let mut ps = Vec::new();
    producers(&flow.nodes, &mut ps);
    let mut templates = Vec::new();
    for p in ps {
        paths(p, &mut templates)
    }
    fn cache_files(nodes: &[FlowNode], out: &mut Vec<(String, Source)>) {
        for node in nodes {
            match node {
                FlowNode::Cache { files, body, .. } => {
                    out.extend(files.iter().cloned().map(|path| (path, Source::Manifest)));
                    cache_files(body, out);
                }
                FlowNode::Branch {
                    body, alternative, ..
                } => {
                    cache_files(body, out);
                    cache_files(alternative, out);
                }
                FlowNode::ForEach { body, .. }
                | FlowNode::ForEnvs { body, .. }
                | FlowNode::Graph { body, .. } => cache_files(body, out),
                _ => {}
            }
        }
    }
    cache_files(&flow.nodes, &mut templates);
    let mut files = HashSet::new();
    let mut visited = HashSet::new();
    let image_columns: Vec<_> = shape
        .resolved_columns(&flow.header)
        .into_iter()
        .filter(|c| c.image.is_some())
        .collect();
    for row in &shape.rows {
        for column in &image_columns {
            let value = column.value(row, &shape.no_match_marker);
            if !value.starts_with("data:") && !value.contains("://") {
                let path = super::producers::resolve_path(ctx.root.as_deref(), &value);
                if path.is_file() {
                    files.insert(path);
                }
            }
        }
        for (template, source) in &templates {
            let text = crate::environment::substitute(template, &row.vars);
            if text.contains("{{") {
                return Err("unresolved producer path in saved plan".into());
            }
            let path = super::producers::resolve_path(ctx.root.as_deref(), &text);
            let source = match source {
                Source::Files(glob) => Source::Files(
                    glob.as_ref()
                        .map(|g| crate::environment::substitute(g, &row.vars)),
                ),
                Source::Folders(glob) => Source::Folders(
                    glob.as_ref()
                        .map(|g| crate::environment::substitute(g, &row.vars)),
                ),
                Source::Manifest => Source::Manifest,
            };
            if !visited.insert((path.clone(), source.clone())) {
                continue;
            }
            match source {
                Source::Files(glob) => {
                    files.extend(super::producers::list_files(&path, glob.as_deref())?)
                }
                Source::Folders(glob) => {
                    for folder in super::producers::list_folders(&path, glob.as_deref())? {
                        files.extend(super::producers::list_files(&folder, Some("**/*"))?);
                    }
                }
                Source::Manifest => {
                    files.insert(path);
                }
            }
        }
    }
    // Probe the actual resolved requests offline as well: producer directories
    // alone don't cover a literal multipart file or a USING override. Neither
    // this pass nor its serialized output contains authentication/capture pools.
    struct Probe {
        root: Option<PathBuf>,
        paths: std::sync::Mutex<HashSet<PathBuf>>,
        error: std::sync::Mutex<Option<String>>,
        env_vars: Vec<String>,
        endpoints: std::sync::Mutex<BTreeMap<String, String>>,
        unknown: HashSet<String>,
    }
    impl super::run::EntryRunner for Probe {
        fn offline(&self) -> bool {
            true
        }
        fn dependency_probe(&self) -> Option<&HashSet<String>> {
            Some(&self.unknown)
        }
        fn run(
            &self,
            entry: &crate::hurl::HurlEntry,
            vars: &HashMap<String, String>,
        ) -> crate::hurl::RunOutput {
            let url = crate::environment::substitute(&entry.url, vars);
            let origin = url.split_once("://").map(|(scheme, rest)| {
                format!(
                    "{scheme}://{}",
                    rest.split(['/', '?', '#']).next().unwrap_or("")
                )
            });
            if let Some(origin) = origin.filter(|o| !o.contains("{{")) {
                let scope: BTreeMap<_, _> = self
                    .env_vars
                    .iter()
                    .filter_map(|name| vars.get(name).map(|v| (name, v)))
                    .collect();
                let key = hash(&serde_json::to_vec(&(&entry.title, scope, &origin)).unwrap());
                self.endpoints
                    .lock()
                    .unwrap()
                    .insert(format!("@endpoint/{key}"), hash(origin.as_bytes()));
            } else {
                *self.error.lock().unwrap() = Some(
                    "native state requires endpoint origins resolved by the offline plan".into(),
                );
            }
            let mut values: Vec<_> = entry
                .form_fields
                .iter()
                .filter(|f| f.enabled && f.kind.is_multipart())
                .map(|f| f.value.clone())
                .collect();
            if let Some(body) = entry
                .body_src
                .as_deref()
                .and_then(|b| b.trim().strip_prefix("file,"))
            {
                values.push(body.trim_end_matches(';').to_owned());
            }
            for value in values {
                let value = crate::environment::substitute(&value, vars);
                if value.contains("{{") {
                    *self.error.lock().unwrap()=Some("native state cannot fingerprint capture-derived request files; supply an explicit input path".into());
                    continue;
                }
                self.paths
                    .lock()
                    .unwrap()
                    .insert(super::producers::resolve_path(self.root.as_deref(), &value));
            }
            super::run::DryRunner.run(entry, vars)
        }
    }
    fn env_vars(nodes: &[FlowNode], names: &mut Vec<String>) {
        for node in nodes {
            match node {
                FlowNode::ForEnvs { var, body, .. } => {
                    names.push(var.clone());
                    env_vars(body, names);
                }
                FlowNode::ForEach { body, .. }
                | FlowNode::Graph { body, .. }
                | FlowNode::Cache { body, .. } => env_vars(body, names),
                _ => {}
            }
        }
    }
    let mut names = Vec::new();
    env_vars(&flow.nodes, &mut names);
    let probe = Probe {
        root: ctx.runner.input_root().map(Path::to_path_buf),
        paths: Default::default(),
        error: Default::default(),
        env_vars: names,
        endpoints: Default::default(),
        unknown: dynamic_names(flow, ctx),
    };
    let dry = RunContext {
        entries: ctx.entries,
        helpers: ctx.helpers,
        base_vars: ctx.base_vars.clone(),
        named_envs: ctx.named_envs.clone(),
        root: ctx.root.clone(),
        runner: &probe,
        strings: ctx.strings,
        params: ctx.params.clone(),
        sink: None,
        shuffle: ctx.shuffle,
        cancel: None,
    };
    let discovered = super::run::run_flow_raw(flow, &dry);
    if !discovered.errors.is_empty() {
        return Err(format!(
            "cannot resolve saved request dependencies: {}",
            discovered.errors.join("; ")
        ));
    }
    if let Some(error) = probe.error.into_inner().unwrap() {
        return Err(error);
    }
    files.extend(probe.paths.into_inner().unwrap());
    let mut hashes = probe.endpoints.into_inner().unwrap();
    for path in files {
        let mut f =
            File::open(&path).map_err(|e| format!("cannot hash input {}: {e}", path.display()))?;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let n = f.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
        }
        let name = if let Some(root) = ctx.root.as_deref().filter(|r| path.starts_with(r)) {
            normalized(&path.to_string_lossy(), Some(root))
        } else if let Some(root) = ctx.runner.input_root().filter(|r| path.starts_with(r)) {
            format!("@collection/{}", path.strip_prefix(root).unwrap().display())
        } else {
            path.to_string_lossy().into_owned()
        };
        hashes.insert(name, format!("{:x}", digest.finalize()));
    }
    Ok(hashes)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingFile {
    schema: u32,
    run_id: String,
    revision: u64,
    overrides: Vec<BindingOverride>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingOverride {
    execution_id: String,
    target: Option<String>,
    values: HashMap<String, String>,
}

pub fn load_bindings(
    path: Option<&Path>,
    state: &State,
    selected: &[usize],
) -> Result<Bindings, String> {
    let Some(path) = path else {
        return Ok(HashMap::new());
    };
    let file: BindingFile = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| format!("invalid bindings: {e}"))?;
    if file.schema != 1 || file.run_id != state.run_id || file.revision != state.revision {
        return Err("bindings run/schema/revision mismatch".into());
    }
    let mut bindings = HashMap::new();
    for item in file.overrides {
        let row = selected
            .iter()
            .map(|&i| &state.rows[i])
            .find(|r| r.execution_id == item.execution_id && r.target == item.target)
            .ok_or("binding references an unselected or mismatched execution/target")?;
        if bindings.contains_key(&row.path) {
            return Err("duplicate binding scope".into());
        }
        if item.values.is_empty()
            || item
                .values
                .iter()
                .any(|(k, v)| !state.execution_bindings.contains(k) || v.contains("{{"))
        {
            return Err("bindings must be literal values for declared execution-only names".into());
        }

        bindings.insert(row.path.clone(), item.values);
    }
    Ok(bindings)
}

/// Presentation output must never overwrite the authoritative bundle.
pub fn check_outputs(path: &Path, outputs: &[String]) -> Result<(), String> {
    fn absolute(path: &Path) -> Result<PathBuf, String> {
        if let Ok(real) = fs::canonicalize(path) {
            return Ok(real);
        }
        let full = std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path);
        let mut normalized = PathBuf::new();
        for part in full.components() {
            match part {
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                std::path::Component::CurDir => {}
                other => normalized.push(other.as_os_str()),
            }
        }
        if let Some(parent) = normalized.parent()
            && let Ok(real) = fs::canonicalize(parent)
        {
            return Ok(real.join(normalized.file_name().ok_or("output requires a filename")?));
        }
        Ok(normalized)
    }
    let bundle = absolute(path)?;
    for output in outputs.iter().filter(|o| o.as_str() != "-") {
        if absolute(Path::new(output))?.starts_with(&bundle) {
            return Err("report output must be outside the native run-state directory".into());
        }
    }
    Ok(())
}
/// The kernel releases this exclusive lock even after SIGKILL. The lock file
/// itself stays in place: unlinking it would let two writers lock different
/// inodes under the same name.
pub struct Store {
    pub path: PathBuf,
    _lock: File,
}
impl Store {
    pub fn acquire(path: &Path, create: bool) -> Result<Self, String> {
        if create {
            fs::create_dir(path)
                .map_err(|e| format!("cannot create state directory {}: {e}", path.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                    .map_err(|e| e.to_string())?;
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut lock = options
            .open(path.join("writer.lock"))
            .map_err(|e| format!("state unavailable: {e}"))?;
        lock.try_lock()
            .map_err(|e| format!("saved run already has an active writer: {e}"))?;
        lock.set_len(0).map_err(|e| e.to_string())?;
        writeln!(lock, "{}", std::process::id()).map_err(|e| e.to_string())?;
        lock.sync_all().map_err(|e| e.to_string())?;
        Ok(Self {
            path: path.to_owned(),
            _lock: lock,
        })
    }
    pub fn load(path: &Path) -> Result<State, String> {
        let bytes =
            fs::read(path.join("state.json")).map_err(|e| format!("cannot read state: {e}"))?;
        let mut state: State =
            serde_json::from_slice(&bytes).map_err(|e| format!("corrupt run state: {e}"))?;
        state.validate()?;
        let journal = path.join(format!("journal-{}.ndjson", state.revision));
        let bytes = match fs::read(&journal) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("cannot read completion journal: {e}")),
        };
        if !bytes.is_empty() {
            // An incomplete last record is the only tolerated corruption.
            let end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
            for line in bytes[..end]
                .split(|b| *b == b'\n')
                .filter(|s| !s.is_empty())
            {
                let event: Journal = serde_json::from_slice(line)
                    .map_err(|e| format!("corrupt completion journal: {e}"))?;
                if event.run_id != state.run_id || event.revision != state.revision {
                    return Err("journal identity mismatch".into());
                }
                let i = state
                    .rows
                    .iter()
                    .position(|r| r.execution_id == event.row.execution_id)
                    .ok_or("journal execution mismatch")?;
                if state.rows[i].path != event.row.path
                    || state.rows[i].case_id != event.row.case_id
                {
                    return Err("journal plan mismatch".into());
                }
                state.replace_row(i, event.row);
            }
        }
        // Preserve unrelated/global errors from the snapshot; row messages in
        // the active journal are reconciled via the same splice helper.
        state.validate()?;
        Ok(state)
    }
    pub fn save(&self, state: &State) -> Result<(), String> {
        state.validate()?;
        let bytes = serde_json::to_vec(state).map_err(|e| e.to_string())?;
        let staging = self
            .path
            .join(format!("state-{}.new", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = private_open(&staging, true).map_err(|e| e.to_string())?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            fs::rename(&staging, self.path.join("state.json")).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            File::open(&self.path)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(staging);
        }
        result?;
        // The new snapshot includes every acknowledged completion. Once its
        // rename is durable, old journals retain no authority and only leak
        // superseded response bodies / waste disk.
        for entry in fs::read_dir(&self.path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            if let Some(revision) = name
                .to_str()
                .and_then(|n| n.strip_prefix("journal-"))
                .and_then(|n| n.strip_suffix(".ndjson"))
                .and_then(|n| n.parse::<u64>().ok())
                && revision != state.revision
            {
                fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
    pub fn append(&self, state: &State, row: &SavedRow) -> Result<(), String> {
        let event = Journal {
            run_id: state.run_id.clone(),
            revision: state.revision,
            row: row.clone(),
        };
        let path = self.path.join(format!("journal-{}.ndjson", state.revision));
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(path).map_err(|e| e.to_string())?;
        let mut bytes = serde_json::to_vec(&event).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        f.write_all(&bytes)
            .and_then(|_| f.sync_data())
            .map_err(|e| e.to_string())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    run_id: String,
    revision: u64,
    row: SavedRow,
}
fn private_open(path: &Path, new: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(new);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::run::{DryRunner, EntryRunner, run_flow_raw, run_flow_raw_selected};
    use std::sync::Mutex;

    struct Fixture {
        calls: Mutex<Vec<(String, String, String)>>,
    }
    impl EntryRunner for Fixture {
        fn run(
            &self,
            base: &crate::hurl::HurlEntry,
            vars: &HashMap<String, String>,
        ) -> crate::hurl::RunOutput {
            let mut out = DryRunner.run(base, vars);
            if base.title == "setup" {
                out.entries[0].captures = vec![
                    ("session_id".into(), "stale-capture".into()),
                    ("token".into(), "CAPTURE_SECRET".into()),
                ];
                return out;
            }
            let target = vars["target"].clone();
            let key = vars["case_id"].clone();
            let binding = vars["session_id"].clone();
            self.calls
                .lock()
                .unwrap()
                .push((target.clone(), key, binding.clone()));
            let bad = target == "candidate" && binding != "fresh";
            let response = if bad || binding == "baseline-wrong" {
                "wrong"
            } else {
                "right"
            };
            let eo = &mut out.entries[0];
            eo.status = if bad { 400 } else { 200 };
            eo.body = response.into();
            eo.raw_body = response.into();
            eo.ok = !bad;
            eo.error = bad.then(|| "fixture rejected binding".into());
            out.error = eo.error.clone();
            out
        }
    }
    fn context<'a>(
        entries: &'a [crate::hurl::HurlEntry],
        runner: &'a dyn EntryRunner,
    ) -> RunContext<'a> {
        RunContext {
            entries,
            helpers: &[],
            base_vars: HashMap::from([("token".into(), "ENV_SECRET".into())]),
            named_envs: ["baseline", "candidate", "other"]
                .into_iter()
                .map(|n| (n.into(), HashMap::from([("target".into(), n.into())])))
                .collect(),
            root: None,
            runner,
            strings: crate::i18n::Strings::english(),
            params: HashMap::new(),
            sink: None,
            shuffle: None,
            cancel: None,
        }
    }
    fn source(truth: bool) -> String {
        format!(
            "{}\nFOR ENV IN ENVS BASELINE(\"baseline\"), COMPARISON(\"candidate\",\"other\")\n\
            FOR (case_id, session_id) IN [(\"a\",\"old\"),(\"b\",\"old\")]\n\
            REQUEST setup\nREPORT REQUEST check HIDE(HttpStatus,Response)\nEND\nEND\n",
            if truth {
                "# columns: case_id, check.answer TRUTH \"right\", Result, Correct, Trend, check.Time STATISTICS(MEAN)"
            } else {
                ""
            }
        )
    }
    fn entries() -> Vec<crate::hurl::HurlEntry> {
        crate::hurl::parse_hurl(
            "# setup\nGET http://fixture.invalid/setup\nHTTP 200\n[Captures]\nsession_id: body\ntoken: body\n\n# check\nGET http://fixture.invalid/{{session_id}}\nHTTP 200\n# [Reports]\n# answer: body\n",
        )
    }
    fn options() -> Options {
        Options {
            case_key: Some("case_id".into()),
            execution_binding: vec!["session_id".into()],
            ..Default::default()
        }
    }
    fn completed(truth: bool) -> (State, ReportFlow, Vec<crate::hurl::HurlEntry>, Fixture) {
        let flow = super::super::parser::parse_flow(&source(truth)).unwrap();
        let entries = entries();
        let fixture = Fixture {
            calls: Mutex::new(Vec::new()),
        };
        let dry = context(&entries, &DryRunner);
        let shape = run_flow_raw(&flow, &dry);
        let mut state = State::create(&flow, &dry, &shape, &options()).unwrap();
        let ctx = context(&entries, &fixture);
        let selection = state.selection(&(0..state.rows.len()).collect::<Vec<_>>(), HashMap::new());
        let result = run_flow_raw_selected(&flow, &ctx, Some(&selection));
        state.merge(&result, &state.names().unwrap()).unwrap();
        state.phase = "complete".into();
        (state, flow, entries, fixture)
    }

    #[test]
    fn saved_exact_candidate_and_baseline_preserve_other_raw_sides() {
        let (mut state, flow, entries, fixture) = completed(false);
        state = serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        assert_eq!(state.rows.len(), 6);
        let candidate = state
            .rows
            .iter()
            .position(|r| r.key == ["a"] && r.target.as_deref() == Some("candidate"))
            .unwrap();
        let before = serde_json::to_value(&state.rows).unwrap();
        let ctx = context(&entries, &fixture);
        fixture.calls.lock().unwrap().clear();
        let selection = state.selection(
            &[candidate],
            HashMap::from([(
                state.rows[candidate].path.clone(),
                HashMap::from([("session_id".into(), "fresh".into())]),
            )]),
        );
        let fresh = run_flow_raw_selected(&flow, &ctx, Some(&selection));
        state.merge(&fresh, &state.names().unwrap()).unwrap();
        assert_eq!(
            *fixture.calls.lock().unwrap(),
            vec![("candidate".into(), "a".into(), "fresh".into())]
        );
        assert_eq!(state.rows[candidate].details["check.HttpStatus"], "200");
        assert_eq!(state.rows[candidate].details["check.Response"], "right");
        assert!(!state.rows[candidate].cells.contains_key("check.HttpStatus"));
        assert_eq!(state.rows[candidate].key, ["a"]);
        assert_eq!(state.errors.len(), 1, "unrelated failed case remains");
        for i in 0..6 {
            if i != candidate {
                assert_eq!(serde_json::to_value(&state.rows[i]).unwrap(), before[i]);
            }
        }
        let baseline = state
            .rows
            .iter()
            .position(|r| r.key == ["a"] && r.role == RowRole::Baseline)
            .unwrap();
        fixture.calls.lock().unwrap().clear();
        let fresh = run_flow_raw_selected(
            &flow,
            &ctx,
            Some(&state.selection(
                &[baseline],
                HashMap::from([(
                    state.rows[baseline].path.clone(),
                    HashMap::from([("session_id".into(), "baseline-wrong".into())]),
                )]),
            )),
        );
        state.merge(&fresh, &state.names().unwrap()).unwrap();
        assert_eq!(fixture.calls.lock().unwrap().len(), 1);
        assert_eq!(fixture.calls.lock().unwrap()[0].0, "baseline");
        let rendered = state.rendered().unwrap();
        assert_eq!(
            rendered.rows.len(),
            4,
            "both candidate comparisons survive baseline-only rerun"
        );
        for row in rendered.rows.iter().filter(|r| r.key == ["a"]) {
            assert_ne!(row.cells["Result"], super::super::compare::MATCH);
        }
        assert_eq!(
            state
                .selected(
                    Action::Rerun,
                    &[state.rows[candidate].execution_id.clone()],
                    true
                )
                .unwrap()
                .len(),
            3
        );
        assert!(
            state
                .selected(Action::Rerun, &["missing".into()], false)
                .is_err()
        );
        assert!(
            state
                .selected(
                    Action::Rerun,
                    &vec![state.rows[candidate].execution_id.clone(); 2],
                    false
                )
                .is_err()
        );
    }

    #[test]
    fn saved_roundtrip_private_rows_images_metadata_and_errors() {
        let (mut state, _, _, _) = completed(true);
        state.rows[0].images.insert(
            "picture".into(),
            ImageData {
                bytes: vec![1, 2, 3],
                mime: "image/png".into(),
                natural: (2, 3),
            },
        );
        let bytes = serde_json::to_vec(&state).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(!text.contains("ENV_SECRET"));
        assert!(!text.contains("CAPTURE_SECRET"));
        assert!(!text.contains("stale-capture"));
        assert!(!text.contains("setup.HttpStatus"));
        let restored: State = serde_json::from_slice(&bytes).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored.rows[0].images["picture"].bytes, [1, 2, 3]);
        assert_eq!(
            serde_json::to_value(&state).unwrap(),
            serde_json::to_value(&restored).unwrap()
        );
        let rendered = restored.rendered().unwrap();
        assert!(!rendered.verdicts.is_empty());
        assert!(
            !rendered.column_stats.is_empty()
                || rendered
                    .resolved_columns(&restored.flow().unwrap().header)
                    .iter()
                    .any(|c| !c.stats.is_empty())
        );
        assert_eq!(rendered.errors.len(), 2);
    }

    #[test]
    fn saved_images_follow_baseline_sources_and_blank_partial_cells() {
        let (mut state, _, _, _) = completed(false);
        state.header = "# name: images\n# columns: baseline.check.answer AS Picture IMAGE\n".into();
        state.baseline_show = vec!["answer".into()];
        for row in &mut state.rows {
            row.images.insert(
                "Picture".into(),
                ImageData {
                    bytes: vec![if row.role == RowRole::Baseline { 1 } else { 2 }],
                    mime: "image/png".into(),
                    natural: (1, 1),
                },
            );
        }
        let rendered = state.rendered().unwrap();
        assert_eq!(rendered.images.len(), 4);
        assert!(rendered.images.values().all(|image| image.bytes == [1]));

        state.rows.last_mut().unwrap().status = Status::Pending;
        assert!(
            state.rendered().unwrap().images.is_empty(),
            "uncollapsed rows must not display their own image as the baseline"
        );
        state.rows.last_mut().unwrap().status = Status::Success;
        for row in &mut state.rows {
            if row.role == RowRole::Baseline {
                row.cells
                    .insert("check.answer".into(), state.no_match_marker.clone());
            }
        }
        assert!(
            state.rendered().unwrap().images.is_empty(),
            "missing-value cells must not retain an image"
        );
    }

    #[test]
    fn saved_baseline_rerun_recomputes_all_dependent_truth_trends() {
        let (mut state, flow, entries, fixture) = completed(true);
        let index = state
            .rows
            .iter()
            .position(|r| r.role == RowRole::Baseline && r.key == ["a"])
            .unwrap();
        let candidates: Vec<_> = state
            .rows
            .iter()
            .filter(|r| r.role == RowRole::Candidate)
            .map(|r| serde_json::to_value(r).unwrap())
            .collect();
        fixture.calls.lock().unwrap().clear();
        let selection = state.selection(
            &[index],
            HashMap::from([(
                state.rows[index].path.clone(),
                HashMap::from([("session_id".into(), "baseline-wrong".into())]),
            )]),
        );
        let fresh = run_flow_raw_selected(&flow, &context(&entries, &fixture), Some(&selection));
        state.merge(&fresh, &state.names().unwrap()).unwrap();
        assert_eq!(fixture.calls.lock().unwrap().len(), 1);
        assert_eq!(
            candidates,
            state
                .rows
                .iter()
                .filter(|r| r.role == RowRole::Candidate)
                .map(|r| serde_json::to_value(r).unwrap())
                .collect::<Vec<_>>()
        );
        let result = state.rendered().unwrap();
        for (i, row) in result
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.key == ["a"])
        {
            let trend = result.trends[&(i, "check.answer".into())];
            if row.target.as_deref() == Some("other") {
                assert_eq!(trend, super::super::model::Trend::Fixed);
            } else {
                assert_eq!(trend, super::super::model::Trend::StillWrong);
            }
        }
    }

    #[test]
    fn saved_replaced_row_reconciles_shared_setup_and_skips() {
        let (mut state, _, _, _) = completed(false);
        state.rows[0].errors = vec!["shared setup".into()];
        state.rows[1].errors = vec!["shared setup".into()];
        state.errors.push("shared setup".into());
        state.rows[0].skipped = vec!["dependent".into()];
        state.rows[1].skipped = vec!["dependent".into()];
        state.skipped = vec!["dependent".into()];
        let mut fixed = state.rows[0].raw();
        fixed.errors.clear();
        state
            .merge(
                &ReportResult {
                    rows: vec![fixed],
                    ..Default::default()
                },
                &state.names().unwrap(),
            )
            .unwrap();
        assert!(state.errors.contains(&"shared setup".into()));
        assert_eq!(state.skipped, ["dependent"]);
        let mut fixed = state.rows[1].raw();
        fixed.errors.clear();
        state
            .merge(
                &ReportResult {
                    rows: vec![fixed],
                    ..Default::default()
                },
                &state.names().unwrap(),
            )
            .unwrap();
        assert!(!state.errors.contains(&"shared setup".into()));
        assert!(state.skipped.is_empty());
        assert_eq!(state.errors.len(), 2, "unrelated engine failures survive");
    }

    #[test]
    fn saved_streamed_shared_setup_error_matches_the_final_flat_diagnostics() {
        struct BrokenSetup(Fixture);
        impl EntryRunner for BrokenSetup {
            fn run(
                &self,
                entry: &crate::hurl::HurlEntry,
                vars: &HashMap<String, String>,
            ) -> crate::hurl::RunOutput {
                if entry.title != "setup" {
                    return self.0.run(entry, vars);
                }
                let mut out = DryRunner.run(entry, vars);
                out.error = Some("setup unavailable".into());
                out.entries[0].ok = false;
                out
            }
        }
        let source = format!(
            "REQUEST setup\n{}",
            source(false).replace("REQUEST setup\n", "")
        );
        let flow = super::super::parser::parse_flow(&source).unwrap();
        let entries = entries();
        let runner = BrokenSetup(Fixture {
            calls: Mutex::new(Vec::new()),
        });
        let shape = run_flow_raw(&flow, &context(&entries, &DryRunner));
        let mut state =
            State::create(&flow, &context(&entries, &runner), &shape, &options()).unwrap();
        let raw = run_flow_raw_selected(
            &flow,
            &context(&entries, &runner),
            Some(&state.selection(&(0..6).collect::<Vec<_>>(), HashMap::new())),
        );
        for row in &raw.rows {
            state
                .merge(
                    &ReportResult {
                        rows: vec![row.clone()],
                        ..Default::default()
                    },
                    &state.names().unwrap(),
                )
                .unwrap();
        }
        assert_eq!(state.errors, raw.errors);
        assert_eq!(
            state
                .errors
                .iter()
                .filter(|e| e.contains("setup unavailable"))
                .count(),
            1
        );
    }

    #[test]
    fn saved_target_specific_tuple_values_and_order_do_not_change_case_pairing() {
        let root = PathBuf::from(format!(".saved-corpus-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("baseline.csv"),
            "case_id,session_id\na,baseline-a\nb,baseline-b\n",
        )
        .unwrap();
        fs::write(
            root.join("candidate.csv"),
            "case_id,session_id\nb,candidate-b\na,candidate-a\n",
        )
        .unwrap();
        let flow = super::super::parser::parse_flow(
            "FOR ENV IN ENVS BASELINE(\"baseline\"), COMPARISON(\"candidate\")\n\
             FOR case_id IN TUPLES FROM \"{{ENV}}.csv\"\nREPORT REQUEST check\nEND\nEND\n",
        )
        .unwrap();
        let entries = entries();
        let fixture = Fixture {
            calls: Mutex::new(Vec::new()),
        };
        let mut ctx = context(&entries, &fixture);
        ctx.root = Some(root.clone());
        let mut dry = context(&entries, &DryRunner);
        dry.root = Some(root.clone());
        let shape = run_flow_raw(&flow, &dry);
        let mut state = State::create(&flow, &ctx, &shape, &options()).unwrap();
        let all = (0..4).collect::<Vec<_>>();
        state
            .merge(
                &run_flow_raw_selected(&flow, &ctx, Some(&state.selection(&all, HashMap::new()))),
                &state.names().unwrap(),
            )
            .unwrap();
        state.phase = "complete".into();
        let result = state.rendered().unwrap();
        assert_eq!(result.rows.len(), 2);
        assert!(
            result
                .rows
                .iter()
                .all(|r| r.cells["Result"] != super::super::compare::NO_BASELINE)
        );
        let candidate = state
            .rows
            .iter()
            .position(|r| r.target.as_deref() == Some("candidate") && r.key == ["a"])
            .unwrap();
        let selected = state
            .selected(
                Action::Rerun,
                &[state.rows[candidate].execution_id.clone()],
                true,
            )
            .unwrap();
        fixture.calls.lock().unwrap().clear();
        let selection = state.selection(
            &selected,
            HashMap::from([(
                state.rows[candidate].path.clone(),
                HashMap::from([("session_id".into(), "fresh".into())]),
            )]),
        );
        run_flow_raw_selected(&flow, &ctx, Some(&selection));
        assert_eq!(
            *fixture.calls.lock().unwrap(),
            [
                ("baseline".into(), "a".into(), "baseline-a".into()),
                ("candidate".into(), "a".into(), "fresh".into()),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn saved_journal_resume_keeps_completed_failures_and_reconciles_errors() {
        let (mut state, _, _, _) = completed(false);
        let path = PathBuf::from(format!(".saved-test-{}", uuid::Uuid::new_v4()));
        let store = Store::acquire(&path, true).unwrap();
        state.revision = 4;
        state.phase = "active".into();
        store.save(&state).unwrap();
        let failed = state
            .rows
            .iter()
            .position(|r| r.status == Status::Error)
            .unwrap();
        let mut fixed = state.rows[failed].clone();
        fixed.errors.clear();
        fixed.status = Status::Success;
        store.append(&state, &fixed).unwrap();
        let mut pending = state.rows[0].clone();
        pending.status = Status::InFlight;
        store.append(&state, &pending).unwrap();
        let journal = path.join("journal-4.ndjson");
        let mut file = OpenOptions::new().append(true).open(journal).unwrap();
        file.write_all(b"{\"truncated").unwrap();
        let restored = Store::load(&path).unwrap();
        assert_eq!(restored.errors.len(), 1);
        assert_eq!(restored.selected(Action::Resume, &[], false).unwrap(), [0]);
        assert_eq!(restored.rows[failed].status, Status::Success);
        assert!(Store::acquire(&path, false).is_err());
        drop(store);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn saved_rejects_schema_plan_ambiguity_and_unsafe_bindings() {
        let (mut state, flow, entries, _) = completed(false);
        let ctx = context(&entries, &DryRunner);
        let shape = run_flow_raw(&flow, &ctx);
        state.validate_plan(&flow, &ctx, &shape).unwrap();
        state.schema = 99;
        assert!(state.validate().unwrap_err().contains("schema"));
        state.schema = 1;
        let mut changed = flow.clone();
        changed.nodes.push(FlowNode::Comment("different".into()));
        assert!(state.validate_plan(&changed, &ctx, &shape).is_err());
        let mut duplicate = shape.clone();
        duplicate.rows[1].vars.insert("case_id".into(), "a".into());
        assert!(State::create(&flow, &ctx, &duplicate, &options()).is_err());
        let mut unsafe_options = options();
        unsafe_options.execution_binding = vec!["case_id".into()];
        assert!(State::create(&flow, &ctx, &shape, &unsafe_options).is_err());
        let dynamic = super::super::parser::parse_flow(
            "REQUEST setup\nFOR case_id IN FILES \"{{session_id}}\"\nREPORT REQUEST check\nEND\n",
        )
        .unwrap();
        assert!(validate_supported(&dynamic, &ctx, "case_id", &[]).is_err());
    }

    /// Whether every entry parsed is a property of the *collection*, not of
    /// anything the report says, so the author of the report has no way to find
    /// the culprit unless the error names it — and the usual cause is a couple
    /// of unused requests in a large Postman export.
    #[test]
    fn saved_image_settings_round_trip_and_default_for_older_bundles() {
        let (_, flow, entries, _) = completed(false);
        let ctx = context(&entries, &DryRunner);
        let mut shape = run_flow_raw(&flow, &ctx);
        shape.image_max_edge = Some(160);
        shape.image_quality = Some(40);
        let state = State::create(&flow, &ctx, &shape, &options()).unwrap();
        let mut json = serde_json::to_value(&state).unwrap();
        let restored: State = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(restored.raw().image_max_edge, Some(160));
        assert_eq!(restored.raw().image_quality, Some(40));
        json.as_object_mut().unwrap().remove("image_max_edge");
        json.as_object_mut().unwrap().remove("image_quality");
        let older: State = serde_json::from_value(json).unwrap();
        assert_eq!(older.image_max_edge, None);
        assert_eq!(older.image_quality, None);
    }

    #[test]
    fn saved_branch_dependencies_include_files_and_origins_from_both_arms() {
        let dir =
            std::env::temp_dir().join(format!("paperboy-branch-inputs-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("document.bin"), b"original").unwrap();
        fs::write(dir.join("other.bin"), b"other").unwrap();
        let entries = crate::hurl::parse_hurl(
            "# probe\nGET http://fixture.invalid/\nHTTP 200\n\n\
             # upload\nPOST https://{{host}}/upload\n[Multipart]\nfile: file,{{input}};\nHTTP 200\n",
        );
        let runner = super::super::run::LiveRunner {
            file_root: Some(dir.clone()),
        };
        let mut ctx = context(&entries, &runner);
        ctx.base_vars.insert("host".into(), "first.invalid".into());
        let opts = Options {
            case_key: Some("case_id".into()),
            ..Default::default()
        };
        for branch in [
            "IF \"x\" == \"x\"\nREQUEST upload\nELSE\nREQUEST upload USING(multipart.file = \"other.bin\")\nEND",
            "TRY\nREQUEST probe\nRECOVER failure\nIF \"x\" == \"x\"\nREQUEST upload\nELSE\nRETHROW\nEND\nEND",
            "IF \"x\" == \"y\"\nCLEANUP upload\nEND",
        ] {
            let flow = super::super::parser::parse_flow(&format!(
                "FOR (case_id,input) IN [(\"one\",\"document.bin\")]\n{branch}\nREPORT case_id\nEND\n"
            )).unwrap();
            let shape = run_flow_raw(&flow, &context(&entries, &DryRunner));
            let state = State::create(&flow, &ctx, &shape, &opts).unwrap();
            assert!(
                state
                    .input_hashes
                    .keys()
                    .any(|k| k.ends_with("document.bin"))
            );
            if branch.contains("other.bin") {
                assert!(state.input_hashes.keys().any(|k| k.ends_with("other.bin")));
            }
            fs::write(dir.join("document.bin"), b"changed").unwrap();
            assert!(state.validate_plan(&flow, &ctx, &shape).is_err());
            fs::write(dir.join("document.bin"), b"original").unwrap();
            ctx.base_vars.insert("host".into(), "second.invalid".into());
            assert!(state.validate_plan(&flow, &ctx, &shape).is_err());
            ctx.base_vars.insert("host".into(), "first.invalid".into());
            fs::remove_file(dir.join("document.bin")).unwrap();
            let error = State::create(&flow, &ctx, &shape, &opts).unwrap_err();
            assert!(error.contains("cannot hash input"), "{error}");
            fs::write(dir.join("document.bin"), b"original").unwrap();
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saved_branch_dependencies_reject_dynamic_paths_even_with_old_values() {
        let entries = crate::hurl::parse_hurl(
            "# probe\nGET http://fixture.invalid/\nHTTP 200\n[Captures]\npath: body\n\n\
             # upload\nPOST https://fixture.invalid/upload\n[Multipart]\nfile: file,{{path}};\nHTTP 200\n",
        );
        let mut ctx = context(&entries, &DryRunner);
        ctx.base_vars
            .insert("path".into(), "old-document.bin".into());
        let opts = Options {
            case_key: Some("case_id".into()),
            ..Default::default()
        };
        for body in [
            "TRY\nREQUEST probe\nRECOVER failure\nREQUEST upload\nEND",
            "IF \"x\" == \"x\"\nREQUEST upload USING(multipart.file = \"{{path}}\")\nEND",
            "IF \"x\" == \"x\"\nREQUEST probe USING(url = \"https://{{path}}/upload\")\nEND",
        ] {
            let flow = super::super::parser::parse_flow(&format!(
                "FOR case_id IN [\"one\"]\n{body}\nREPORT case_id\nEND\n"
            ))
            .unwrap();
            let shape = run_flow_raw(&flow, &ctx);
            let error = State::create(&flow, &ctx, &shape, &opts).unwrap_err();
            assert!(
                error.contains("fingerprint") || error.contains("endpoint origins"),
                "{error}"
            );
        }
    }

    #[test]
    fn saved_interrupted_branches_remain_resumable() {
        use super::super::run::{Cancel, RowEvent};
        struct Stopping<'a> {
            cancel: &'a Cancel,
            stop_at: &'a str,
            calls: Mutex<Vec<String>>,
        }
        impl EntryRunner for Stopping<'_> {
            fn run(
                &self,
                entry: &crate::hurl::HurlEntry,
                vars: &HashMap<String, String>,
            ) -> crate::hurl::RunOutput {
                self.calls.lock().unwrap().push(entry.title.clone());
                let mut out = DryRunner.run(entry, vars);
                if entry.title == "fail" {
                    out.error = Some("missing".into());
                    out.entries[0].ok = false;
                    out.entries[0].error = out.error.clone();
                }
                if entry.title == self.stop_at {
                    self.cancel.stop();
                }
                out
            }
        }
        let entries = crate::hurl::parse_hurl(
            "# first\nGET http://fixture.invalid/first\n\n# upload\nGET http://fixture.invalid/upload\n\n\
             # fail\nGET http://fixture.invalid/fail\n\n# clean\nGET http://fixture.invalid/clean\n",
        );
        for branch in [
            "TRY\nCLEANUP clean\nREQUEST first\nREQUEST upload\nRECOVER failure\nRETHROW\nEND",
            "IF \"x\" == \"x\"\nCLEANUP clean\nREQUEST first\nREQUEST upload\nEND",
            "TRY\nREQUEST fail\nRECOVER failure\nCLEANUP clean\nREQUEST first\nREQUEST upload\nEND",
        ] {
            let flow = super::super::parser::parse_flow(&format!(
                "FOR case_id IN [\"one\"]\n{branch}\nREPORT \"done\" AS Status\nEND\n"
            ))
            .unwrap();
            let dry = context(&entries, &DryRunner);
            let shape = run_flow_raw(&flow, &dry);
            let opts = Options {
                case_key: Some("case_id".into()),
                ..Default::default()
            };
            let state = Mutex::new(State::create(&flow, &dry, &shape, &opts).unwrap());
            let names = state.lock().unwrap().names().unwrap();
            let sink = |event: RowEvent| {
                let mut state = state.lock().unwrap();
                match event {
                    RowEvent::Started(_) => state.rows[0].status = Status::InFlight,
                    RowEvent::Completed { row, .. } => state.rows[0].replace(row, &names),
                }
            };
            let cancel = Cancel::new();
            let stopping = Stopping {
                cancel: &cancel,
                stop_at: "first",
                calls: Mutex::new(Vec::new()),
            };
            let mut ctx = context(&entries, &stopping);
            ctx.cancel = Some(&cancel);
            ctx.sink = Some(&sink);
            let interrupted = run_flow_raw(&flow, &ctx);
            assert!(interrupted.rows.is_empty());
            assert_eq!(interrupted.unfinished_rows.len(), 1);
            state.lock().unwrap().merge(&interrupted, &names).unwrap();
            if branch.contains("REQUEST fail") {
                assert!(!state.lock().unwrap().errors.is_empty());
            }
            let calls = stopping.calls.lock().unwrap();
            assert!(calls.iter().any(|n| n == "clean"));
            assert!(!calls.iter().any(|n| n == "upload"));
            drop(calls);
            let mut restored: State =
                serde_json::from_value(serde_json::to_value(&*state.lock().unwrap()).unwrap())
                    .unwrap();
            let selected = restored.selected(Action::Resume, &[], false).unwrap();
            assert_eq!(selected, vec![0]);
            let mut with_global: State =
                serde_json::from_value(serde_json::to_value(&restored).unwrap()).unwrap();
            with_global.errors.push("unrelated global failure".into());
            let selection = restored.selection(&selected, HashMap::new());
            let cancel = Cancel::new();
            let finishing = Stopping {
                cancel: &cancel,
                stop_at: "",
                calls: Mutex::new(Vec::new()),
            };
            let finished =
                run_flow_raw_selected(&flow, &context(&entries, &finishing), Some(&selection));
            restored.merge(&finished, &names).unwrap();
            with_global.merge(&finished, &names).unwrap();
            assert!(restored.errors.is_empty(), "{:?}", restored.errors);
            assert_eq!(with_global.errors, vec!["unrelated global failure"]);
            assert_eq!(restored.rows[0].status, Status::Success);
            assert!(
                finishing
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|n| n == "upload")
            );
            assert!(
                restored
                    .selected(Action::Resume, &[], false)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn saved_conditionals_keep_identity_static_and_report_after_recovery() {
        let entries = entries();
        let ctx = context(&entries, &DryRunner);
        let flow = super::super::parser::parse_flow(
            "FOR (case_id, session_id) IN [(\"a\",\"old\")]\nTRY\nREQUEST setup\nRECOVER failure\nRETHROW\nEND\nREPORT REQUEST check\nEND\n",
        ).unwrap();
        let shape = run_flow_raw(&flow, &ctx);
        State::create(&flow, &ctx, &shape, &options()).unwrap();
        let outer = super::super::parser::parse_flow(
            "TRY\nREQUEST setup\nRECOVER failure\nRETHROW\nEND\nFOR case_id IN [\"one\"]\nREPORT case_id\nEND\n",
        ).unwrap();
        assert!(
            validate_supported(&outer, &ctx, "case_id", &[])
                .unwrap_err()
                .contains("leaf blocks")
        );
        for body in [
            "IF \"a\" == \"a\"\ncase_id=changed\nEND",
            "IF \"a\" == \"a\"\nREPORT case_id\nEND",
        ] {
            let bad = super::super::parser::parse_flow(&format!(
                "FOR (case_id, session_id) IN [(\"a\",\"old\")]\n{body}\nREPORT REQUEST check\nEND\n",
            )).unwrap();
            assert!(validate_supported(&bad, &ctx, "case_id", &[]).is_err());
        }
    }

    #[test]
    fn saved_cache_retry_report_leaf_has_stable_offline_discovery() {
        let entries = entries();
        let ctx = context(&entries, &DryRunner);
        let flow = super::super::parser::parse_flow(
            "FOR case_id IN [\"a\", \"b\"]\nCACHE session\nPERSIST(session_id)\nREQUEST setup\nEND\nTRY\nREPORT REQUEST check\nRECOVER RETRY(1) failure\nCACHE REFRESH session\nEND\nEND\n",
        ).unwrap();
        validate_supported(&flow, &ctx, "case_id", &[]).unwrap();
        let shape = run_flow_raw(&flow, &ctx);
        assert_eq!(shape.rows.len(), 2);
        let state = State::create(&flow, &ctx, &shape, &options()).unwrap();
        assert_eq!(state.rows.len(), 2);
        for protected in [
            "TRY\nREPORT REQUEST check\nRECOVER failure\nEND",
            "TRY\nREPORT REQUEST check\nRECOVER RETRY(1) failure\nREPORT case_id\nEND",
            "TRY\nIF \"a\" == \"b\"\nREPORT REQUEST check\nEND\nRECOVER RETRY(1) failure\nEND",
        ] {
            let bad = super::super::parser::parse_flow(&format!(
                "FOR case_id IN [\"a\"]\n{protected}\nEND"
            ))
            .unwrap();
            assert!(
                validate_supported(&bad, &ctx, "case_id", &[]).is_err(),
                "{protected}"
            );
        }
    }

    #[test]
    fn saved_cache_assignments_cannot_define_case_identity() {
        let entries = entries();
        let ctx = context(&entries, &DryRunner);
        let flow = super::super::parser::parse_flow(
            "CACHE c\nPERSIST(case_id)\ncase_id=dynamic\nEND\nTRY\nREPORT REQUEST check\nRECOVER RETRY(1) failure\nEND",
        ).unwrap();
        assert!(
            validate_supported(&flow, &ctx, "case_id", &[])
                .unwrap_err()
                .contains("case key")
        );
    }

    #[test]
    fn saved_cache_files_are_fingerprinted_without_uploads_or_persistence() {
        let entries = entries();
        let mut ctx = context(&entries, &DryRunner);
        ctx.root = Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        let flow = super::super::parser::parse_flow(
            "DOC=Cargo.toml\nFOR case_id IN [\"a\"]\nCACHE c\nFILES(\"{{DOC}}\")\nPERSIST(session_id)\nREQUEST setup\nEND\nREPORT REQUEST check\nEND",
        ).unwrap();
        let shape = run_flow_raw(&flow, &ctx);
        let hashes = input_hashes(&flow, &ctx, &shape).unwrap();
        let expected = hash(&std::fs::read(ctx.root.as_ref().unwrap().join("Cargo.toml")).unwrap());
        assert!(
            hashes
                .iter()
                .any(|(path, digest)| path.ends_with("Cargo.toml") && digest == &expected)
        );
        assert!(
            validate_supported(&flow, &ctx, "case_id", &["DOC".into()])
                .unwrap_err()
                .contains("cache input file")
        );
    }

    #[test]
    fn an_unparsed_entry_is_named_in_the_error() {
        let (_, flow, entries, _) = completed(false);
        let mut entries = entries.to_vec();
        entries.push(crate::hurl::HurlEntry::unreadable(
            "# upload_face\nPOST {{url}}/upload_face\n[Multipart]\nliveness_image_file: file,;\n",
        ));
        let ctx = context(&entries, &DryRunner);
        let err = validate_supported(&flow, &ctx, "case_id", &[]).unwrap_err();
        assert!(
            err.contains("upload_face"),
            "must name the entry that could not be parsed, got: {err}"
        );
    }
}
