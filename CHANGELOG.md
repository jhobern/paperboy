# Changelog

All notable changes to PaperBoy are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Releases before 0.1.2 predate this changelog and are not recorded here.


## [0.2.0] - 2026-08-04

### Added

- **A native graphical front-end (`-g` / `--gui`).** PaperBoy now ships a
  desktop GUI (built on eframe/egui) alongside the terminal UI and the headless
  CLI runner — all three drive the *same* shared core (collections,
  environments, the Hurl request model, request running, git-remote load/save,
  themes and translations), so anything you build in one front-end round-trips
  through the others. Launch it with `paperboy -g`. The terminal UI remains the
  default when no flag is given.

  The GUI mirrors the TUI feature-for-feature: a Postman-style layout with a
  collapsible collection/folder tree, the request editor (Params, Headers,
  Body, Auth, Cookies, Options, Asserts, Captures and a resolved-request Code
  view), the response viewer (Body/Headers/Asserts), the Global Environments
  panel (activate, link, edit variables, resolve `env:` / `ssm:` / `op://`
  secrets), report tabs, the theme editor, and loading/saving collections and
  environments to a git remote with no local clone. **Tab** cycles the panels
  in the same order as the terminal UI (Tabs → List → Main → GlobalEnv →
  Response), and **Shift+Tab** cycles backwards.

  Interactions that needed keyboard shortcuts in the terminal become native GUI
  gestures: panels are resized by dragging their splitters (rather than with
  keys), selection, scrolling and clipboard use the platform's own handling, and
  collection tabs switch by clicking. Every built-in and custom **theme** applies
  to the GUI unchanged (the shared RGB `ThemeSpec` is mapped onto egui's visual
  style), and all GUI text flows through the existing `i18n` table, so English,
  French and Danish are all covered. GUI icons (the tree's folder/file/report/
  environment glyphs, the run/close/add controls and the pass/fail markers) are
  drawn from a bundled **Phosphor** icon font, so they render consistently on
  every machine rather than depending on which emoji fonts the host has
  installed.

- **A Scratch-style PaperTrail report editor in the GUI.** Report tabs (and
  `.trail` files opened from a workspace) now open in an interactive editor that
  mirrors the terminal UI's node editor. A **Blocks** view shows the report flow
  as stacked, nested, colour-coded blocks — assignments, list declarations,
  request and report steps, and `for` loops — that you add from a palette,
  reorder (move up/down), delete, and edit inline (either as a single grammar
  line or, for request steps, by picking a request from the bound collection).
  A **Source** view exposes the raw `.trail` text; the two round-trip through
  the same parsed flow, so an edit in one is reflected in the other. A live
  validation panel flags parse errors and diagnostics against the bound
  collection and environments, and **Ctrl+Z** undoes structural edits. The pure
  flow-editing and validation logic is shared with the terminal UI, so both
  front-ends stay in lock-step.

- **Running PaperTrail reports from the GUI.** The report editor now has a
  **Run** button that executes the report against its bound collection on a
  background thread (so the window stays responsive), streaming results into a
  live **Results** grid: the projected rows appear immediately (greyed), each
  row un-greys and fills as its requests complete — several at once under a
  `PARALLEL` loop — and the finalized table (including any `ENVS`/baseline
  comparison collapse and `STATISTICS(…)` summary rows) replaces it at the end.
  **Stop** cancels a run in flight while keeping the rows completed so far, and
  **Export…** writes the results to CSV, JSON, HTML or XLSX (chosen by the file
  extension). Running reuses the same front-end-agnostic executor and run-input
  assembly as the terminal UI, so a report produces identical results in either
  front-end.

- **Workspaces in the GUI.** *File → Open workspace…* points the GUI at a
  folder of collections and shows its filesystem tree in the left panel, exactly
  like the terminal UI: folders expand and collapse, collection files list their
  requests inline, and selecting a request opens it in the editor on the right
  (double-click runs it). Selecting a `.vars` file loads it as a global
  environment, and selecting a `.trail` report opens it in the PaperTrail block
  editor in the centre pane. The expand/collapse state is the same set the
  terminal UI persists, so a workspace opened in one front-end round-trips
  through the other.

- **A drag-and-drop block palette for the GUI report editor.** The Blocks view
  now has an always-visible palette split into **Blocks** (base statements —
  `REQUEST`, `REPORT` variable, an assignment, `LIST`, and the three `FOR`
  loops) and **Modifiers** (`REPORT`, `PARALLEL`, `WITH`, `AS`). Drag a base
  block into the report and the existing blocks slide apart to open an animated
  gap with a placeholder showing exactly where it will land. A report line is
  now *composed* from several chips rather than one opaque row: dropping the
  `REPORT` modifier onto a `REQUEST` turns it into a reported request
  (`[REPORT] [REQUEST …]`), `PARALLEL` onto a `FOR` runs it concurrently, `WITH`
  adds an ad-hoc field to a report request, and `AS` names a report column — and
  each attached modifier shows as its own chip with a `×` to detach it. A
  modifier only highlights and accepts a drop where it is valid for the block it
  is over. Picking a request from the name picker now renames in place, keeping
  a report request's `AS` / `WITH` / `RESPONSE` modifiers intact. The
  compositional edit operations live in the shared flow-editing core, so they
  are available to both front-ends.

- **Legible report chips, drag-to-reorder, a trash bin, and node wizards for
  the GUI report editor.** A report line's clauses each render as their own
  chip so a long line stays readable: a reported request now shows separate
  `RESPONSE(…)`, `SHOW(…)` and `HIDE(…)` chips (each with a `×` to detach it),
  and a `FOR … IN ENVS` role loop shows its `BASELINE(…)` and `COMPARISON(…)`
  as distinct chips. Blocks and chips already in the report are now themselves
  drag sources: drag a whole row to **reorder** it (the surrounding blocks slide
  apart to show where it will land, just like dropping from the palette), or
  drag a row or a modifier chip onto the new **trash bin** at the foot of the
  palette to delete the block or detach the modifier. Double-clicking a block
  (or a request/`ENVS`/files node) opens a **configuration wizard** — a modal
  form ported from the terminal UI's node editors — so you tick fields, pick
  requests and environments and set `RESPONSE`/`SHOW`/alias options through
  checkboxes, combo boxes and radio buttons instead of hand-editing the grammar
  line. The wizard, detach and move operations all run through the shared
  flow-editing core, so the terminal UI and GUI stay in lock-step.

- **Everything-is-a-drop-target, wizard-on-drop and inline pickers for the GUI
  report editor.** Placing blocks is now far more forgiving and guided. A
  modifier chip (`WITH`, `AS`, `REPORT`, …) can be dropped **anywhere along its
  line** — the whole row to the right of the base block accepts it — and a block
  dropped in the **empty space beneath the last line** simply becomes the last
  line, so you no longer have to hit a thin strip. Dropping a new block (or
  attaching a modifier) now **opens its configuration wizard straight away**, so
  you fill it in there rather than in a separate step. The palette's two
  overlapping `REPORT` entries are combined: a plain **VARIABLE** block sets
  `VAR = VALUE`, and the single `REPORT` modifier now drops onto either a
  request *or* a variable (adding a `REPORT (VAR)` line after the assignment).
  Every node kind now has a wizard — assignments, lists and `FOLDERS` loops get
  purpose-built forms, and anything else falls back to a raw-line editor — so no
  block is ever left uneditable, and the old always-on raw text box under a
  selected line is gone. Finally, enumerable fields are now **inline dropdowns
  right on the chip**: a request chip's name and a `BASELINE`/`COMPARISON`
  environment are picked from a combo box (of the bound collection's requests /
  the loaded environments) without opening the wizard at all. These edits use
  the shared flow-editing core, keeping both front-ends in lock-step.

- **Uniform chips, an editable `WITH` block and a filterable request picker for
  the GUI report editor.** A round of polish makes the block editor feel more
  direct. Every chip is now laid out to the **same height**, so a row no longer
  grows taller where it hosts a dropdown or text field, and the drop **ghost
  matches that height** so the gap that opens is exactly the size of the block
  being dropped. A modifier chip's `×` now reliably **detaches** it (its click is
  no longer swallowed by the chip's drag handle). An `AS` alias is edited **in
  place** through a small text field right on the chip, and a report request's
  `WITH … END` fields now render as a **nested block** under the line — like a
  `for` loop — where each `name: query` field has an *add-field* affordance and a
  little wizard, rather than being crammed onto the line. The request picker is
  now a single **filterable combo** with a type-to-narrow search box, mirroring
  the terminal UI's request filter. Finally, the delete target is a distinct
  full-width **trash bar** that only appears while you are dragging a block or
  chip, so it never reads as just another palette block. The `WITH`/alias edits
  run through the shared flow-editing core, keeping both front-ends in lock-step.

