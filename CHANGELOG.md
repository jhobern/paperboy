# Changelog

All notable changes to PaperBoy are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Releases before 0.1.2 predate this changelog and are not recorded here.


## [Unreleased]

### Added

- **PaperTrail reports — structured node editor.** Press **`n`** in a report to
  switch the flow between the source text and a new keyboard-driven *node*
  editor: the flow is shown as a navigable outline (a "Begin" root, one row per
  statement, `FOR …` loops with their nested body and an `END` row) that you
  build by inserting, removing and moving whole nodes instead of typing text.
  **`a`** (or Insert) opens an insert palette of node kinds; choosing `REQUEST`
  / `REPORT REQUEST` opens a request picker prepopulated from the bound
  collection's request titles, so names are never mistyped (rows are coloured
  green when the name resolves, amber when it doesn't). **`e`**/Enter edits the
  selected node (request nodes reopen the picker; other nodes open an
  "edit as line" prompt), **`f`** opens a **folder browser** to choose a
  `FOR … IN FILES/FOLDERS` loop's source directory (no path typing),
  **Del**/Backspace removes it, and **Shift+↑/↓** (or `K`/`J`) moves it among
  its siblings. Both editors are views over the same
  flow AST — every structural edit re-serializes back to the source text — so
  you can freely switch between them, and the logic is front-end agnostic for a
  future GUI. `?` Help and the Reports page document the node keys.
- **PaperTrail reports — new Reports view (work in progress).** A report is a
  new kind of tab, opened with **Shift+R**, that lives alongside the collection
  tabs but takes the whole body (no list / environment / response panels, so it
  fits small screens). Each report holds a PaperTrail flow (`.report` source)
  that will drive a bound collection against ranges of files/environments to
  produce a tabular report. This first slice adds the tab itself: a view of the
  flow source with its live validation (bound-collection status, parse errors,
  and per-statement diagnostics). The source is edited **inline** — press
  **`e`**/Enter to give the source panel edit focus and type directly into it
  (edits apply live; Esc returns to navigation mode where single letters are
  shortcuts again), mirroring the request wizard's text cells. Report tabs are
  persisted (source text is snapshotted so an unsaved scratch report survives a
  restart), and the Help (**`?`**) overlay gains a **Reports** tab explaining
  what a report is, the report shortcuts, and the flow language. Running a
  report, CSV export, and binding a collection follow in later updates.
- **Syntax highlighting in the report source.** The PaperTrail source now
  highlights as you read and edit it: keywords (REQUEST, REPORT, FOR, END, …)
  are drawn in the theme accent, `{{var}}` substitutions reuse the app's
  substitution colour, `#` comment lines are dimmed, and the exact line the
  parser rejects is recoloured and underlined so a malformed flow is obvious at
  a glance. The read-only source and validation panels are now scrollable
  `MultiSelectPanel`s (with a scrollbar) for a consistent feel with the rest of
  the app — scroll the source with the arrow keys (Home/End jump to the
  top/bottom) when it isn't in edit focus.
- **Report source editor: word-wise cursor movement and name completion.**
  While editing a flow, **Ctrl+←/→** now moves the cursor one word at a time
  (instead of jumping to the line ends), and typing a `REQUEST` (or `REPORT
  REQUEST`) name — or an environment name on a `FOR … IN ENVS` clause — shows a
  dim inline suggestion of a matching name that **→** or **Tab** fills in, so
  names stay correct and discoverable even though the report view can't show the
  collection or environment lists. Completion is quote-aware: a matching name
  that contains spaces is auto-quoted on accept (typing `Up` completes to
  `"Upload document"`), completion keeps matching even after you type one of the
  name's spaces, and completing inside an already-opened `"` fills the rest of
  the name and appends the closing quote — so an accepted completion always
  parses.
- **Run a report and export its results to CSV.** A bound report can now be
  **run** (**`r`**/F5): PaperBoy drives the flow against its bound collection and
  shows the produced rows in a results **grid** (columns follow the flow's
  `columns:` directive, else the reported fields in first-seen order). **Tab**
  (or **`v`**) flips between the flow source and the grid, and **`x`** exports the
  last run to a CSV file — chosen through the regular **file picker** (browse to
  a folder and confirm the name) rather than being dropped into the app's
  working directory — as RFC 4180, so multi-line response bodies are preserved.
  A report that isn't ready to run — unbound, unparseable, or with validation
  errors — says why in the status bar instead of running. The run happens on a
  **background thread** so the UI stays responsive; a `⏳ Running…` indicator
  shows in the binding panel and pressing **`r`** again **cancels** the in-flight
  run (an already-issued request finishes, but no further ones start).
