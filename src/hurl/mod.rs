//! Hurl format support:
//! - [`entry`]: the `HurlEntry` request model + Hurl-text serializer.
//! - [`json_comments`]: strip `//` and `/* */` comments from a JSON body, so a
//!   body can be authored with notes but written out as strict JSON.
//! - [`parser`]: parse Hurl text into `HurlEntry` values.
//! - [`run`]: execute + evaluate via the `hurl` runner (`[Captures]`/`[Asserts]`).
//! - [`stage`]: copy out-of-scope `[Form]`/`[Multipart]` files next to the
//!   run's `file_root` so Hurl's sandbox doesn't reject them.

mod entry;
pub mod json_comments;
mod parser;
pub mod run;
mod stage;

pub use entry::{
    CommentAnchor, EntryComment, FormField, FormFieldKind, HurlEntry, KeyProblem, KvRow, METHODS,
    ParamNameError, PlaceholderProblem, RunStatus, check_parameter_name, collection_to_hurl,
    key_problem, method_rgb, placeholder_problems, status_eq_code, suggest_parameter_name,
    value_problem,
};
pub(crate) use parser::parse_file_form_value;
pub use parser::{parse_hurl, parse_hurl_error};
pub use run::{
    AssertOutcome, EntryOutcome, RunOutput, run_hurl, run_hurl_streaming, run_hurl_streaming_with,
};
pub use stage::{expand_base64_form_fields, stage_out_of_scope_form_files};