- **A resizable report validation panel and a Response "compact view".** The
  report editor's validation panel can now be **dragged taller** (GUI) / shows
  many more problems at once and scrolls (TUI), so a report with a long list of
  errors no longer hides them below the fold. The response viewer gains a
  **compact view** toggle — a button in the GUI, the `c` key in the terminal UI
  (while the Response pane is focused) — that shortens long string *values* to a
  `"head…tail"` overview so a body full of opaque tokens/hashes/base64 can be
  skimmed at a glance. It is display-only: copying still yields the **full,
  untruncated body**. The truncation itself lives in the shared core, so both
  front-ends stay in lock-step.

- **The compact overview now copies the full string even from a partial
  selection (TUI).** Previously only a whole-panel copy of the compacted
  Response view returned the untruncated body; drag-selecting a shortened
  `"head…tail"` value copied exactly what was on screen. The compaction now
  records a column map back to the full body, so a drag-selection over compacted
  text copies the **full, untruncated** value(s) it covers. The `c` compact-view
  toggle is also listed in the `?` help panel. The terminal footer drops four
  rarely-needed shortcut hints (`r` reload-var, `f` file, `s` settings and the
  `[`/`]` tab-cycling reminder) so it no longer overflows on a narrow terminal —
  the keys themselves still work.

- **GUI polish: app icon, resizable block palette, roomier edit fields, an "All"
  request view, and clearer report chips.** The desktop GUI now shows the
  PaperBoy logo as its window/taskbar icon and in the status bar (in place of
  the generic gear). In the PaperTrail block editor the divider between the
  block **palette** and the report is now **drag-resizable** (like the other
  splitters). Every key/value table (request Headers, Params, Cookies, Options,
  Captures and the environment variable editor) now **stretches its value field
  to fill the panel** instead of leaving it stuck at a narrow fixed width. The
  request editor gains an **"All"** tab — now the default — that stacks every
  section (Params, Headers, Body, Auth, Cookies, Options, Asserts, Captures) in
  one scrollable form, and a **Name** field, mirroring the terminal UI's
  edit-request wizard. Finally, PaperTrail keywords/chips are now **coloured by
  category** (`REPORT`/`WITH` in the substitution colour, `RESPONSE` in the
  accent, `SHOW` green, `HIDE` dimmed, `AS`/`BASELINE`/`COMPARISON` amber) in
  both the GUI chips and the terminal source highlighter, so a long report line
  reads at a glance.

- **GUI follow-ups: roomier key fields, aligned form rows, distinct
  `WITH`/`PARALLEL` chips, and a Linux taskbar launcher.** Key/value tables
  (request Headers, Params, Cookies, Options, Captures, the environment variable
  editor and the Body form/multipart fields) now **split their spare width ~35%
  key / ~65% value** so the key grows with the panel too, instead of staying a
  fixed sliver next to a filling value. The Body **form/multipart rows are now a
  grid**, so the Text/File kind dropdowns and their values line up in shared
  columns rather than drifting downwards row by row. PaperTrail chip colours are
  further de-conflicted: **`WITH`** now reads in the accent (a block opener,
  distinct from `REPORT`'s substitution colour) and **`PARALLEL`** in the error
  hue, so it stands apart from the blue loop/`SET` chips it sits beside — mirrored
  in the terminal highlighter. On Linux the GUI now installs a per-user
  freedesktop launcher (`~/.local/share/applications/paperboy.desktop` plus a
  logo copy under `~/.local/share/paperboy/`) so the **taskbar/dock shows the
  PaperBoy logo** rather than a generic icon; it is written only if absent and
  never blocks launch.

- **GUI PaperTrail chip polish: aligned dropdown labels, matched field
  backgrounds, and a distinct `REPORT` colour.** The keyword label on a chip
  that hosts a dropdown (`BASELINE`/`COMPARISON`/`REQUEST`) is now **vertically
  centred against the combo-box text** rather than floating above it. The
  editable **`AS` alias field** now paints with the same lighter fill as the
  neighbouring combo-box buttons instead of the darker sunken text-edit
  background, so a report line reads uniformly. And a report's **substituted
  values** (the reported variables and computed templates) are now drawn in the
  plain text colour, leaving the **`REPORT`** keyword its substitution colour so
  the two are no longer indistinguishable — matching the terminal highlighter,
  where reported values are plain identifiers.

- **GUI key fields now grow with the panel instead of collapsing to a sliver.**
  In every key/value table (request Headers, Params, Cookies, Options, Captures,
  the Body form/multipart fields and the environment variable editor) the
  **key** text box now reliably takes ~40% of a row's free width (the value
  fills the rest). A bare `TextEdit` clamps its width to the grid cell's
  available width, which stays tiny for a non-last column whose neighbour fills,
  so the key rendered as a ~24px sliver regardless of the intended split; the
  field is now allocated its width up front so the ~40/60 key/value split
  actually takes effect.

- **GUI report block editor: deeper nesting indent, stable selection, and
  click-to-deselect.** Statements inside a `FOR`/`PARALLEL`/`WITH` block are now
  indented further (per-level step widened) so nesting is easier to read at a
  glance. Selecting a block now **recolours it in place** without changing its
  size or position — the selection outline previously used a thicker stroke that
  expanded the chip by a pixel and nudged it and its neighbours. And **clicking
  any empty space in the block pane deselects** the current block (matching the
  usual editor gesture), so you're no longer stuck with a block selected.

- **GUI report results: a cell inspector.** Clicking any cell in a report's
  Results grid now opens a small floating window showing that cell's **full,
  unflattened value** (JSON bodies are pretty-printed one field per line),
  so a long string that's truncated in the grid can be read in full, selected,
  and copied (a **Copy full value** button copies the whole cell). Esc or the
  window's close button dismisses it. This mirrors the terminal UI's result-cell
  popup.


## [0.1.10] - 2026-08-03

### Changed

- **Creating a new report now goes through a destination-folder browser.**
  Previously, `Shift+R` in a Workspace tab (and `R` in the workspace picker)
  opened a bare "name a new report" prompt that only accepted a path relative
  to the workspace root. Both now open a folder browser instead, seeded to the
  highlighted folder (or the workspace root), so you navigate to where the
  report should land and name it there (a missing extension defaults to
  `.trail`). In a Workspace tab, `Shift+R` opens this browser no matter which
  pane has focus — so a report started while viewing the workspace always lands
  *in* the workspace. If the chosen folder lies inside an open Workspace, the
  report is created **embedded** in that workspace's tree; otherwise it opens as
  a **standalone** report tab bound to the file. Pressing `Ctrl+N` in the
  browser is an escape hatch that abandons the folder choice and opens an
  unsaved scratch report tab instead.

  When launched from within a Workspace the browser is **scoped to that
  workspace**: only folders are selectable and it can't navigate above the
  workspace root, so a report can only land inside the workspace. The
  workspace's own files (collections, environments and existing reports) are
  shown alongside the folders — non-selectable — so it's visually clear the
  picker is scoped inside the workspace. Typing a `subfolder/name` path in the
  filename field creates the subfolder on the spot. A destination that
  *resolves* outside the workspace once symlinks are followed — e.g. through a
  symlinked folder — is refused rather than silently written outside the tree.

- **Report tabs now use the same 📊 icon as reports in the Workspace tree**, so
  a report reads the same whether it's a standalone tab or a workspace row.

### Added