- **Dry-run preview (`d`).** Before firing any requests, press **`d`** in the
  Reports view to expand the flow with a no-op runner and preview the result: the
  projected **row count**, a sample of the first iterations' resolved variable
  bindings (e.g. `FILE=…, PREFIX=…`), and any producer/request-resolution
  problems (empty globs, `ZIP` length mismatches, unresolved request names). This
  catches Cartesian-product blow-ups and mis-wired loops without sending a single
  HTTP request. The overlay scrolls with the arrow keys and closes with Esc.
- **`# environment:` report header.** A report can now name a single, already
  loaded environment to run against — `# environment: staging` — used as the
  run's base variable layer for a plain, no-comparison run. It makes a report
  self-contained and reproducible (the named env is used regardless of which
  environment is active or pinned in the app); when omitted, the run falls back
  to the app's active plus the bound collection's pinned environment as before.
  Naming an environment that isn't loaded is a validation error (mirroring
  `ENVS`), and the report's binding panel shows the chosen environment. Multi
  environment comparison still uses a `FOR … IN ENVS` loop.
- **Environment comparison — the `Result` column.** A report that loops over
  environments with roles — `FOR TARGET IN ENVS BASELINE("prod"),
  COMPARISON("staging")` — now collapses each document's baseline and candidate
  runs into a single output row (the row key excludes the environment axis, so
  the two align) and adds a reserved **`Result`** column describing the diff.
  The candidate's values are shown; `Result` reads `OK` when every reported
  field matches the baseline, or a `field: baseline→candidate` summary of each
  field that changed (falling back to the whole `Response` for a request that
  declares no `[Reports]`/`WITH` fields). Multiple comparisons are grouped per
  document, and an unmatched row still appears (`no baseline` / `no candidate`).
  `Result` is shown by default and can be renamed/reordered like any column via
  `# columns:` (e.g. `# columns: FILE as Name, Result, proc.status as Status`).
