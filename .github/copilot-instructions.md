# PaperBoy — Copilot instructions

PaperBoy is a Rust-native API client (a Postman alternative): a Hurl-based
request runner exposed through two front-ends over one shared core — a
terminal UI (default) and a headless CLI runner.

## Build, test, lint

- `cargo run` — launch the terminal UI (the default mode).
- `cargo run -- -c collection.hurl [-e environment.vars] [--batch]` — headless
  CLI runner (runs a collection and exits).
- `cargo build` / `cargo build --release`.
- `cargo test` — run the whole suite. This is a **binary crate with no lib
  target**, so `cargo test --lib` fails; always use plain `cargo test` (unit
  tests are compiled into `src/main.rs`).
- `cargo test <name>` — run a single test (or a substring filter), e.g.
  `cargo test brackets_cycle_wizard_section_tabs_on_non_text_fields`.
- `cargo fmt` — formatting is expected before committing (there are explicit
  "Ran cargo fmt" commits).

### libcurl / build environment

libcurl reaches the tree via `hurl` → `curl` → `curl-sys`. The direct `curl`
dependency in `Cargo.toml` enables `static-curl`/`static-ssl`, so curl-sys
**compiles and statically links vendored libcurl + OpenSSL from source** — no
system libcurl `-dev` package or pkg-config `.pc` is needed (this environment
ships neither). The build is therefore self-contained; it needs a C compiler,
`perl` and `make` at build time (all present). Don't remove the `static-*`
features expecting to fall back to a system libcurl — this environment can't
link one.

Rust edition is **2024**.

## Architecture — one core, two front-ends

`main.rs` parses args (clap) and dispatches: `-c/--collection` → `cli::run`
(headless); otherwise → `tui::run` (terminal UI).

**Core domain (front-end agnostic):**

- `hurl/` — the Hurl format is the internal request model. `entry.rs`
  (`HurlEntry` + text serializer), `parser.rs` (Hurl text → `HurlEntry`),
  `run.rs` (execute/evaluate `[Captures]`/`[Asserts]` via the `hurl` crate),
  `stage.rs` (copy out-of-scope `[Form]`/`[Multipart]` files next to the run's
  `file_root` so Hurl's sandbox accepts them).
- `collection.rs` — a collection is an ordered list of requests (a `.hurl`
  file). `postman.rs` imports Postman `.json` exports and converts them to Hurl.
- `environment.rs` — `.vars` files (`KEY=value`). Values can be literals or
  provider references resolved **at load time by shelling out to a CLI**:
  `{{ env:NAME }}`, `{{ ssm:/path }}` (AWS SSM), `{{ op://… }}` (1Password).
  Resolved secret values are never persisted.
- `request.rs`, `http.rs` — request building and response types.
- `git_remote.rs` + `tui/remote.rs` — load/save collections, environments and
  whole workspaces to/from git remotes with **no local clone** (only the files
  being read/written are fetched).
- `workspace.rs` — filesystem-tree view over a folder of collections.
- `persistence.rs` — session state (tabs/collections/environments, secrets as
  source references only) to `~/.config/paperboy/state.json`. Honours
  `PAPERBOY_STATE_DIR` (used by tests) and `XDG_CONFIG_HOME`.

**Terminal UI (`tui/`):** event loop in `mod.rs`; `app.rs` holds `TuiApp`
(all UI state); `input.rs` is the keyboard dispatch (large `match` per mode);
`draw.rs` renders; `new_request.rs` is the request wizard (`NewReq` +
section-view tabs); `editor.rs` is the multiline text editor; plus
`selection.rs`, `wrapcache.rs`, `clipboard.rs`, `theme.rs`, `theme_editor.rs`.

## Conventions

- **All user-facing strings live in `i18n.rs`** in the `strings!` macro table,
  one row per string with English / French / Danish columns side by side.
  Adding a string is one row (which keeps the three languages from drifting).
  Never hardcode display strings in the UI — add a `Strings` field and read it.
- **Comment the "why".** This codebase documents rationale, edge cases, and
  terminal/dependency quirks in prose comments (e.g. the panic-hook /
  mouse-capture handling in `tui/mod.rs`). Match that when touching non-obvious
  code; don't strip existing explanatory comments.
- **Tests are inline** `#[cfg(test)] mod tests` — the large one is
  `src/tui/tests.rs`. Drive the UI through `TuiApp` using helpers like `press`,
  `form_ref`, `new_focus`, and `open_form_on_*`; add new tests alongside them.
- **User-facing changes get a CHANGELOG entry.** `CHANGELOG.md` follows Keep a
  Changelog + SemVer; bump `version` in `Cargo.toml` and add a dated section.
- **Terminal state must be restored on exit and on panic.** Mouse capture and
  the keyboard-enhancement protocol are toggled in `tui/mod.rs`; the panic hook
  there also undoes them — preserve that when changing startup/teardown.
- **Colours go through the theme, never hardcoded.** Draw code reads
  `app.theme()` (a runtime `Theme` of `ratatui` colours) rather than fixed
  colours. Themes are `ThemeSpec`s (RGB triples, serialisable to `state.json`)
  built from language presets in `theme.rs` or user-defined custom themes;
  `active_theme: None` means "follow the language". Add a new theme colour by
  extending `ThemeSpec`/`Theme`, `THEME_COLOR_COUNT`, and its `i18n.rs` label.
- **Keybinding placement:** main-view keys are handled in `input.rs::on_key`;
  wizard keys in the `new_request` key handler. Tabs cycle with `[`/`]` and
  `PageUp`/`PageDown`; in the wizard `[`/`]` only cycle when focus isn't on a
  text-entry cell (so brackets can still be typed into URLs/bodies/values).