- **`WITH` fields can now rename intrinsics, carry statistics, and use quoted
  names.** In a `REPORT REQUEST … WITH … END` block, a field's value may be an
  intrinsic name (`HttpStatus`, `Time`, `Asserts`, `Error`, `Response`) to alias
  that intrinsic under a friendlier column — e.g. `Status: HttpStatus` — instead
  of only accepting a Hurl query. A field may append its own
  `STATISTICS(MEAN, …)` clause (identical to a `columns:` statistics clause,
  attached to the field's `alias.name` column), and a field name may be a quoted
  string so a column header can contain spaces (`"Response Time": Time`). All
  three compose: `"Response Time": Time STATISTICS(MEAN, MEDIAN)`.

- **A configure wizard for `FOR … IN FILES` loops, and a `PARALLEL` toggle on
  every loop wizard.** In the report node editor, `Enter` on a `FILES` loop now
  opens a structured form — like the `ENVS` one — to set the loop variable, pick
  the source folder (the file picker is a keystroke away, and pre-selected for a
  freshly-inserted loop), type an optional `MATCH` glob, and toggle whether the
  loop runs `PARALLEL`. The `ENVS` wizard gained the same `PARALLEL` checkbox, so
  a loop's concurrency can be set from the node editor instead of by hand.

- **JSON cell values are pretty-printed in the report cell viewer.** Drilling
  into a report-grid cell whose entire value is a single JSON document (`Enter`
  on the cell) now shows it indented, one field per line, instead of a dense
  single line. Cells that aren't whole-value JSON are shown unchanged.

- **Type-to-filter in the load browsers.** In the Open Collection / Load
  Environment / Open Report file dialogs, start typing to filter the visible
  files by name (case-insensitive substring) on top of the existing extension
  filter. Backspace trims the query and the first Esc clears it (a second Esc
  then closes the dialog); an active filter is shown beneath the list.

- **Inserting a node in the report node editor opens its configure view
  immediately.** Picking a kind from the node palette now drops you straight
  into that node's most helpful editor — the same view `Enter` opens on an
  existing node — instead of the raw line prompt: a `FOR … IN FILES`/`FOLDERS`
  loop opens the source-folder browser (choosing the folder is the whole point
  of the loop), and a `FOR … IN ENVS` loop opens the baseline/comparison/mode
  popup. Kinds without a dedicated form yet (report-var, assignment, list) still
  open the line prompt.

- **Reuse a saved baseline snapshot inside an `ENVS` comparison with
  `FILE(…)`.** A `BASELINE(…)`/`COMPARISON(…)` role argument may now be
  `FILE("path")` instead of an environment name — e.g.
  `FOR TARGET IN ENVS BASELINE(FILE("prod.baseline")), COMPARISON("staging")`.
  The named `.baseline` snapshot (the same kind exported from the results grid,
  resolved relative to `# root:`) is loaded once and stands in for a live run of
  that role, so a fixed reference can be compared against without re-running it;
  the live comparison env still runs each time. Accepted on both roles, so you
  can diff live-vs-snapshot either way, or snapshot-vs-snapshot. In the ENVS
  configure overlay, press `f` on a role to turn it into a `FILE` reference and
  cycle it through the snapshots found in the report's directory. This is the
  loop-scoped counterpart of the report-wide `# baseline:` directive.

- **Rerunning a report warns before discarding unexported results.** Running a
  report again (`r` / `F5`) when the current results haven't been saved anywhere
  since the run that produced them now asks to confirm first — the results would
  otherwise be replaced with no way to get them back. Exporting the results
  (CSV / JSON / HTML / XLSX) or saving a `.baseline` snapshot counts as saving
  them, so the next rerun goes straight through; the warning also never
  interrupts cancelling a run that's still in flight.

- **The Help window (`?` / `F1`) is now searchable.** Start typing to filter
  every tab's entries down to those whose shortcut/keyword or description
  contains what you typed (case-insensitively), keeping each match's section
  heading and dropping sections with nothing left; the active filter is echoed
  under the tab strip. The filter persists as you switch tabs (`Tab` / `←→`), so
  a search can be checked against the Shortcuts, Glossary and Reports views in
  turn; Backspace trims it, the first `Esc` clears it and a second `Esc` closes
  Help. Scrolling (`↑↓` / `PgUp` / `PgDn` / `Home` / `End`) and the grouped,
  titled sections are unchanged.

- **`Ctrl+↑` / `Ctrl+↓` page the report results cursor.** In a results grid the
  Ctrl-modified up/down arrows now move the cell cursor a whole screenful at a
  time (clamped to the grid), so a long report can be traversed quickly without
  holding an arrow key. Plain arrows still step one cell; `Ctrl+←/→` still cycle
  tabs for a standalone report.

- **The Response pane now shows the request duration.** Alongside the status
  line it reports the transfer time (e.g. `Time: 123 ms`) — the same figure a
  report surfaces as the per-request "Time" column — for the selected entry's
  last run.

- **Summary statistics for report columns with `STATISTICS(…)`.** A column can
  now request one or more summary statistics — numeric `MEAN`/`MEDIAN`/`MIN`/
  `MAX`/`SUM`/`STDDEV`, `MODE` and `COUNT` (any column), or `DISTRIBUTION` (a
  per-value count for a categorical column such as an overall verdict). Attach
  them inline in the `columns:` directive
  (`proc.Time AS "Time (ms)" STATISTICS(MEAN, MEDIAN)`) or on a `REPORT`
  statement (`REPORT Overall AS Verdict STATISTICS(DISTRIBUTION)`); the computed
  values are appended as footer rows below the data, and shown in the in-app
  grid (styled as an italic-accent footer) and in every CSV/JSON/HTML export. In
  the **xlsx** export the numeric statistic cells are written as **live
  spreadsheet formulas** (`AVERAGE`, `MEDIAN`, `SUM`, `MIN`, `MAX`, `STDEVP`,
  `MODE`, `COUNT`, `COUNTIF`) over the data range, so they recalculate if you
  edit the sheet. The `FILES`/`ENVS` loop wizards are unaffected. The Reports
  Help tab (`?`) documents the new `STATISTICS`/`HIDE`/`BASELINE(FILE(…))` forms,
  and a from-scratch guide covering every PaperTrail feature lives at
  `docs/reports/00-tutorial.md`. See `docs/reports/02-grammar.md` §8.1.

### Changed (backlog)

- **Report column titles stay pinned while scrolling.** The results grid's
  header row now stays fixed at the top of the pane as the data rows scroll
  underneath it, so you can always see which column you're reading in a long
  report. Mouse-wheel and `Ctrl+↑/↓` paging scroll the body without disturbing
  the header, and clicking the header row still starts a text selection rather
  than selecting a cell.

- **The report results footer no longer explains the arrow keys.** The
  `↑/↓/←/→ cursor` segment was dropped from the results hint line (the arrow
  keys are self-evident); the `Enter` drill-down / `v` / `x` / `B` hints remain.

- **Selected rows in the request, workspace and environment lists no longer show
  a leading `› ` caret.** The selected row is already highlighted, so the caret
  was redundant; dropping it reclaims two columns of width for the row text.

- **Exporting a report's results moved from `x` to `Ctrl+S`.** A bare `x` in a
  report view used to export the last run, but `x` deletes the selected
  environment/request one pane away in the collection view, so the shared key
  felt unsafe. Export is now `Ctrl+S` (the help window, results hint line and
  `paper_trail.md` reference are updated to match); `x` no longer does anything
  in a report view.

### Fixed

- **Arrowing up out of the request wizard's Body returns to the column you were
  editing.** Leaving the multiline Body upward used to drop onto the Form
  section's "+ Add" row, losing the table cell you came from. It now returns to
  the exact Headers/Cookies/Queries/Options or Form cell (row *and* column) that
  was last focused, falling back to the old section-step only when no table cell
  has been visited.

- **A collection that fails to parse now explains *why* instead of just "not a
  valid collection".** When a `.hurl` file can't be loaded, the status (and the
  CLI/`--batch` and report-runner messages) now name the offending line and the
  concrete reason from the parser — e.g. `line 54: parsing filename` for a
  `[Multipart]` `file,;` with an empty filename — so a single malformed line
  that makes `hurl_core` reject the whole file is easy to find. A failed Postman
  JSON import still shows the generic message (a Hurl-parse reason would be
  meaningless there).

- **PaperBoy no longer writes a `.hurl` file it can't read back.** A
  `[Multipart]`/`[Form]` file field left with no file path used to serialize to
  an invalid `file,;` line — which PaperBoy's own parser then rejected on
  reload, stranding the whole collection. Saving such a collection (locally, to
  a Workspace, when moving/copying a request between files, or when pushing to
  git) is now refused with a message naming the request and the empty field, so
  the problem is fixed before it reaches disk.

- **The load-browser filter strip now matches the theme.** The one-line
  "Filter: …" strip shown beneath the file list while type-to-filtering was
  drawn on the terminal's default background (and as a single flat accent
  colour), so it stood out from the rest of the dialog. It now fills the theme
  panel background with a dim label + accent query, like the export-format strip
  and the Help filter.

