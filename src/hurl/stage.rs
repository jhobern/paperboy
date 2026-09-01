//! Stages `[Form]`/`[Multipart]` files that live outside a run's `file_root`
//! into a temporary directory alongside it, so Hurl's own sandbox (see
//! [`hurl::util::path::ContextDir`]) doesn't reject them.
//!
//! Hurl only allows a request to read a local file (for a `[Form]`/
//! `[Multipart]` file field, or `[Options] output`) when it resolves inside
//! `file_root` — normally the directory containing the `.hurl`/collection
//! file. That's the right default sandbox, but it means a File-kind Form
//! field pointing at a file that isn't collocated with the collection (e.g.
//! picked from `~/Downloads`) is always rejected as "Unauthorized file
//! access", regardless of how the path is written. Rather than widen the
//! sandbox (which would defeat its purpose), we copy the handful of files a
//! request actually references into a fresh temp directory that becomes the
//! run's `file_root` for that one run — invisible to the user, and skipped
//! entirely when every referenced file is already in scope.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::STANDARD};
use hurl::util::path::ContextDir;

use super::entry::{FormFieldKind, HurlEntry};

/// If every `[Form]`/`[Multipart]` File field across `entries` already
/// resolves inside `file_root` (Hurl's own authorization rule), returns
/// `Ok(None)` and leaves `entries` untouched — nothing needs staging.
/// Otherwise, copies every referenced file (in-scope ones too, so they all
/// end up together under one new root) into a fresh temporary directory,
/// rewrites each affected field's `value` in place to just the copy's file
/// name, and returns `Ok(Some(staging_dir))`. The caller should run with
/// `file_root` set to that directory and remove it once the run finishes
/// (e.g. via [`std::fs::remove_dir_all`]); best-effort cleanup is fine since
/// it's under the OS temp directory regardless.
///
/// On any I/O error partway through staging (a referenced file missing or
/// unreadable, temp dir creation failing, ...), returns `Err` and leaves
/// `entries` as they were left — the caller should fall back to running
/// unstaged so Hurl's own error (e.g. "no such file") still surfaces
/// normally, rather than silently swallowing the problem.
pub fn stage_out_of_scope_form_files(
    entries: &mut [HurlEntry],
    file_root: Option<&Path>,
) -> io::Result<Option<PathBuf>> {
    let current_dir = std::env::current_dir().unwrap_or_default();
    let root = file_root
        .map(PathBuf::from)
        .unwrap_or_else(|| current_dir.clone());
    let ctx = ContextDir::new(&current_dir, &root);

    let out_of_scope = entries.iter().any(|e| {
        e.form_fields.iter().any(|f| {
            f.kind == FormFieldKind::File
                && !f.value.trim().is_empty()
                && !ctx.is_access_allowed(Path::new(f.value.trim()))
        })
    });
    if !out_of_scope {
        return Ok(None);
    }

    let stage_dir =
        std::env::temp_dir().join(format!("paperboy-form-stage-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&stage_dir)?;

    // Cache by resolved source path so the same file referenced from more
    // than one field (or entry) is only copied once, and every reference
    // ends up pointing at the same staged copy.
    let mut staged: HashMap<PathBuf, String> = HashMap::new();
    let mut used_names: HashSet<String> = HashSet::new();

    for entry in entries.iter_mut() {
        for f in entry.form_fields.iter_mut() {
            if f.kind != FormFieldKind::File || f.value.trim().is_empty() {
                continue;
            }
            let raw = f.value.trim();
            let source = if Path::new(raw).is_absolute() {
                PathBuf::from(raw)
            } else {
                root.join(raw)
            };
            let staged_name = match staged.get(&source) {
                Some(name) => name.clone(),
                None => {
                    let base = source
                        .file_name()
                        .map_or_else(|| "file".to_string(), |n| n.to_string_lossy().to_string());
                    let name = unique_name(&base, &mut used_names);
                    if let Err(e) = std::fs::copy(&source, stage_dir.join(&name)) {
                        let _ = std::fs::remove_dir_all(&stage_dir);
                        // Name the source path (a missing form file listed by a
                        // loop is the common case); keep `ErrorKind` intact.
                        return Err(io::Error::new(
                            e.kind(),
                            format!("{}: {e}", source.display()),
                        ));
                    }
                    staged.insert(source.clone(), name.clone());
                    name
                }
            };
            f.value = staged_name;
        }
    }

    Ok(Some(stage_dir))
}

/// Expands every `Base64File` form field in `entries` into a plain `Text`
/// field, in place, so an actual request never sees PaperBoy's
/// PaperBoy-specific `Base64File` kind (Hurl has no equivalent). For each
/// such field the referenced file is read, base64-encoded (standard alphabet,
/// no line breaks — the `base64` crate never wraps), and the field's value is
/// set to `base64_prefix` followed by that encoding; its kind becomes `Text`
/// and its `base64_prefix`/`content_type` are cleared. A field whose path is
/// empty encodes to just its prefix (nothing to read). Paths resolve the same
/// way file staging resolves them: absolute as-is, relative against
/// `file_root` (or the process's current directory when none is given).
///
/// Reading is done by PaperBoy itself, not Hurl, so — unlike `File` fields —
/// there's no sandbox to satisfy and out-of-scope files work everywhere.
/// Call this *before* [`stage_out_of_scope_form_files`] so staging only ever
/// sees the resulting `Text`/`File` fields. Returns `Err` (leaving already
/// expanded fields expanded) if a referenced file can't be read, so the
/// caller can surface a clear error instead of sending a half-formed request.
pub fn expand_base64_form_fields(
    entries: &mut [HurlEntry],
    file_root: Option<&Path>,
) -> io::Result<usize> {
    let current_dir = std::env::current_dir().unwrap_or_default();
    let root = file_root
        .map(PathBuf::from)
        .unwrap_or_else(|| current_dir.clone());

    let mut entry_count = 0;
    for entry in entries.iter_mut() {
        entry.is_multipart = false;
        for f in entry.form_fields.iter_mut() {
            if f.kind.is_multipart() {
                entry.is_multipart = true;
            }
            if f.kind != FormFieldKind::Base64File {
                continue;
            }
            let prefix = f.base64_prefix.clone().unwrap_or_default();
            let raw = f.value.trim();
            let encoded = if raw.is_empty() {
                String::new()
            } else {
                let source = if Path::new(raw).is_absolute() {
                    PathBuf::from(raw)
                } else {
                    root.join(raw)
                };
                // Name the path in the error (a bare `io::Error` doesn't):
                // during a report run a file listed by a `FOR FILE` loop can be
                // deleted between planning and the send, and the run surfaces
                // this as a non-fatal per-row error — but only usefully if it
                // says *which* file vanished. Preserve `ErrorKind` so callers
                // can still detect `NotFound`.
                let bytes = std::fs::read(&source)
                    .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", source.display())))?;
                STANDARD.encode(bytes)
            };
            f.value = format!("{prefix}{encoded}");
            f.kind = FormFieldKind::Text;
            f.base64_prefix = None;
            f.content_type = None;
            entry_count += 1;
        }
    }
    Ok(entry_count)
}

/// Returns `base` unchanged the first time it's seen; on every subsequent
/// collision, inserts a `_N` counter before the extension (or at the end,
/// if there isn't one) so two different source files that happen to share
/// a name don't overwrite each other in the staging directory.
///
/// A bare counter isn't enough, because the name it invents can itself be
/// the real name of another source file: two `report.csv`s produce
/// `report_1.csv`, and a third field may genuinely reference a file called
/// `report_1.csv`. Both would be copied to the same staged path and one
/// field would silently send the other's bytes, so we keep counting until
/// we land on a name nothing else has claimed.
fn unique_name(base: &str, used: &mut HashSet<String>) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    let mut n: u32 = 1;
    loop {
        let candidate = match base.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem}_{n}.{ext}"),
            None => format!("{base}_{n}"),
        };
        if used.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hurl::entry::FormField;

    fn file_field(key: &str, value: &str) -> FormField {
        FormField {
            key: key.to_string(),
            value: value.to_string(),
            kind: FormFieldKind::File,
            content_type: None,
            base64_prefix: None,
            enabled: true,
            desc: String::new(),
        }
    }

    fn entry_with_form(fields: Vec<FormField>) -> HurlEntry {
        HurlEntry {
            form_fields: fields,
            ..Default::default()
        }
    }

    #[test]
    fn does_nothing_when_every_file_is_already_in_scope() {
        let dir = std::env::temp_dir().join(format!(
            "paperboy_stage_test_inscope_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("avatar.png"), b"fake").unwrap();

        let mut entries = vec![entry_with_form(vec![file_field("avatar", "avatar.png")])];
        let result = stage_out_of_scope_form_files(&mut entries, Some(&dir)).unwrap();

        assert!(
            result.is_none(),
            "nothing needs staging when the file is already under file_root"
        );
        assert_eq!(
            entries[0].form_fields[0].value, "avatar.png",
            "the field is left untouched"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn copies_an_out_of_scope_file_into_a_staging_dir_and_rewrites_its_value() {
        let collection_dir = std::env::temp_dir().join(format!(
            "paperboyman_stage_test_coll_{}",
            uuid::Uuid::new_v4()
        ));
        let elsewhere = std::env::temp_dir().join(format!(
            "paperboytage_test_elsewhere_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&collection_dir).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let source = elsewhere.join("avatar.png");
        std::fs::write(&source, b"fake-png-bytes").unwrap();

        let mut entries = vec![entry_with_form(vec![file_field(
            "avatar",
            source.to_str().unwrap(),
        )])];
        let staged_dir = stage_out_of_scope_form_files(&mut entries, Some(&collection_dir))
            .unwrap()
            .expect("a file outside file_root must trigger staging");

        assert_eq!(
            entries[0].form_fields[0].value, "avatar.png",
            "the field now points at just the staged file name"
        );
        let staged_path = staged_dir.join("avatar.png");
        assert!(
            staged_path.is_file(),
            "the file must actually be copied into the staging dir"
        );
        assert_eq!(std::fs::read(&staged_path).unwrap(), b"fake-png-bytes");

        std::fs::remove_dir_all(&collection_dir).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
        std::fs::remove_dir_all(&staged_dir).ok();
    }

    #[test]
    fn staging_also_brings_along_already_in_scope_files_so_one_root_covers_everything() {
        let collection_dir = std::env::temp_dir().join(format!(
            "paperboyan_stage_test_mixed_{}",
            uuid::Uuid::new_v4()
        ));
        let elsewhere = std::env::temp_dir().join(format!(
            "paperboytage_test_mixed_out_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&collection_dir).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(collection_dir.join("in_scope.txt"), b"in").unwrap();
        std::fs::write(elsewhere.join("out_of_scope.txt"), b"out").unwrap();

        let mut entries = vec![entry_with_form(vec![
            file_field("a", "in_scope.txt"),
            file_field("b", elsewhere.join("out_of_scope.txt").to_str().unwrap()),
        ])];
        let staged_dir = stage_out_of_scope_form_files(&mut entries, Some(&collection_dir))
            .unwrap()
            .unwrap();

        assert!(
            staged_dir.join("in_scope.txt").is_file(),
            "the already-in-scope file is copied too"
        );
        assert!(staged_dir.join("out_of_scope.txt").is_file());
        assert_eq!(entries[0].form_fields[0].value, "in_scope.txt");
        assert_eq!(entries[0].form_fields[1].value, "out_of_scope.txt");

        std::fs::remove_dir_all(&collection_dir).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
        std::fs::remove_dir_all(&staged_dir).ok();
    }

    #[test]
    fn two_different_source_files_sharing_a_name_are_disambiguated() {
        let collection_dir = std::env::temp_dir().join(format!(
            "paperboybman_stage_test_dup_{}",
            uuid::Uuid::new_v4()
        ));
        let a_dir = std::env::temp_dir().join(format!(
            "paperboyan_stage_test_dup_a_{}",
            uuid::Uuid::new_v4()
        ));
        let b_dir = std::env::temp_dir().join(format!(
            "paperboyan_stage_test_dup_b_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&collection_dir).unwrap();
        std::fs::create_dir_all(&a_dir).unwrap();
        std::fs::create_dir_all(&b_dir).unwrap();
        std::fs::write(a_dir.join("photo.png"), b"AAA").unwrap();
        std::fs::write(b_dir.join("photo.png"), b"BBB").unwrap();

        let mut entries = vec![entry_with_form(vec![
            file_field("a", a_dir.join("photo.png").to_str().unwrap()),
            file_field("b", b_dir.join("photo.png").to_str().unwrap()),
        ])];
        let staged_dir = stage_out_of_scope_form_files(&mut entries, Some(&collection_dir))
            .unwrap()
            .unwrap();

        let val_a = entries[0].form_fields[0].value.clone();
        let val_b = entries[0].form_fields[1].value.clone();
        assert_ne!(
            val_a, val_b,
            "two different source files sharing a name must not collide in the staging dir"
        );
        assert_eq!(std::fs::read(staged_dir.join(&val_a)).unwrap(), b"AAA");
        assert_eq!(std::fs::read(staged_dir.join(&val_b)).unwrap(), b"BBB");

        std::fs::remove_dir_all(&collection_dir).ok();
        std::fs::remove_dir_all(&a_dir).ok();
        std::fs::remove_dir_all(&b_dir).ok();
        std::fs::remove_dir_all(&staged_dir).ok();
    }

    #[test]
    fn the_same_source_file_referenced_twice_is_only_copied_once() {
        let collection_dir = std::env::temp_dir().join(format!(
            "paperboyman_stage_test_same_{}",
            uuid::Uuid::new_v4()
        ));
        let elsewhere = std::env::temp_dir().join(format!(
            "paperboystage_test_same_out_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&collection_dir).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let source = elsewhere.join("shared.bin");
        std::fs::write(&source, b"shared-bytes").unwrap();

        let mut entries = vec![
            entry_with_form(vec![file_field("a", source.to_str().unwrap())]),
            entry_with_form(vec![file_field("b", source.to_str().unwrap())]),
        ];
        let staged_dir = stage_out_of_scope_form_files(&mut entries, Some(&collection_dir))
            .unwrap()
            .unwrap();

        assert_eq!(
            entries[0].form_fields[0].value, entries[1].form_fields[0].value,
            "both references reuse the one staged copy"
        );
        assert_eq!(
            entries[0].form_fields[0].value, "shared.bin",
            "no spurious _1 suffix for a single shared source"
        );

        std::fs::remove_dir_all(&collection_dir).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
        std::fs::remove_dir_all(&staged_dir).ok();
    }

    #[test]
    fn a_missing_source_file_returns_an_error_and_cleans_up_the_staging_dir() {
        let collection_dir = std::env::temp_dir().join(format!(
            "paperboy_stage_test_missing_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&collection_dir).unwrap();
        let missing = std::env::temp_dir().join(format!(
            "paperboyge_test_missing_src_{}.bin",
            uuid::Uuid::new_v4()
        ));

        let mut entries = vec![entry_with_form(vec![file_field(
            "a",
            missing.to_str().unwrap(),
        )])];
        let result = stage_out_of_scope_form_files(&mut entries, Some(&collection_dir));
        let err = result
            .expect_err("a missing source file must surface as an error, not be silently skipped");
        assert_eq!(
            err.kind(),
            io::ErrorKind::NotFound,
            "the io ErrorKind must be preserved so callers can detect NotFound"
        );
        assert!(
            err.to_string().contains(missing.to_str().unwrap()),
            "the error must name the missing file, got: {err}"
        );

        std::fs::remove_dir_all(&collection_dir).ok();
    }

    #[test]
    fn a_missing_base64_file_errors_with_notfound_naming_the_path() {
        let dir = std::env::temp_dir().join(format!(
            "paperboy_stage_test_b64_missing_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // A file that was listed (e.g. by a FOR FILE loop) but has since
        // vanished before the send.
        let gone = dir.join("vanished.png");

        let mut entries = vec![entry_with_form(vec![FormField {
            key: "image".to_string(),
            value: gone.to_str().unwrap().to_string(),
            kind: FormFieldKind::Base64File,
            content_type: None,
            base64_prefix: Some("data:image/png;base64,".to_string()),
            enabled: true,
            desc: String::new(),
        }])];
        let err = expand_base64_form_fields(&mut entries, Some(&dir))
            .expect_err("a missing base64 file must surface as a non-fatal error");
        assert_eq!(
            err.kind(),
            io::ErrorKind::NotFound,
            "ErrorKind must be preserved as NotFound"
        );
        assert!(
            err.to_string().contains("vanished.png"),
            "the error must name the missing file, got: {err}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn text_only_form_fields_never_trigger_staging() {
        let collection_dir = std::env::temp_dir().join(format!(
            "paperboystage_test_textonly_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&collection_dir).unwrap();
        let mut entries = vec![entry_with_form(vec![FormField {
            key: "name".to_string(),
            value: "/completely/unrelated/path".to_string(),
            kind: FormFieldKind::Text,
            content_type: None,
            base64_prefix: None,
            enabled: true,
            desc: String::new(),
        }])];
        let result = stage_out_of_scope_form_files(&mut entries, Some(&collection_dir)).unwrap();
        assert!(
            result.is_none(),
            "a Text-kind field's value is never a file path, so it must never trigger staging"
        );

        std::fs::remove_dir_all(&collection_dir).ok();
    }

    #[test]
    fn expands_a_base64_file_field_into_prefixed_base64_text() {
        let dir =
            std::env::temp_dir().join(format!("paperboy_b64_expand_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // Bytes chosen so their base64 is well known: "hi" -> "aGk=".
        std::fs::write(dir.join("blob.bin"), b"hi").unwrap();

        let mut entries = vec![entry_with_form(vec![FormField {
            key: "avatar".to_string(),
            value: "blob.bin".to_string(),
            kind: FormFieldKind::Base64File,
            content_type: None,
            base64_prefix: Some("data:x;base64,".to_string()),
            enabled: true,
            desc: String::new(),
        }])];
        expand_base64_form_fields(&mut entries, Some(&dir)).unwrap();

        let f = &entries[0].form_fields[0];
        assert_eq!(f.kind, FormFieldKind::Text, "it becomes a plain Text field");
        assert_eq!(f.value, "data:x;base64,aGk=", "prefix + unwrapped base64");
        assert_eq!(f.base64_prefix, None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn expanding_a_base64_file_with_no_path_yields_just_the_prefix() {
        let mut entries = vec![entry_with_form(vec![FormField {
            key: "empty".to_string(),
            value: String::new(),
            kind: FormFieldKind::Base64File,
            content_type: None,
            base64_prefix: Some("pfx-".to_string()),
            enabled: true,
            desc: String::new(),
        }])];
        expand_base64_form_fields(&mut entries, None).unwrap();
        let f = &entries[0].form_fields[0];
        assert_eq!(f.kind, FormFieldKind::Text);
        assert_eq!(
            f.value, "pfx-",
            "no file to read, so only the prefix remains"
        );
    }

    #[test]
    fn expanding_a_base64_file_with_a_missing_path_is_an_error() {
        let mut entries = vec![entry_with_form(vec![FormField {
            key: "gone".to_string(),
            value: "/no/such/file/here.bin".to_string(),
            kind: FormFieldKind::Base64File,
            content_type: None,
            base64_prefix: None,
            enabled: true,
            desc: String::new(),
        }])];
        assert!(
            expand_base64_form_fields(&mut entries, None).is_err(),
            "an unreadable Base64File must surface as an error, not a silent send"
        );
    }

    /// Regression: the `_N` name invented for a clash must not be allowed to
    /// clash in turn. Two `report.csv`s produce `report_1.csv`, and a third
    /// field genuinely referencing a file called `report_1.csv` used to be
    /// copied over it — so that field silently sent the other file's bytes.
    #[test]
    fn a_generated_name_never_overwrites_a_file_that_is_really_called_that() {
        let dirs: Vec<std::path::PathBuf> = (0..3)
            .map(|i| {
                let d = std::env::temp_dir().join(format!(
                    "paperboy_stage_collide_{i}_{}",
                    uuid::Uuid::new_v4()
                ));
                std::fs::create_dir_all(&d).unwrap();
                d
            })
            .collect();
        std::fs::write(dirs[0].join("report.csv"), b"AAA-first").unwrap();
        std::fs::write(dirs[1].join("report.csv"), b"BBB-second").unwrap();
        std::fs::write(dirs[2].join("report_1.csv"), b"CCC-third").unwrap();

        let mut entries = vec![entry_with_form(vec![
            file_field("a", dirs[0].join("report.csv").to_str().unwrap()),
            file_field("b", dirs[1].join("report.csv").to_str().unwrap()),
            file_field("c", dirs[2].join("report_1.csv").to_str().unwrap()),
        ])];
        let staged_dir = stage_out_of_scope_form_files(&mut entries, None)
            .unwrap()
            .unwrap();

        let sent = |i: usize| {
            let name = &entries[0].form_fields[i].value;
            std::fs::read_to_string(staged_dir.join(name)).unwrap()
        };
        assert_eq!(sent(0), "AAA-first");
        assert_eq!(sent(1), "BBB-second", "each field sends its own file");
        assert_eq!(sent(2), "CCC-third");

        for d in &dirs {
            std::fs::remove_dir_all(d).ok();
        }
        std::fs::remove_dir_all(&staged_dir).ok();
    }
}
