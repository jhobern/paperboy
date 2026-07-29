//! Hurl format support:
//! - [`entry`]: the `HurlEntry` request model + Hurl-text serializer.
//! - [`parser`]: parse Hurl text into `HurlEntry` values.
//! - [`run`]: execute + evaluate via the `hurl` runner (`[Captures]`/`[Asserts]`).
//! - [`stage`]: copy out-of-scope `[Form]`/`[Multipart]` files next to the
//!   run's `file_root` so Hurl's sandbox doesn't reject them.

mod entry;
mod parser;
pub mod run;
mod stage;

pub use entry::{
    FormField, FormFieldKind, HurlEntry, METHODS, RunStatus, collection_to_hurl, method_rgb,
};
pub use parser::{parse_hurl, parse_hurl_error};
pub use run::{AssertOutcome, EntryOutcome, RunOutput, run_hurl, run_hurl_streaming};
pub use stage::{expand_base64_form_fields, stage_out_of_scope_form_files};