- **Backspace in the request wizard's Asserts/Captures/Reports cells no longer
  types a literal `h`.** On terminals without the keyboard-enhancement protocol
  `Backspace` arrives as `Ctrl+H`; the wizard's text cells now treat that as a
  delete (matching the multiline editor) instead of inserting an `h`.

- **The mouse wheel now scrolls a report results grid freely.** With a cell
  highlighted, the wheel used to be pinned — every frame re-centred the view on
  the selected cell, so scrolling snapped straight back. The wheel now scrolls
  the viewport directly (leaving the cursor put, even off-screen); the view only
  re-centres when keyboard navigation actually moves the cursor.

- **The report cell drill-down popup grows to fit long values.** A cell whose
  value is one long line (e.g. a big JSON body) used to open a two-line popup
  because the box was sized by logical line count, ignoring soft-wrapping. It is
  now sized to the wrapped row count (still capped at the terminal height), so
  there's far less scrolling inside the popup.

- **Numeric report columns export as real numbers in `.xlsx`.** Every cell used
  to be written as text (with a leading `'` guard), so a column like `Time`
  couldn't be summed or averaged in a spreadsheet. Columns whose every value is
  a number are now written as numbers; identifier-like values (empty, or with
  redundant leading zeros) and mixed columns stay text. CSV export is unchanged.

- **The Response pane no longer shows "Sending…" for idle requests.** While one
  request was in flight, *every* request's Response pane showed the sending
  spinner because it was driven by a single shared flag — so you couldn't look
  at another request's last response until the send finished. Sending state is
  now tracked per request: only the request that's actually in flight shows the
  spinner, and selecting any other request shows its own last response.



## [0.1.9] - 2026-08-03

### Fixed

- Mouse feedback now matches the selected-row model: first clicks select rows,
  second clicks activate Global and Workspace environments plus structured
  report nodes, and only the primary Run hint starts the selected request.


## [0.1.8] - 2026-08-01

### Added

- **Conventional mouse navigation across the TUI.** Visible tabs, menus,
  request/environment rows, report grids and node outlines, request-wizard
  fields/dropdowns, confirmation choices, browser rows, theme controls and
  scrollable panels now respond to ordinary left-click and wheel input while
  preserving the existing text selection, scrollbar dragging and keyboard
  behaviour.


## [0.1.7] - 2026-07-30

### Changed

- **The request wizard's section titles are now coloured bands.** Each section
  header (Headers, Cookies, Queries, Options, Form, Body, Asserts, Captures,
  Reports) is drawn as a full-width filled strip rather than plain text, so in
  the stacked "All" view it's obvious where one section ends and the next
  begins. The section the cursor is in gets a solid accent bar (matching the
  active section-tab styling); the others get a subtle inset band, and empty
  sections' compact `Label   (＋ Add …)` lines share the same banding, with
  their labels padded to a common width so the `(＋ Add …)` actions all line up
  in one column despite the differing label lengths.

- **The build no longer needs a system libcurl.** libcurl and OpenSSL are now
  compiled and statically linked from source (via the `curl` crate's
  `static-curl`/`static-ssl` features), so the resulting binary is
  self-contained with no runtime libcurl dependency. Building now requires a C
  compiler, `perl` and `make`; the previous hand-written pkg-config shim
  (`.cargo/config.toml` + `.curl/`) has been removed.

- **Stopping a report run now keeps the partial results.** Previously, pressing
  `r` to cancel a running report discarded all streamed rows and restored
  whatever grid was showing before the run started. Now the partial grid is
  retained: rows that finished keep their real responses, and rows that hadn't
  started yet remain as greyed skeleton placeholders. The view stays on the
  Results grid so the partial output can be inspected, saved, or exported
  immediately. Closing a running report tab also retains the partial result in
  the stashed tab (reopenable with `u`). A new status message "Run stopped —
  partial results kept" reflects the change.

- **The response's expected status is now editable in the request wizard.** A
  request's `HTTP <code>` status expectation is surfaced in the `[Asserts]`
  table as a `status == <code>` row, so it can be changed or removed like any
  other assert (previously it was only reachable through raw Hurl/JSON editing).
  Editing the row updates the expectation and typing a new `status == <code>`
  assert sets it; both round-trip back to the canonical `HTTP <code>` line.

### Added

