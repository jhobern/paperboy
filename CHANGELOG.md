# Changelog

All notable changes to PaperBoy are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Releases before 0.1.2 predate this changelog and are not recorded here.


## [0.1.6] - 2026-07-18

### Added

- **Query parameters section in the request editor** — the New Request wizard
  gains a `[Query]` section alongside Headers and Cookies, with the same
  enabled checkbox / key / value / description columns and the same navigation
  shortcuts.
- **Disabled request rows now survive a save to disk.** A disabled Header,
  Cookie, Query or Form row is written to the `.hurl` file as a commented
  `# key: value` line instead of being dropped, so its enabled/disabled state
  is no longer lost when a collection is saved and reloaded. On load, a
  commented line that still looks like a real request row is read back as a
  disabled entry (ordinary prose comments are left untouched), and the Raw Hurl
  view shows disabled rows as those comments so you can see exactly what will be
  saved and sent.

### Fixed

- **The "Target collection" selector in the New Request wizard cycles again.**
  `←`/`→` (or `h`/`l`) once more move the new request between collections
  instead of doing nothing.
- **Form-row arrow keys reach the enabled checkbox and skip inert cells.**
  `←`/`→` now step onto a Form row's leading enabled checkbox, and skip the
  Content-Type cell on a Base64 File row (where it doesn't apply) rather than
  stopping on it.


## [0.1.5] - 2026-07-16

### Fixed

- **Bodyless `POST`/`PUT`/`PATCH`/`DELETE` requests now only switch to having
  `Content-Length: 0`** if there are no Forms and no Body.
- Requests with a Form field with a `Type` of `Base64 File` will now correctly
  send as `[Multipart]` requests. 

## [0.1.4] - 2026-07-15

### Added

- **"Base64 File" form field type** — the Form section's `Type` dropdown gains
  a "Base64 File" option alongside Text and File. Like a File field its `Value`
  cell opens a file picker (`Enter`/`Ctrl+F`), but at send time the field is
  transmitted as plain **Text** whose value is the file's base64 encoding
  (unwrapped, single line). A new "Base64 Prefix" column lets you prepend a
  string to that encoding — e.g. a `data:image/png;base64,` prefix — so the
  request value becomes `<prefix><base64>`. Saved collections round-trip the
  file reference and prefix so the field reloads as a Base64 File.
- **Custom themes** — Settings → Theme opens a theme editor. The three
  built-in per-language looks are now named, non-deletable presets (Britannia,
  Parisian Purple, Dannebrog) you can pick from a list. `Ctrl+N` opens a popup
  to create your own: name it (with a blinking cursor ready for typing) and
  choose an existing theme to copy its colours from, and it's added to the list,
  activated, and opened for editing. Select a custom theme and step into the
  colour rows (`→`/`Tab`) to change any of its eleven colours; press `Enter` on
  a colour to open a picker where you dial each R/G/B channel with the arrow
  keys (`←`/`→` ±1, `Ctrl`+`←`/`→` or `PageUp`/`PageDown` ±16) or type a
  `0`–`255` value — the whole UI previews live as you go, `Enter` applies (and
  auto-saves), `Esc` cancels. Rename a custom theme from the editable name row
  above the colours (`Enter` submits the new name). `Ctrl+D` deletes a custom
  theme, moving focus to the theme just above it. Built-in presets are
  read-only. Changing language still switches to that language's preset unless
  you've manually chosen a theme.
- **Reopen a deleted Global Environment** — deleting an environment (`x` in the
  Global Environments panel) is now undoable: press `u` to reopen the most
  recently deleted one, restored to where it was. Both the deletion and the
  reopen are reported in the status bar.
- **"Confirm before deleting an environment" preference** — Settings →
  Preferences gains a toggle (on by default) to skip the delete-environment
  confirmation popup; with it off, `x` deletes straight away (still undoable
  with `u`).

### Changed

- **`[` / `]` switch section tabs in the New Request wizard** — an
  easier-to-reach alias for `PageUp`/`PageDown` (which still work), matching
  the main view's tab keys. They only cycle tabs when focus is on a non-text
  field (Method, Target, or a "+ Add …" row), so the brackets can still be
  typed into URLs, JSON bodies, and header/cookie/form values.

### Internal

- **Reusable TUI components split out into standalone crates, published to
  crates.io, and consumed as dependencies** (repository:
  [`paperboy-tui`](https://github.com/jhobern/paperboy-tui)). PaperBoy no longer
  vendors these in-tree — they're ordinary dependencies now. No user-facing
  behaviour change.
  - **`tui-panel-select`** — panel-scoped mouse selection, resize-stable wrap
    cache, and cross-platform clipboard copy, behind a simple `SelectablePanel`
    API. Also provides an opt-in batteries-included
    `SelectablePanel::handle_mouse` (configured via `MouseConfig`, e.g.
    copy-on-release) that wires up drag-to-select-to-copy in one call while the
    low-level `begin`/`extend`/`copy_selection` methods stay available, and a
    default-on `terminal-guard` feature whose `TerminalGuard` RAII helper
    enables mouse capture (and optional keyboard enhancement) and restores the
    terminal on drop *and* on any panic — PaperBoy's TUI setup uses it.
  - **`tui-rgb-picker`** — the R/G/B channel-slider colour picker (state, input,
    and a styleable/localizable ratatui widget); the theme editor consumes it,
    supplying its own colours, labels and hint.
  - **`tui-line-editor`** — the single- and multi-line text editor primitive
    (cursor, selection, masking, and the scrolling/field renderers); PaperBoy's
    `editor` module is a thin theming shim over it. (`tui-textarea` was
    evaluated but only supports ratatui 0.29, incompatible with PaperBoy's
    ratatui 0.30.)

### Fixed

- **Bodyless `POST`/`PUT`/`PATCH`/`DELETE` requests now send `Content-Length:
  0`** — matching what Postman and browsers send. libcurl (which the runner
  uses) omits the header for a bodyless request over HTTP/2, and some servers
  reject such a request with `400 Bad Request`; the header is now added
  automatically at run time (unless the request has a body/form fields or you
  set `Content-Length` yourself). Saved `.hurl` files are unaffected.
- **Postman import no longer fails on `null` string fields** — collections
  exported from Postman routinely carry an explicit `"value": null` (or null
  `src`) on blank `file` form-data entries. A single such `null` previously
  aborted the whole import and the file couldn't be opened as a collection;
  these are now treated as empty strings.

## [0.1.3] - 2026-07-15

### Added

- **Move / copy requests between workspace collections** — `m` moves and `c`
  copies the selected request into another collection file in the workspace
  (chosen through a picker); the change is written straight to disk.
- **Undo hints in the status bar** — closing a tab or deleting a request now
  shows a message naming the `u` key to reverse it.

### Fixed

- Environment files whose name carries an extra suffix (e.g.
  `environment.env.dev-au`) now show their full name in the Environments panel
  instead of being truncated to `environment.env`. Only the known environment
  extensions (`.env` / `.vars`) are hidden; any other suffix is kept verbatim.

## [0.1.2] - 2026-07-15

### Added

- **Save Workspace to Git** — push an entire workspace tree back to a remote
  branch or tag, with no local clone (only the files being written are fetched
  or touched).
- **Workspace destination picker for new requests** — when a new request
  targets a workspace, choose which collection it joins, or create a brand-new
  collection in the workspace by entering a relative path (subfolders and a
  default `.hurl` extension are handled for you).
- **"Always save when prompted" preference** (Settings → Preferences, off by
  default) — automatically pick *Save* whenever an action would otherwise pop
  up a Save / Discard / Cancel choice.
- **Unsaved-changes warning** when switching away from a workspace collection
  that has edits, so in-memory changes are no longer lost silently.
- **File browser reset shortcut** (`Ctrl+R`) — jump straight back to the folder
  the picker originally opened in after navigating away.
- **Move / copy requests between workspace collections** — `m` moves and `c`
  copies the selected request into another collection file in the workspace
  (chosen through a picker); the change is written straight to disk.
- **Undo hints in the status bar** — closing a tab or deleting a request now
  shows a message naming the `u` key to reverse it.

### Changed

- **Redesigned the File → Load / Save menus** into a two-step flow: first pick
  *what* (Request / Collection / Environment / Workspace / Response), then pick
  the source (Local / From Git) or destination (Save / Save As / To Git). Git
  and local options are no longer duplicated across one long list. `←` / `→`
  (and `Esc` / `Enter`) step out of and into submenus.
- **Reworked the workspace request list** into a filesystem tree + accordion:
  browse folders and collections inline with breadcrumb navigation, open the
  highlighted folder or collection with `→` (or `Enter`), and run just the
  current folder's requests with `Alt+F5`. Press `w` to pop up the full
  workspace tree at any time.
- **Improved file-browser Left/Right navigation** — it is now directional and
  retraces multiple levels (Left ×N then Right ×N returns to the start), and
  `→` no longer climbs back up through the `../` row.
- **Simplified the git collection loader** — it no longer prompts "also load an
  environment" as a separate step.
- **Filtered the git file picker** to the relevant file types instead of listing
  every file in the repository.

### Fixed

- Variables no longer show as *shadowed* when the linked environment is also the
  active (global) environment.
- Dotted environment filenames (e.g. `.env.dev-au`) keep their full name, while
  collection tab titles hide only real `.hurl` / `.json` extensions.
- The environment file picker reopens in the last environment folder instead of
  the last folder used by any picker.
- `F2` renames the selected environment when the Environments panel is focused
  (previously it was shadowed by the tab-rename binding).
- `[Form]` file paths containing spaces are now handled correctly when staging,
  checking existence, and emitting Hurl.
- A new request saved with an empty name no longer appears nested under a folder
  derived from its URL.
- A request with an empty URL can now be saved (its URL is validated at run
  time) instead of being silently discarded.

### Internal

- Refactored a range of verbose, "reinvented" code from earlier development into
  standard-library and crate-backed equivalents — including moving the Postman
  importer onto typed serde DTOs — with behavior preserved and the full test
  suite green.