- **Compare against a saved run — `.baseline` snapshots (`# baseline:`).** The
  `Result` column now has a second source: instead of comparing two environments
  in one run, a report can compare *this* run against a **saved snapshot of an
  earlier accepted run** — the "did this release change anything?" workflow.
  After a run, press **Shift+B** in the results view to save the run as a
  `.baseline` JSON file (via the same folder picker as CSV export, seeded with
  `<report>.baseline`). Add a **`# baseline: <path>`** header directive (the path
  resolves like producer paths — relative to `# root:`/the report's folder) and
  every subsequent run diffs its reported fields against the snapshot to fill the
  same `Result` column (`OK`, a `field: was→now` summary, `no baseline`, or `no
  candidate`), reusing the environment-comparison engine so the two read
  identically. A live `ENVS BASELINE/COMPARISON` clause takes precedence; the
  directive is flagged as ignored when both are present, and a missing/invalid
  snapshot is a non-fatal run error (rows are still produced).
- **Choose which response fields a request contributes — `SHOW(...)`.** A
  `REPORT REQUEST` can now be followed by `SHOW(field, field, …)` to emit only
  the listed fields (in that order) instead of every intrinsic and `[Reports]`
  field. This is the lever for keeping a heavy body — a base64 image, say — out
  of the report: `REPORT REQUEST process AS proc SHOW(status, score)` drops the
  whole-body `proc.Response` column entirely while keeping the small extracted
  fields. Listing a field the request can't produce is a validation warning.
- **Column picker overlay (`c`).** In the Reports view, `c` opens an interactive
  checklist of every column the last run produced (plus the flow's loop/assign
  variables). Space toggles a column in or out, Shift+↑/↓ reorders, and Enter
  writes the selection back to the flow's `# columns:` directive — so a
  non-programmer can shape the output without editing the directive by hand.
  (Run the report once first, so its available columns are known.)
- **Reports view editing and results refinements.** A batch of usability
  improvements to the Reports view: the results grid, source and validation
  panels all support **mouse selection, copy** (drag to select, **`y`** copies
  the selection or the whole panel) and show the **line-wrap** indicator, like
  the response view. Syntax highlighting now also colours the `# collection:`
  and `# environment:` header references and `FOR … IN ENVS` names by whether
  they currently resolve (loaded/found vs missing). In the source editor,
  pressing **Enter** keeps the current line's indentation — and adds one level
  after a `FOR` — while typing `END` snaps the line back to its matching `FOR`'s
  indent, so nested blocks stay aligned without manual spacing. The binding
  panel now names the **base directory** that relative `FILES`/`FOLDERS` paths
  resolve against (the report's own folder once saved, else the working
  directory, flagged as a fallback). Plain **←/→** arrows on the tab bar now move
  across report tabs too (previously only Ctrl+←/→ and `[`/`]` did).
- **Request names are highlighted in report scripts.** The name on a `REQUEST`
  (or `REPORT REQUEST`) line now lights up green when it resolves to a request
  in the bound collection and amber when it doesn't — mirroring how `# collection:`,
  `# environment:` and `ENVS` names are coloured — so a mistyped or unbound
  request name is obvious at a glance. Both bare (`REQUEST Oauth`) and quoted
  (`REPORT REQUEST "Upload document"`) names are coloured, and keyword-looking
  words inside a quoted name (e.g. a name containing `for`) are still left alone.
- **Undo and word-delete in every text editor (Ctrl+Z / Ctrl+Backspace).** All
  of PaperBoy's text fields and multi-line editors — the request wizard cells,
  the report source editor, git/save prompts, and so on — gain **Ctrl+Z** to
  undo (and **Ctrl+Shift+Z** to redo) and **Ctrl+Backspace** to delete the
  previous word. A run of typing collapses into a single undo step, and
  Ctrl+Backspace removes a whole `"…"` quoted token in one go, so a quoted
  request name deletes as a unit. (Implemented in the shared `tui-line-editor`
  crate, so it applies everywhere consistently.)
- **Report view: Tab cycles focus across the editor, results and tab bar.**
  In the report view, **Tab** now rotates focus **editor → results grid → tab
  list → editor** (Shift+Tab reverses it; the results stop is skipped until the
  report has been run), so the tab bar is reachable from the keyboard without
  leaving the report. The focused area is highlighted (and the unfocused body
  dimmed). The plain **`v`** key still simply flips between the source and the
  results grid.
- **Load and save reports through the File menu, and re-point them with `b`.**
  A report is now a first-class file: **File ▸ Load ▸ Report** opens a `.report`
  flow into a new tab, and **File ▸ Save ▸ Report** writes the active report back
  to its file (**Save As** opens a folder chooser seeded with `<name>.report`).
  The Reports view also gains a **`b`** (bind) action that lists the open
  collections and re-points the report's `# collection:` header at the chosen
  one — preferring a path relative to the report file (so a report and its
  collection committed together stay linked), then an absolute path, then the
  collection's name for an unsaved scratch collection. Saving to a git remote
  follows in a later update.
- **Author a request's `[Reports]` fields in the request editor.** The New/Edit
  Request wizard gains a **Reports** section (a tab beside Asserts and Captures,
  reachable with **Alt+8**, PageDown, or Tab). Each row is a `name: <hurl query>`
  pair — authored exactly like a Capture — that names a value to pull from the
  response into a generated report (e.g. `status: jsonpath "$.status"`). The
  section round-trips through collection save/load (stored as a spec-safe
  `# [Reports]` comment block), and a request with no report fields contributes
  its whole response to a report instead.

### Fixed

- **Ctrl+Backspace (word-delete) now works on terminals without the
  keyboard-enhancement protocol.** Such terminals report Ctrl+Backspace as a
  bare **Ctrl+H**, which previously did nothing (or inserted a stray character)
  in the report editor and other text fields; it is now accepted as an alias for
  Ctrl+Backspace, so word-delete works everywhere regardless of terminal
  support.


## [0.1.6] - 2026-07-18

### Added

- **Status-code assertions appear in the `[Asserts]` list.** A bare `HTTP 200`
  status line is now shown as a synthesized `status == 200` row at the top of a
  request's `[Asserts]` section (in both the Request preview and the Response
  view) and is counted in the pass/fail badge, so an implicit status check is
  no longer invisible.
- **"Run All" streams results as they finish.** By default Run All (Alt+F5) now
  runs each request on its own and stamps each pass/fail marker in the Requests
  list the instant that request completes — matching the CLI's default — with a
  status-bar note that automatic cookies aren't carried between requests in this
  mode. A new **Run All in batch mode** preference (Settings → Preferences)
  switches back to running the whole collection in one execution, which chains
  Hurl's cookie jar and `[Captures]` across every request.
- **The Settings and Preferences menus have shortcut keys.** Each row now shows
  a `(letter)` mnemonic — like the File menu — that activates it directly.
