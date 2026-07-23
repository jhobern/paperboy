//! PaperTrail — the report-flow language and engine.
//!
//! A *report flow* (a `.report` file, in the **PaperTrail** DSL) is written once
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

pub mod flow;
pub mod model;
pub mod parser;
pub mod producers;
pub mod run;
pub mod validate;
pub mod writer;

pub use flow::{
    Binder, Element, EnvClause, FlowNode, Header, HeaderLine, Pattern, Producer, ReportFlow,
    ReportStmt, ResponseFmt, WithItem,
};
pub use model::{OutputColumn, ReportResult, ReportRow, TARGET_COLUMN};
pub use parser::{ParseError, parse_flow};
pub use run::{EntryRunner, LiveRunner, RunContext, resolve_title, run_flow};
pub use validate::{Diagnostic, Severity, validate};
pub use writer::{CsvWriter, ReportWriter};
