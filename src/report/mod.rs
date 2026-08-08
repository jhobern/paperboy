//! PaperTrail — the report-flow language and engine.
//!
//! A *report flow* (a `.trail` file, in the **PaperTrail** DSL) is written once
//! by a programmer and run repeatedly by a non-technical user: it drives a bound
//! collection against ranges of files/environments and produces a tabular report
//! (CSV first). The design docs live in `docs/reports/` — `02-grammar.md` is the
//! authoritative grammar, `03-examples.md` a worked cookbook, `01-build-
//! breakdown.md` the phased plan.
//!
//! This module is deliberately **front-end agnostic** (no `ratatui`
//! dependencies), mirroring `hurl/`, so a future GUI can reuse it wholesale:
//! - [`flow`]: the AST ([`ReportFlow`]) + its canonical text serializer.
//! - [`parser`]: PaperTrail text → [`ReportFlow`].
//! - [`validate`]: static checks over a flow (+ bound collection / loaded envs).

pub mod baseline;
pub mod compare;
pub mod context;
pub mod dry_run;
pub mod edit;
pub mod flow;
pub mod image;
pub mod indent;
pub mod model;
pub mod parser;
pub mod producers;
// The `Report` domain type lives in `report::report`; the repeated name trips
// clippy::module_inception, but renaming the file would obscure that this is
// *the* report type's home, so the lint is allowed here rather than worked
// around.
#[allow(clippy::module_inception)]
pub mod report;
pub mod run;
pub mod validate;
pub mod writer;

pub use baseline::Baseline;
pub use model::ReportResult;
pub use parser::parse_flow;
pub use report::{Report, expand_output_tokens, name_has_output_token};
pub use writer::{CsvWriter, ReportWriter};