- **Workspace tree: environments show as their own rows, `.vars` is filtered
  in, and `Ctrl+F` toggles the filter.** Environment files (`.vars`) in a
  workspace now appear in the tree with a distinct icon and open (Enter / Right)
  as a global environment — the same as File → Load → Environment — instead of
  being mis-parsed as a collection. The tree's file-type filter now includes
  `.vars` alongside `.hurl`, `.json` and `.trail`, and **Ctrl+F** toggles that
  filter directly from the tree (previously only reachable from the `w`
  picker's Tab), so folders cluttered with images or other files can be shown or
  hidden without leaving the tree. The choice is persisted per workspace.

- **The request `[Options]` section is now editable in the wizard.** A new
  **Options** section tab (between Queries and Form) lets you add, edit, disable
  and delete Hurl request options (`retry`, `insecure`, `variable: host=…`, and
  so on) as a key/value table, just like Headers or Queries. It cycles with the
  other tabs (`[`/`]`, PageUp/PageDown) and has a direct **Alt+4** jump; the
  remaining section jumps shift up by one (Form is now Alt+5 … Reports Alt+9).

- **Workspace file tree: real expand/collapse replaces the old breadcrumb.**
  The workspace tab's file list is now a proper multi-folder tree: any number of
  folders can be expanded at once, their open/closed state persists across
  restarts, and you can see files from several parts of the tree simultaneously.
  Navigation keys follow normal file-tree conventions — **Right** / **Enter**
  expands a collapsed folder (Enter on an already-open folder collapses it);
  **Left** collapses an expanded folder, or moves the cursor to the parent of a
  collapsed folder / file; **Up**/**Down** move across visible rows as usual.
  Opening a collection file or a report works exactly as before.  The old
  breadcrumb ("enter one folder at a time" + `../` row) is gone; there is no
  longer a "current folder" appended to the panel title.  The expanded-folder
  set is saved in `state.json` under a new `workspace_expanded_paths` field;
  older state files without the field load cleanly with all folders collapsed.

- **Workspace tree: expand a collection to list its requests inline.** Opening a
  collection in the workspace tree now keeps its request names listed beneath it
  until you collapse it, giving a clearer picture of the whole workspace.
  Several collections can be expanded at once, and each collection's expanded
  state persists across restarts alongside folders (same
  `workspace_expanded_paths` set). **Right**/**Enter** on a collection loads and
  expands it; **Left** or a second **Enter** collapses it. Requests of a
  collection that isn't the loaded one are shown dim by name only; highlighting
  one just previews its name, while **Enter**/**Right** loads that collection and
  jumps straight to the highlighted request. The loaded collection's name is
  drawn in the accent colour (and the others dim) so it's clear which collection
  the coloured requests belong to.

- **Dry-run preview now shows the real output grid.** Pressing `d` on a report
  opens the same column/row table the full run would produce — but with all
  HTTP-response fields blank because no request is sent. Loop bindings, variable
  assignments, `ZIP` pairings, producer structure and column headers are all
  resolved and visible, so the shape and size of the run are immediately clear
  before you commit to sending any traffic.

- **Variable-availability static analysis.** The report validation panel (and
  the dry-run overlay) now warns when a `{{VAR}}` referenced by a request may
  not be defined by the time that request runs. The check walks the flow in
  execution order, tracking variables from the environment, `FOR` loop binders,
  explicit assignments, `FOLDERS … WITH` role names, and `[Captures]` blocks of
  earlier requests. It is conservative — sources that can't be statically
  resolved (provider references, `TUPLES FROM` column names, unknown env names)
  are treated as "may define" so no false positives are emitted. Warnings are
  non-blocking and never prevent a run.

- **Drill down into any Results-grid cell with a popup.** In the Results view
  of a report, arrow keys now move a highlighted cell cursor across the grid.
  Pressing **Enter** (or clicking a cell a second time) opens a scrollable
  popup showing the column name and the full cell value — useful for long
  values that are truncated in the grid. The popup supports text selection and
  copy (matching the existing request/response panels), and **Esc** closes it.

- **`REPORT REQUEST … HIDE(a, b, …)` drops columns you don't want.** Mirroring
  `SHOW(…)`, a `HIDE(…)` clause removes the named field suffixes (intrinsics like
  `Response`/`Time`, `[Reports]` fields or `WITH` fields) from a request's report
  output. It is applied last, so it works in every case — the default column set,
  a `SHOW(…)` selection, or a `WITH`-restricted one. Naming the same field in both
  `SHOW` and `HIDE` is now a validation error, and both keywords are highlighted
  in the report source editor.

- **Keep baseline fields when comparing environments.** In a
  `FOR … IN ENVS BASELINE(…), COMPARISON(…)` comparison, a `SHOW(field, …)` clause
  after `BASELINE(…)` copies the chosen baseline fields into each candidate row as
  `baseline.<request>.<field>` — but only for requests that actually report that
  field. This lets a report show, say, both environments' `Time` side by side so
  you can spot a performance regression, not just the comparison env's timing.
  `SHOW` is only valid on `BASELINE` (it's a parse error on `COMPARISON`), and the
  new columns are selectable/renamable through the `columns:` directive like any
  other.

- **Load choosers hide files that can't be what you're opening.** The local
  "Open Collection", "Load Environment" and "Open Report" browsers now show only
  the matching file types (`.hurl`/`.json`, `.vars`/`.env*`, `.trail`) plus
  folders to navigate — the same sets the git picker already uses — so loading
  from a busy directory isn't buried under unrelated files. Press `Tab` to toggle
  the filter off (show everything) and on again for an oddly-named file.

- **Workspace picker shows report files with an icon and updated filter label.**
  Report files (`.trail`) now display with a report icon (📊) in the workspace
  quick-browse popup (opened with `w`), so they stand out from plain collection
  files. The popup's filter label is also updated to show
  "Filter: .hurl/.json/.trail" instead of just ".hurl/.json".

- **Pick a `FOR … IN ENVS` loop's environments from the loaded ones.** In the
  report node editor, pressing **Enter** on a `FOR … IN ENVS` node now opens a
  small configure form instead of the raw line editor: choose the loop
  variable, switch between **Iterate** (`ENVS "a", "b"`) and **Compare**
  (`ENVS BASELINE(…), COMPARISON(…)`) mode, and pick each environment by cycling
  through the environments you've actually loaded (`←/→`) rather than typing
  their names. `b` marks the baseline, `n` adds an environment and `x` removes
  one — mirroring how the request node already cycles request names.

- **Revert a request or an environment to its last saved version.** Press
  **Ctrl+R** in the Requests list to discard a request's in-memory edits and
  reload it from the collection's file on disk, or **Ctrl+R** in the environment
  entries popup to drop every unsaved change to that environment (edited values
  go back to their saved value; hand-added variables are removed). Both actions
  ask for confirmation first (there is no undo) and are a no-op with an
  explanatory status when there's nothing to revert — a scratch collection/env
  with no file, an unedited request, or an environment with no unsaved changes.

- **`REPORT <var> AS <name>` renames a variable's column inline.** Alongside
  `REPORT (VAR)` (which uses the variable's own name as the header) you can now
  write `REPORT FILE AS "Pretty name"` to project a single variable under a
  chosen column heading, without a separate `# columns:` directive. The pretty
  name follows the usual quoting rules (quote it when it contains spaces or
  punctuation); a bare word needs no quotes. Round-trips through the raw editor
  and the structured node editor.

- **`paperboy -r report` can resolve its collection and environment from the
  report itself.** The headless report runner no longer requires `-c`: when it
  is omitted, the report's own `# collection:` header is used, resolved relative
  to the report's folder (so a workspace report "just runs"). Likewise, with no
  `-e`, the report's `# environment:` header supplies the base variables.
  Explicit `-c`/`-e` flags still override the headers; a report with neither a
  flag nor a `# collection:` header fails with a clear error.

- **Postman import now recovers `[Captures]` from test scripts.** A request's
  Postman `test` script often stores a value out of the response with
  `pm.environment.set("token", jsonData['token'])` (or `.collectionVariables`/
  `.globals`/`.variables`); the importer now scans those calls and emits the
  equivalent `[Captures]` line (`token: jsonpath "$.token"`), so captured
  variables survive the import instead of being dropped. Simple accessor chains
  (`json['a']['b']`, `json.a.b`, array indices) are mapped; anything more exotic
  is skipped rather than guessed.

- **Multiple `-e/--env` environments on the CLI report runner.** `-e` is now
  repeatable, so a headless report that compares environments —
  `FOR TARGET IN ENVS BASELINE("prod"), COMPARISON("staging")` — can be run with
  `paperboy -c coll.hurl -e prod.vars -e staging.vars -r report.trail`. Each
  file is loaded and made selectable by its file stem (the name an `ENVS` clause
  references); the first `-e` also serves as the base variable layer for
  requests outside any `ENVS` loop. Passing two files that share a stem is a
  fatal error (an `ENVS` clause could not tell them apart). A single `-e` behaves
  exactly as before; a plain collection run (`-c` without `-r`) still uses only
  the first environment and warns if more are given.

- **Live per-row status icons in the streaming results grid.** While a report
  runs, each row now shows a status marker in a leading column: `·` (dim) for a
  row still scheduled, `…` for a row whose requests are in flight, and `✓`
  (green) once its result lands — reusing the same glyphs as the collection
  view's Run-All markers. Under a `PARALLEL` loop several rows show `…` at once,
  so the grid makes it obvious at a glance what has finished, what is running,
  and what is still queued.

- **Create a new report directly in a workspace.** In the workspace file
  picker, press **R** to name and create a brand-new `.trail` inside the
  workspace (subfolders allowed; a missing extension defaults to `.trail`).
  The file is written straight away, appears in the workspace tree next to its
  collections, and opens as a workspace-pinned report ready to bind to a
  collection and edit — mirroring the **n** new-collection action.

- **Load and save `.trail` files to a git remote.** Reports now use the same
  git flow as collections and environments: **File → Load → Report → Git** pulls
  a `.trail` straight from a repo (no local clone — only that file is fetched),
  and **File → Save → Report → To Git…** pushes the report's source back,
  repinning its origin so the next save appends to the same branch. This lets a
  team keep a report versioned alongside the collection it drives.

- **Reports warn up front when a `# baseline:` snapshot is missing.** If a
  report references a saved `.baseline` snapshot that isn't on disk, the report
  view (and the CLI) now flag it as a warning while you edit — instead of only
  finding out mid-run that there's nothing to compare against.

- **Reports export to JSON, HTML and Excel, not just CSV.** A report's results
  can now be written in four formats, chosen by the output file's extension (or a
  `# output:` header): **CSV**, **JSON** (a `{ columns, rows }` document),
  **HTML** (a self-contained, styled page you can just double-click open in a
  browser — ideal for handing a run to someone with no spreadsheet program), and
  **`.xlsx`** (a real Excel workbook). The HTML and xlsx outputs colour-code
  recognisable status/result cells (green = pass/`OK`, red = error, amber =
  changed) so a large run is easy to scan, exactly like the hand-made reports
  this feature replaces. In the report view, `x` (export) picks the format from
  the filename you type; on the command line, `-o out.xlsx` (or `-o out.html` /
  `-o out.json`) does the same, and omitting `-o` uses the report's `# output:`
  format. (The `.xlsx` writer is pure Rust — no external tools required.)

- **Turn a request into a reported one (or back) from the node editor.** The
  report node editor's per-node configure form now has a **Report** checkbox:
  tick it to promote a plain `REQUEST` into a `REPORT REQUEST` (which reveals
  the response-format, alias and field options), or un-tick it to drop reporting
  again — no need to retype the line. The request name is now chosen inline on
  the form too (Space/←→ cycle through the bound collection's requests). This
  answers "how do I add REPORT to a line in the node editor?" without leaving
  the structured view.

- **Open a report in place inside its workspace tab.** A `.trail` in a Workspace
  tab's file tree now shows *in that same tab's right pane* — the tree stays on
  the left driving navigation, exactly as it does for collections and requests,
  so a workspace no longer splits into a separate report tab. Selection follows
  the tree highlight: moving the cursor onto a report row shows it embedded (no
  `Enter` needed, just like landing on a request row shows that request), and
  moving off it returns the pane to the request/response view — the report is
  retained in the background with its edits intact, so highlighting it again
  re-shows it instantly. `Enter` on a report row opens its node editor (the
  report equivalent of a request's edit wizard) and moves focus into the body;
  `Tab` moves focus between the tree and the report body. The tree keeps focus
  and its highlighted row throughout, so selecting a report no longer jerks the
  cursor back to the top, and every report action (edit, node editor, run,
  results grid, dry-run, bind, columns, export, undo, save/revert) works
  unchanged on the embedded report. The report a workspace tab is showing is
  saved with the session and restored in place on the tree — as is a
  highlighted-away report that still has unsaved edits, so moving off a dirty
  report and quitting no longer loses those edits. Standalone reports
  (File → Load Report, with no workspace) are unchanged — they still open as
  their own full-screen tab.

- **Run reports from the command line.** A report can now be run headlessly
  without opening the TUI: `paperboy -c collection.hurl -e env.vars -r
  report.trail` runs the flow and writes its table, then exits — ideal for
  scripting, CI, or a scheduled nightly run. `--dry-run` expands the report and
  prints the projected table without sending a single request (handy before a
  big run), and `-o` chooses where the output goes: `-o -` streams clean CSV to
  stdout for piping (all human/progress text is diverted to stderr), `-o
  out.csv` writes a named file, and omitting it derives the filename from the
  report's `# output:`/`# name:` headers next to the report file — honouring the
  `{time}` token so repeated runs don't overwrite each other. Live runs print a
  `done/total` progress counter as rows complete. Validation errors block a live
  run (as in the TUI) but a `--dry-run` still previews; `-r` requires `-c`.
- **Report results stream in live, row by row.** Running a report no longer
  waits for the whole run to finish before showing anything: the results grid
  appears immediately as a greyed-out skeleton of every projected row (in
  canonical order, so you see the run's shape and size up front), then each row
  lights up and fills with its response as that iteration completes, with a
  running `done/total` progress count in the status bar. Ideal for the big runs
  (500–1000 documents) — you get a real sense of progress instead of a frozen
  wait, and can watch which rows are done and which are still pending. Rows
  arriving out of order (under `PARALLEL`) still land in the right slot, and the
  final comparison/`Result` verdict is folded in once the run finishes. Cancel
  (a second `r`) still discards the partial run and restores the prior grid.
- **Timestamp your report output with `{time}`.** Put `{time}` anywhere in a
  report's `# name:` (e.g. `# name: staging_{time}`) and every file the run
  writes — the CSV export, a saved `.baseline` — is stamped with the local time
  it was produced (`staging_2026-07-26-204500.csv`, `YYYY-MM-DD-HHMMSS`), so
  running the same report repeatedly leaves a trail of files instead of
  overwriting one. The token expands only when a file is written (the source and
  the tab name keep the literal `{time}`), and a name with a token drives the
  export filename even for a saved report — landing next to it.
- **Report dry-run preview marks soft-wrapped lines.** Long binding/error lines
  in the dry-run overlay (`d`) now show the same dim `↵` end-of-line marker the
  Request/Response panels use, so a wrapped sample reads unambiguously as one
  logical line instead of several — much easier to scan.
- **Undo in the structured node editor (Ctrl+Z).** The node editor now keeps a
  per-report undo stack: every structural edit (insert, replace/edit, delete,
  move, folder pick, and the REPORT REQUEST detail form) snapshots the flow
  first, so **Ctrl+Z** takes back an accidental change — restoring both the
  source and the node selection — and can be pressed repeatedly to step back
  through the session's edits. It mirrors the source editor's Ctrl+Z, and a
  brief status confirms each undo (or notes when there's nothing left to undo).

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
  `FOR … IN FILES/FOLDERS` loop's source directory (no path typing) or, on a
  `REPORT REQUEST` node, a **detail form** — cycle its response format
  (`RESPONSE RAW/PRETTY`), type an `AS` alias, and tick which of the fields it
  can emit (its intrinsics, `[Reports]` fields and any `WITH` fields) are shown
  (`SHOW(…)`), so a noisy field (e.g. a base64 `Response`) can be dropped
  without editing text (leaving everything ticked emits them all). **Del**/Backspace
  removes the node, and **Shift+↑/↓** (or `K`/`J`) moves it among
  its siblings. Both editors are views over the same
  flow AST — every structural edit re-serializes back to the source text — so
  you can freely switch between them, and the logic is front-end agnostic for a
  future GUI. `?` Help and the Reports page document the node keys.
- **PaperTrail reports — new Reports view (work in progress).** A report is a
  new kind of tab, opened with **Shift+R**, that lives alongside the collection
  tabs but takes the whole body (no list / environment / response panels, so it
  fits small screens). Each report holds a PaperTrail flow (`.trail` source)
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
- **Chain loop sources end-to-end — `CONCAT(...)`.** A new producer,
  `CONCAT(a, b, …)`, appends the items of each input into one longer stream, so
  a single `FOR` body can run the same requests over documents gathered from
  several unrelated folders without duplicating the loop —
  `FOR DOC IN CONCAT(FILES "batch-jan", FILES "batch-feb", FILES "rescans")`.
  Unlike `ZIP` (which pairs positionally and needs equal lengths), `CONCAT`
  inputs may be different lengths and an empty input contributes nothing; every
  input must share the same arity (mixing e.g. a `FILES` with a `ZIP(...)` is a
  validation error). `CONCAT` composes with the other producers and can be
  named with `LIST`.
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
  A report is now a first-class file: **File ▸ Load ▸ Report** opens a `.trail`
  flow into a new tab, and **File ▸ Save ▸ Report** writes the active report back
  to its file (**Save As** opens a folder chooser seeded with `<name>.trail`).
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

- **Export format picker in the report export dialog.** Exporting a report's
  last run (**x**) now shows a `CSV / JSON / HTML / XLSX` strip above the
  filename, with the active format highlighted; **↑/↓** (while the filename
  field is focused) cycles it, rewriting the filename's extension. The format
  has always been chosen by that extension — so typing `.json` still works — but
  the picker makes it discoverable, and the dialog is no longer mislabelled
  "Export Report CSV" (its `x export` hint was likewise misleading).

- **Resize the workspace tree from the report view.** `<` and `>` now widen and
  narrow the pinned workspace column while viewing a report, the same as in the
  collection view.

- **Pencil marker on environments with unsaved edits.** The Global Environments
  panel now shows a `✎` in a column to the *left* of any environment name that
  has added or modified variables — matching the Requests list's modified/added
  marker placement (and the same glyph already used per-variable in the entries
  popup) — so unsaved changes are visible without opening the environment.

### Changed

- **Report binding and validation panels moved to the bottom.** In the report
  editor view, the collection binding info and validation diagnostics now appear
  at the bottom of the panel (below the source editor) instead of at the top.
  This keeps the layout stable when scrolling past different reports in a
  workspace — the sections that change height per report no longer cause jarring
  layout shifts above them.

- **`REPORT REQUEST` column selection now uses a union model.**  The emitted
  columns are the union of (a) the request's `[Reports]` fields, (b) any `WITH`
  fields, and (c) any fields explicitly named in `SHOW(…)`.  Intrinsics
  (`HttpStatus`, `Time`, `Asserts`, `Error`, `Response`) are included by default
  only for a *bare* request that has no `[Reports]` and no `WITH` fields; once
  any declared field exists, intrinsics are suppressed unless `SHOW` names them.
  `SHOW(…)` is now **additive** rather than a whitelist — it force-includes the
  named fields (its practical purpose is bringing a specific intrinsic back on a
  request that has declared fields); naming a field that is already in the set is
  harmless, and naming a non-existent field is silently ignored.  `[Reports]`
  and `WITH` fields are always emitted unless removed by `HIDE`.  This removes
  the previous asymmetry where a `[Reports]`-only request kept its intrinsics
  while a `WITH`-only request suppressed them — both now behave consistently.

- **Environment-comparison `Result` cells are now readable, structured JSON.**
  The old run-on `field: a→b; …` summary is replaced by a compact single-line JSON
  object keyed by environment name — the baseline carries a `(baseline)` suffix —
  listing only the fields that differ. Values that are themselves JSON are embedded
  structurally rather than escaped, so a differing breakdown reads as nested JSON
  instead of a wall of backslashes, and the cell can be parsed by JSON-aware tools.

- **The report grid highlights the request that's currently running.** While a
  report streams, the running row is drawn in the theme's `pending` colour and
  bold so it stands out at a glance; queued rows stay dimmed and finished rows
  return to normal. (Change the `pending` colour in your theme if you'd rather it
  read differently.)

  tab.** Selecting a `.trail` from a workspace tab's tree used to spawn a
  *separate* report tab (so one workspace could sit in the strip twice). It now
  replaces that same tab's right pane — the request editor + response give way
  to the report body while the pinned workspace tree stays on the left — exactly
  as opening a collection/request from a workspace never spawns a new tab.
  Opening a collection/request from within the embedded report returns the right
  pane to the request/response view (the report is kept, so re-selecting it
  restores its edits/results); the tree keeps its highlight on the report you
  opened instead of jumping back to the top; and the workspace tab's Save menu
  now offers Report alongside Request/Collection/Workspace. Which report is
  embedded is remembered across a restart, reopening pinned to its tree.
  Standalone reports (File → Load Report with no workspace) still open as their
  own tab, unchanged.

- **The File → Save submenu now only lists what you can actually save.** The
  Save menu was a fixed list of all six kinds (Request, Collection, Environment,
  Workspace, Report, Response) regardless of context; it now shows just the ones
  that apply. A collection tab offers Request and Collection (plus Workspace when
  it's workspace-backed); a report tab offers Report; Environment appears when an
  environment is loaded, and Response when there's a response to write. This
  removes the confusing (and previously no-op) cases such as "Save Request" or
  "Save Collection" while a report tab is active.

- **Report files now use the `.trail` extension (was `.report`).** A PaperTrail
  file describes *how to build* a report, not the report output itself, so the
  extension now matches the language (`.trail`) to make that distinction clear.
  This is a **breaking change**: existing `.report` files are no longer
  recognised anywhere (workspace trees, the git and local file pickers, the CLI
  `-r` runner) and must be renamed to `.trail` by hand. New reports are created
  and saved as `.trail`; the `# collection:`/`# environment:` headers inside a
  report are unaffected.

- **Enter opens the report node editor; `e` edits the raw source.** Pressing
  **Enter** in a report now opens the structured node editor — mirroring how
  Enter opens the request wizard on a collection — while **`e`** is the
  dedicated raw source-text editor. On a report whose source doesn't parse there
  is no node outline to show, so Enter falls back to the raw editor (the one that
  can fix the source). **Esc** backs out of the node view to the source view.
  The `n` key, which used to toggle source/nodes, is now unbound — reserved for a
  future "new request" binding.

- **Tab in a report only swaps focus with the workspace tree now; `v` swaps
  source and output.** Pressing **Tab** in a workspace report toggles focus
  between the report body and the pinned file tree — it never jumps onto the
  results grid (an easy mis-hit) and never stops on the tab bar. In a standalone
  report (no tree) Tab is inert. To flip the body between the editor and the run
  output, use **`v`** (advertised in the panel hint once a run exists). Switch
  tabs with `[`/`]`, PageUp/PageDown, or Ctrl/plain arrows.

- **Report source autocomplete is now case-insensitive and fixes casing.**
  Typing a request or environment name fragment matches regardless of case
  (typing `r` offers `Report value`), and accepting the completion rewrites the
  fragment with the name's canonical spelling — so a lowercased `r` becomes `R`
  rather than leaving `report value`.

- **The report source editor remembers your last cursor position.** Leaving edit
  mode (Esc) and returning (`e`) — or flipping to the node view and back — now
  restores the caret where it was instead of jumping to the end of the buffer
  (clamped to the current text if the source changed meanwhile).

- **Simpler, consistent keys in the report node editor.** `f` now always opens
  the **File** menu (as it does everywhere else) instead of doing double duty as
  a per-node "detail" key. Configuring a node — a request's options, a loop's
  folder, or an assignment's text — is now on **Enter** (a single "configure
  this node" form whose shape follows the node kind), and `e` remains the raw
  "edit the source line" escape hatch. The request form's long shortcut hint
  moved off the title onto a footer, so a long request name no longer truncates
  it.

- **Tab in the report source editor indents.** While editing a report's source,
  **Tab** now inserts four spaces (one indent level) instead of doing nothing —
  unless a request/environment name completion is pending, in which case it
  still accepts the completion. **Backspace** in a line's leading whitespace
  deletes back to the previous four-space stop (so one press clears a whole
  indent level), and both Tab and Backspace snap a bare `END` back to its
  opener's indent, matching the existing space-key behaviour.

- **Clearer "matched baseline" comparison result.** A comparison row that agrees
  with its baseline on every field now reads **Comparison matched baseline** in
  the `Result` column instead of a terse `OK`, so an exported CSV/JSON is
  self-explanatory.

### Fixed

- **Prompt dialogs no longer clip their title.** A single-line prompt used a
  fixed-width box, so a long title — most visibly the workspace **New report
  (path relative to workspace)** prompt, and longer still in French/Danish —
  ran past the panel border and lost its trailing `Esc cancel` (and the box's
  own right edge). The box now widens to fit its title (clamped to the terminal
  width).

- **The request wizard's combined "All" view no longer hides populated
  sections when several are stacked.** With nine sections (Headers, Cookies,
  Queries, Options, Form, Body, Asserts, Captures, Reports) the fixed layout
  could overflow the dialog and let the ratatui solver compress the tallest
  table — most visibly the Headers table, which on smaller terminals collapsed
  to *zero* visible rows (so a request loaded from a `.hurl` file appeared to
  have no headers even though they were parsed correctly). The All view now
  (1) collapses each **empty** section to a single compact `Label   (＋ Add …)`
  line — dropping its unused `Key / Value / Description` column-title row so the
  populated sections get the space — and (2) **scrolls the whole stack** (whole
  sections at a time, keeping the focused section on screen, with a scrollbar in
  the reclaimed rightmost column) whenever the naturally-sized sections are
  still collectively taller than the dialog body. The stale hint text that read
  "Alt+1-6 jump" is corrected to "Alt+1-9" to match the nine section jumps.
  Two follow-up glitches in that view are also fixed: the per-section scrollbar
  no longer extends past the last data row into the pinned "＋ Add …" line (it
  now covers only the scrollable data region), and pressing **Up** to leave a
  section now stops on the "＋ Add …" line of the populated section above it
  (instead of jumping straight into that section's last data row).

- **A failed assertion no longer hides the response.** When a request's
  `HTTP <status>` expectation or an `[Asserts]` check failed, the Response panel
  replaced the whole response with the error text. It now keeps showing the full
  response — status line, assertions, and body — with the failing check(s)
  marked with a cross in the error colour and the `[Asserts]` badge counting the
  failures, so the response you were inspecting stays visible. A runner error
  that isn't already spelled out by a failing assertion (for example a failed
  `[Captures]`) is surfaced as one error-coloured line above the body. A
  transport failure that returned no response at all still shows the error on
  its own, as before.

- **Headers separated from the request line by a blank line are no longer
  dropped on load.** Hurl permits blank lines — and prose comment lines —
  between a request's method/URL line and its header block, and between header
  rows (likewise for the `[QueryStringParams]`, `[Cookies]`, `[Form]` and
  `[Multipart]` sections). PaperBoy's source-scan treated the first such line as
  "end of block", so a `.hurl` file whose request looked like `POST …` / blank
  line / headers loaded with every header silently gone. The scan now matches
  `hurl_core`: it skips leading and interior blank/comment lines within a block,
  bounding each block by the next structural anchor (the body, the following
  section, or the response's `HTTP` line) so trailing and fully commented-out
  (disabled) rows are still recovered. When a request has no such anchor below
  its headers (no body, section or response), the scan stops at the blank line
  separating it from the next request, so one entry can never absorb the
  following entry's title/banner as a stray header.

- **Prose comments in `.hurl` files are no longer silently discarded on load.**
  Free-standing comment lines (banners, section notes, anything that isn't a
  request title, a disabled `# key: value` row, or the `# [Reports]` block) used
  to vanish the first time PaperBoy parsed a collection, so saving the file back
  or opening it in the raw editor lost them. They now round-trip: each comment
  is anchored to the nearest structural block (the header block, body,
  `[Cookies]`/`[Query]`/`[Form]` section, the response, `[Asserts]`,
  `[Captures]`, a file-leading banner, or the end of the entry) and re-emitted
  in that position. This matters for the `[Reports]` feature, which works by
  injecting comments into the `.hurl` file, and for the raw editor, which now
  shows the comments it did before.

- **Request `[Options]`, expected response headers/body, and the response HTTP
  version are no longer silently dropped on load.** `hurl_core` parses all four,
  but PaperBoy's request model discarded them, so saving a collection back (or
  opening a request in the raw editor) erased any `[Options]` section, expected
  response header rows, expected response body, and a specific `HTTP/x.y`
  version (it was normalised away to a bare `HTTP`). They now round-trip through
  the model and serializer unchanged — including disabled (`#`-prefixed)
  `[Options]` rows — and the execution-affecting `[Options]` and the real
  response header/body assertions are carried into the run instead of being lost.

- **Copying no longer makes the clipboard helper flicker in the app bar.** On
  Wayland/X11 the background `wl-copy`/`xclip` helper PaperBoy forks to own the
  selection is now placed in its own session (via `setsid()` in the child before
  `exec`), so the desktop environment no longer briefly lists it as a running
  application — the Ubuntu app bar no longer expands and contracts on every copy.
  (Requires `tui-panel-select` 0.1.5.)

- **"Save Request" no longer crashes when a report tab is active.** The File ▸
  Save ▸ Request action indexed the active tab straight into the collections
  list, but a report tab's index points past it — so invoking it while a report
  was focused panicked with an out-of-bounds error. It's now a guarded no-op
  (a "no request" status) in that context. (A future change will hide the option
  entirely when it doesn't apply.)

- **"Save Environment As" remembers where environments live.** A never-saved
  environment's "Save As" prompt used to offer only a bare `name.vars` filename,
  dropping the file in the process working directory; it now seeds the prompt
  inside the last folder an environment was loaded from or saved to. Saving an
  environment also records its folder for next time.

- **A cancelled report run can be restarted immediately.** Pressing `r` while a
  report was running cancelled it, but the run stayed marked "running" until the
  background worker finished winding down (which, mid-`PARALLEL` batch, could
  take a while), so the next `r` was read as *another* cancel instead of a fresh
  start. Cancelling now retires the run at once — the running marker clears, the
  partial grid rolls back to whatever was showing before, and the very next `r`
  starts a new run. The detached worker keeps winding down in the background;
  its late results are ignored. (In-flight requests already dispatched still
  finish — aborting a request mid-flight isn't possible — but no *new* requests
  fire once cancelled.)

- **A workspace report now reopens focused on its pinned tree.** Reopening the
  app restored a workspace *collection* with focus on its file tree but a
  workspace *report* with focus on the editor — an inconsistency for the same
  workspace. A workspace report now resumes on its pinned tree, matching a
  collection (a standalone report, which has no tree, still resumes on the
  editor).

- **Blank lines in a report source no longer break selection and highlighting.**
  The read-only source view dropped blank separator lines from what it drew
  while still counting them in its selection/scroll geometry, so a mouse
  selection or the highlighted parse-error line landed one row off for every
  blank above it. The source panel now renders one row per line (blanks
  included), keeping the view exactly aligned with selection and highlighting.

- **The report body border now dims when the workspace tree has focus.** In a
  workspace report the source/nodes/results panel's border stayed lit even when
  focus was on the pinned file tree, so it was ambiguous which pane was active.
  The body border now lights only when it (or its editor) actually holds focus.

- **Ctrl+Backspace no longer types a literal `h` in the raw-mode and value
  prompts.** The raw request / raw-JSON editor and the environment-value / save
  prompts have their own key handling that lacked a word-delete binding, so on
  terminals without the keyboard-enhancement protocol — where Ctrl+Backspace
  arrives as Ctrl+H — the keystroke fell through to plain typing and inserted an
  `h`. These prompts now delete the previous word on Ctrl+Backspace (and its
  Ctrl+H alias), matching every other editor in the app.

- **The runner "Request Error" is now copyable and selectable.** The error shown
  when a request fails to send (e.g. an unresolved `{{VAR}}`) lives in a
  separate channel from the top status line, so **Ctrl+Y** did not grab it and
  the Response panel drew it as non-selectable text. Ctrl+Y now also copies the
  runner error, and the Response panel renders it through the selectable body
  panel so it can be mouse-selected and `y`-copied like any response.

- **A `columns:` directive or `#` comment containing accented/non-ASCII text no
  longer crashes.** Two report code paths sliced UTF-8 text at a fixed byte
  offset while scanning for the ` AS ` keyword (in a `# columns:` header) or a
  directive key (when BIND or the column picker rewrites the header). A
  multi-byte character (e.g. `naïve`, `café`, `año`) landing on that offset
  panicked — crashing the whole TUI on BIND/column-apply, or the run on a
  non-ASCII column header. Both now compare bytes on char boundaries.

- **Report names, aliases and computed-column headers containing spaces or
  punctuation now survive a save/reload.** The serializer only quoted names
  that contained whitespace, and never quoted an `AS <alias>` / computed
  `AS <header>` at all. A name with a space (`AS "Overall Result"`) or a
  bareword terminator (`REQUEST "a,b"`, `"get(id)"`) was written unquoted and
  then failed to re-parse, silently corrupting the report on the next load.
  Such names are now re-quoted whenever they aren't a valid bare token.

- **The report editor now auto-indents `PARALLEL` loops and `REPORT … WITH`
  blocks.** Pressing Enter after a block-opening line indents the body one
  level, and typing `END` snaps back to the opener's indent. Previously only a
  plain `FOR` line was recognised, so `PARALLEL FOR` / `PARALLEL(n) FOR` loops
  and `REPORT REQUEST … WITH … END` blocks were left un-indented and their
  `END` never dedented. Block recognition now runs the PaperTrail grammar's own
  parser, so it stays in step with the language.

- **Outer-scope report columns now fill in live, not only at the end.** A
  `REPORT REQUEST` placed *outside* a loop (e.g. a top-level `REPORT REQUEST
  "Get token"`) produces a column that applies to every row. Previously that
  column stayed blank in the live results grid for the whole run and only
  populated when the run finished — on a 500–1000 document run it looked like
  the column never worked. Its value is now broadcast onto each row *as the row
  streams in*, so the grid is correct throughout the run (the final export was
  always correct).

- **A file that disappears mid-run now names itself in the error.** When a
  request loads a local file during a report run (a Base64 file body, or a
  `[Form]`/`[Multipart]` file) and that file has been deleted since the run's
  file list was built, the run still emits the row with a non-fatal error (as
  before) — but the message now includes the missing file's path (e.g.
  `Base64 file error: /photos/gone.png: No such file or directory`) instead of a
  bare "No such file", so it's obvious *which* file vanished.
- **Ctrl+Backspace (word-delete) now works on terminals without the
  keyboard-enhancement protocol.** Such terminals report Ctrl+Backspace as a
  bare **Ctrl+H**, which previously did nothing (or inserted a stray character)
  in the report editor and other text fields; it is now accepted as an alias for
  Ctrl+Backspace, so word-delete works everywhere regardless of terminal
  support.
- **Compressed response bodies are now decoded before display.** When a request
  sends its own `Accept-Encoding` header (e.g. `gzip, deflate, br`), the server
  compresses the response and libcurl doesn't auto-decode it, so PaperBoy was
  showing the raw compressed bytes as garbled text in both the CLI runner and
  the TUI response panel. The body is now decompressed by its `Content-Encoding`
  before it's shown (and pretty-printed if it's JSON); `[Captures]`/`[Asserts]`
  were unaffected as Hurl already decoded internally for those.

- **A symlink loop under a `FILES … MATCH "**/…"` producer no longer crashes the
  run.** The recursive file walk followed directory symlinks, so a link that
  pointed back at an ancestor (a cycle) recursed forever and overflowed the
  stack, aborting the process. The walk now recurses only into real
  subdirectories and skips directory symlinks, so a cyclic tree terminates
  cleanly while ordinary files (and file symlinks) are still listed.

- **CSV report exports are hardened against spreadsheet formula injection.**
  Report cells carry arbitrary HTTP response text, so a value beginning with a
  spreadsheet formula trigger (`=`, `+`, `@`, tab or CR) could execute as a
  formula when the exported `.csv` was opened in Excel or Google Sheets. Such a
  field is now prefixed with an apostrophe (the "treat as text" marker) so it is
  shown literally; a leading `-` is left alone so negative numbers and the
  no-match marker keep their value. JSON/HTML/xlsx exports were never affected.

- **Closing a report tab while its run is still streaming no longer leaves it
  stuck.** A tab closed mid-run was stashed (for reopen with `u`) with its live
  progress state intact, but the background poller can only reach *open* tabs —
  so reopening it showed a permanently greyed, half-filled "running" grid that
  never completed. Closing a running tab now cancels its worker, retires the
  run, and restores the grid that was showing before the run started.

- **A `# columns:` directive that names two columns the same is now rejected.**
  Two columns resolving to the same header (e.g. `columns: FILE AS X, p.status
  AS X`) collided in JSON output — where rows are keyed by header, so the second
  column silently overwrote the first and a column vanished. Such a directive is
  now flagged as an error while you edit (and blocks the run), so every export
  format stays faithful; give each column a distinct `AS <name>`.


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