- **The tab bar scrolls when it overflows.** When more collection/workspace
  tabs are open than fit across the top, the bar now scrolls to keep the active
  tab in view and shows `‹`/`›` markers so you can tell there are more tabs off
  each edge.
- **Request names are shown in the Requests list.** A request with a name set
  (in the request editor) now displays that name in the list instead of its
  URL; unnamed requests still show the URL.
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
- **Soft-wrapped lines are marked in the Request and Response panels.** When a
  long line in the request preview or response body wraps onto further rows, a
  dim `↵` now appears in the panel's rightmost column on each continued row, so
  a wrapped line reads unambiguously as one line rather than several separate
  ones. The marker sits in a reserved column and never hides any content.
- **"Save to folder" dialogs have an inline file-name editor.** The folder
  browser for "Save Collection As…" and "Save Workspace…" now shows a file-name
  field at the bottom: press `Tab` to focus it, edit the name, and press `Enter`
  to save into the current folder (a missing `.hurl` extension is added
  automatically). This replaces the previous two-step "pick folder, then answer
  a separate name prompt" flow.
- **The status/error line can be copied with `^y`.** Messages in the top bar —
  including long Hurl parse errors — can't be mouse-selected, so `Ctrl+Y` now
  copies the current status line to the clipboard (the message stays on screen).

### Changed

- **A failed status assertion now explains itself.** Instead of the terse
  `Request error: Assert status code: HTTP 200`, the Response pane now reads
  e.g. `Expected status 200 but got 404 Not Found (GET https://example/x)`,
  naming the expected and actual status and the request that failed.
- **"Save Collection As…" opens a folder chooser.** Saving a collection (or the
  Scratch Space) to a new location now lets you browse to and pick the
  destination folder before naming the file, matching "Save Workspace…".
- **The Raw Hurl editor explains why text won't save.** When saving from Raw
  Mode fails, the status line now gives the specific reason and line number
  (e.g. `[Captures] is a response section — add an 'HTTP' status line above it
  (use 'HTTP *' to accept any status)`) instead of the generic "expected exactly
  one request".
- **The default-new-request URL no longer occupies the top bar.** The persistent
  "Default New Request URL" readout has been removed from the header; the `b`
  shortcut still opens the editor for it (and it remains documented in Help).

### Fixed

- **Truncation ellipsis is placed correctly for multi-byte text.** The dim `…`
  shown at the end of a clipped, unfocused wizard cell (Header/Cookie/Query/Form)
  is now positioned by character width rather than byte length, so cells
  containing non-ASCII text no longer mark themselves as truncated too early.
- **The "Target collection" selector in the New Request wizard cycles again.**
  `←`/`→` (or `h`/`l`) once more move the new request between collections
  instead of doing nothing.
- **Form-row arrow keys reach the enabled checkbox and skip inert cells.**
  `←`/`→` now step onto a Form row's leading enabled checkbox, and skip the
  Content-Type cell on a Base64 File row (where it doesn't apply) rather than
  stopping on it.
- **Closing the request editor returns focus to the Requests list.** Cancelling
  or submitting the editor opened from the list no longer jumps focus to the raw
  request view; it stays on the collection's Requests list where it was opened.

### Internal

- **The Request and Response body panels now use `tui-panel-select`'s
  `MultiSelectPanel`** instead of PaperBoy's own re-implemented
  selection/scroll/wrap plumbing. The crate type owns multi-region selection,
  keyboard extension, drag-autoscroll, scroll clamping, styled/plain content,
  and the new end-of-row wrap marker; PaperBoy keeps only the app-specific
  cross-panel orchestration (copy ordering, syntax-highlighted content,
  scrollbar drag). No user-facing behaviour change beyond the wrap marker above.
- **The clipped-cell truncation ellipsis moved into `tui-line-editor`** (as a
  reusable `TruncationMarker` / `render_clipped_line`), so PaperBoy's wizard
  cells render it through the shared crate rather than a local copy.
- **The vertical scrollbar's row↔scroll mapping and thumb rendering moved into
  `tui-panel-select` 0.1.4** (behind its default-on `scrollbar` feature). The
  wizard tables and the Request/Response body panels now share the crate's
  `render_scrollbar`, and mouse clicks on a body-panel scrollbar map to a scroll
  position through `MultiSelectPanel::scroll_to_track_row`, replacing PaperBoy's
  local scrollbar math. No user-facing behaviour change.


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
