# PaperBoy

PaperBoy is a Rust-native alternative to Postman. Collections and environments
are plain, git-friendly text files — [Hurl](https://hurl.dev) `.hurl` files
for requests, `.vars` files for environments — so everything can be committed,
diffed, and shared through a normal git repository. There is no hosted
service and nothing runs outside your machine.

It ships as a single binary with three front-ends:

- **Terminal UI** (default) — a full interactive client for building,
  editing, running and organizing requests.
- **Graphical UI** (`-g`/`--gui`) — a native desktop client (eframe/egui)
  with the same features and layout as the terminal UI, using mouse gestures
  (drag to resize panels, click to switch tabs) instead of shortcuts. Opt-in at
  build time via the `gui` Cargo feature, since it more than doubles the
  dependency tree.
- **Headless CLI** (`-c`/`--collection`) — runs a collection end-to-end and
  prints the results, for scripts and CI.

All three drive the same shared core, so collections, environments, themes and
git-remote workflows behave identically across them.

## Contents

- [Installing & running](#installing--running)
- [Core concepts](#core-concepts)
- [Features](#features)
- [Graphical UI](#graphical-ui)
- [Working with collections & environments](#working-with-collections--environments)
- [Loading & saving via git](#loading--saving-via-git)
- [Secrets & environment variables](#secrets--environment-variables)
- [Keyboard shortcuts](#keyboard-shortcuts)
- [Headless CLI mode](#headless-cli-mode)
- [Changelog](CHANGELOG.md)

## Installing & running

```sh
cargo install paperboy                 # terminal UI + headless runner
cargo install paperboy --features gui  # …and the graphical UI
```

The graphical UI is behind the `gui` feature because it pulls in
eframe/winit/wgpu — roughly twice as many crates as everything else combined —
and most of that build time is wasted on anyone who only wants PaperBoy in a
terminal. A build without it still reads and writes the same state file, so
switching between the two loses nothing. Running `--gui` on a build that lacks
it prints the command to install one that has it.

From a checkout:

```sh
cargo run                           # launch the terminal UI
cargo run --features gui -- --gui   # launch the graphical UI
cargo build --release
```

## Core concepts

- **Collection** — a `.hurl` file: an ordered list of requests, each with a
  method, URL, headers, cookies, body/form fields, and optional
  `[Captures]`/`[Asserts]` sections. Postman collection exports (`.json`)
  can also be opened directly — they are imported and converted to Hurl
  automatically.
- **Environment** — a `.vars` file: `KEY=value` lines supplying the
  `{{ VAR }}` values a collection references. A value can be a literal, or a
  reference to a secret provider (see [Secrets](#secrets--environment-variables)).
- **Scratch Space** — the first, always-present tab. It behaves like any
  other collection (add/run/edit requests) but isn't tied to a file until you
  explicitly save it — a place to try things out without committing to a
  collection first.

## Features

- Multiple collection tabs, each with its own environment.
- Add, edit, reorder, run, and delete requests within a collection.
- **Edit Request wizard**: pressing `Enter` on a request opens the same
  form used for "New Request", prefilled with its current method, URL,
  headers, cookies, body, form fields, `[Captures]`, and `[Asserts]` — no
  manual JSON or Hurl syntax required. `Shift+R` opens **Raw Mode** instead,
  a direct editor for the request's real Hurl text (for anything the form
  doesn't expose, such as query params, Basic Auth, or the expected
  status); saving reparses the text and applies it back to the request,
  keeping invalid text open for correction rather than discarding it.
- **`[Form]`/`[Multipart]` and `[Cookies]` sections** in the request wizard:
  add form fields (enabled checkbox, Key, a Kind dropdown defaulting to
  `Text` — press `Enter` to open it and pick `File` or `Base64 File` instead,
  Value, a separate Content-Type column, a Base64 Prefix column, and a
  free-text Description column — Kind comes before Value so filling a row
  naturally picks the type before typing the value) and cookies (checkbox,
  Key, Value), using the same table and tabbing conventions as `[Headers]`.
  Saving picks the correct Hurl section automatically — all-Text fields become
  `[Form]`, and any File field promotes the section to `[Multipart]`. File
  fields are colored to show whether the referenced path exists and is
  readable — relative paths resolve against the collection's own directory,
  matching where Hurl actually looks for them when sending the request. With
  focus on a `File`- or `Base64 File`-kind Value cell, `Ctrl+F` (or `Enter`)
  opens a file picker to choose the path instead of typing it (without
  stealing focus to the Content-Type column afterwards); the Content-Type cell
  is a dropdown offering "Auto" plus the fixed MIME types from
  [hurl.dev](https://hurl.dev/docs/request.html) (still editable as free
  text for anything else), and is auto-set from the picked file's
  extension. An empty/auto-detected Content-Type cell shows a dimmed
  "Auto" placeholder rather than looking blank. A **`Base64 File`** field is
  sent as plain text: its picked file is base64-encoded (unwrapped) at send
  time and the field value becomes `<Base64 Prefix><base64>`, so a prefix like
  `data:image/png;base64,` produces a ready-to-use data URI. The Base64 Prefix
  column only affects `Base64 File` rows. Editing a request into an
  impossible Body + Form/Multipart combination shows a clear status-bar
  error instead of sending it.
- **Headers, Cookies, Form, Asserts, and Captures all start empty** — just
  a "+ Add …" button for each, with no default blank row to delete for
  requests that don't need one (a lingering blank row used to intercept
  the `↓` key meant for scrolling past that section). To remove a row you
  no longer need, focus any cell in it and press `Ctrl+D`; the wizard's
  hint bar shows `^D delete row` as a reminder whenever focus is on a
  deletable row.
- **Enabled checkbox is reachable with `←` from a row's Key cell** — it's
  the leftmost visual column of every Headers/Cookies/Form row, so arrow
  navigation now matches that layout instead of requiring a trip through
  every other column to reach it. A brand new row still starts focus on
  Key (not the checkbox), matching how you naturally fill a row in. `Ctrl+E`
  toggles the focused row's enabled state and jumps focus to the checkbox,
  from anywhere in that row.
- **Dropdowns only auto-open when their cell is empty.** The Key suggestion
  and Content-Type dropdowns pop open automatically the first time you land
  on an empty cell, but once a cell has a value, navigating onto it again
  leaves the dropdown closed — press `Enter` to bring it back up without
  disturbing the current value. The Form Kind dropdown follows the same
  "populated cell" rule: since it defaults to `Text` on a fresh row, it
  never auto-opens — `Enter` always reveals it to switch to `File`. This
  keeps `↑`/`↓` navigation through a populated section from getting stuck
  reopening a dropdown on every row.
- **Scrollable, navigable tables**: when a Headers/Cookies/Form/Asserts/
  Captures section has more rows than fit on screen, it grows to show up
  to 5 rows before a scrollbar appears on the **left** edge, right next to
  the data; `↑`/`↓` scroll the focused row into view, and `Ctrl+↑`/`Ctrl+↓`
  jump straight to the first row of the previous/next section. The "+ Add
  …" row always stays reachable even when the list is scrolled, and a long
  `Body` also grows a left-hand scrollbar once it overflows.
- **Per-section wizard tabs**: the request wizard has a tab bar (`All │
  Headers │ Cookies │ Form │ Body │ Asserts │ Captures`), switched with
  `[`/`]` or `PageUp`/`PageDown` and reordered with `Ctrl+Shift+←`/`→`
  (just like collection tabs). `[`/`]` only cycle tabs when focus is on a
  non-text field (Method, Target, or a "+ Add …" row), so they can still be
  typed into URLs, bodies, and other text cells; `PageUp`/`PageDown` always
  switch tabs. Picking a section tab gives that section almost the
  whole dialog to itself — handy for editing long Headers or Form-field
  lists — while `All` keeps the combined, everything-at-once layout. Every
  tab is just a different view of the same request, so edits made on one
  tab show up immediately on every other. Switching to a section tab jumps
  focus straight to its first entry (Body's tab drops you right into
  editing), and `Tab`/`Shift+Tab`/`↑`/`↓`/`Enter` are confined to the
  active section so navigation never jumps to a section you can't see.
- **Request chaining**: capture a value from a response (Hurl `[Captures]`)
  and reference it as `{{ name }}` in any later request in the same
  collection.
- **Basic Auth** support (Hurl's native `[BasicAuth]` section).
- Colored, substituted request preview: `{{ VAR }}` placeholders in the
  Request JSON panel are shown replaced with their real value, colored by
  status (green = loaded, cyan = literal, orange = still loading, red =
  missing) — while the editor itself always keeps the original `{{ VAR }}`
  text, so you always know exactly what will be sent versus what's stored.
  Secrets substituted into the preview are masked as 8 dots.
- Editing an environment entry to look like a secret reference (e.g. typing
  `{{ op://vault/item/field }}` or `{{ ssm:/path }}` into a value) triggers an
  automatic load attempt, the same as if it had been in the file originally.
- Load/save collections and environments from local files, or straight from a
  git remote (see below) — no local clone required. A collection or
  environment loaded from git shows a small ⎇ icon in its tab title /
  Environment heading, and can be pushed back to a new branch or tag with
  **Save → Collection → To Git…**.
- A **recently-used git URLs** dropdown in the "from Git" wizard, most
  recent first.
- Resizable panes: the left column width and the response pane height are
  both adjustable and persisted across restarts.
- Tab management: close (`Ctrl+W`/`x`), reopen the last-closed tab (`u`),
  cycle tabs (`[`/`]` or `PageUp`/`PageDown`), and reorder tabs
  (`Ctrl+Shift+←`/`→`).
- **Deleted requests can be restored too.** Pressing `x` on the Requests
  list deletes the selected request; `u` on that same pane brings back the
  most recently deleted one (last-deleted-first, one collection's stack
  per collection) — the exact parallel of reopening a closed tab, just
  scoped to individual requests instead of whole collections. Closing a tab
  or deleting a request also shows a status-bar message naming the `u` undo
  key, so an accidental deletion is easy to reverse.
- **Move or copy requests between workspace collections.** In a workspace
  tab, `m` moves and `c` copies the selected request into another collection
  file — a picker chooses the destination and the change is written straight
  to disk (no separate Save).
- The app remembers your window layout, active tab/request, language, and
  recently-used git URLs across restarts.
- English, French and Danish UI languages (Options → Language).
- **Custom themes.** Settings → Theme lets you pick a per-language preset or
  build and save your own colour scheme (see [Themes](#themes)).
- **Nested folders.** A request's name can encode a folder path with `/`
  separators (e.g. `Auth/Tokens/Refresh`); the Requests list browses these
  as folders — `Enter` on a folder row descends into it, `Enter` on the
  "‹ .." row (or `Backspace`) goes back up — instead of an ever-more-indented
  tree, and the panel title shows a breadcrumb of the folder you're in.
  Importing a Postman collection preserves its nested folder structure this
  way automatically; native Hurl collections get the same behavior for free
  simply by naming requests with `/` in them. Creating a new request while
  browsing a folder prefills the Name field with that folder's path.

## Graphical UI

Run `paperboy -g` (or `cargo run --features gui -- --gui`) to open the native
desktop front-end instead of the terminal UI. It needs a build made with the
`gui` feature (see [Installing & running](#installing--running)). It is built
on
[eframe/egui](https://github.com/emilk/egui) and drives the exact same core as
the terminal UI, so it is feature-for-feature equivalent — collection tabs, the
folder tree, the request editor (Params, Headers, Body, Auth, Cookies, Options,
Asserts, Captures, and a resolved-request Code view), the response viewer
(Body / Headers / Asserts), the Global Environments panel (activate, link, edit
variables, resolve `env:` / `ssm:` / `op://` secrets), report tabs, the theme
editor, and loading/saving collections and environments to a git remote with no
local clone.

What changes is how you interact with it:

- **Tab** cycles the focused panel in the same order as the terminal UI
  (Tabs → List → Main → GlobalEnv → Response), and **Shift+Tab** cycles
  backwards — so the muscle memory carries over.
- **Panels are resized by dragging** their splitters, rather than with keyboard
  shortcuts.
- **Selection, scrolling and the clipboard** use the platform's native
  handling; **collection tabs switch by clicking**; folders collapse/expand by
  clicking their header.
- **The File menu is grouped by verb** (New / Open / Save / Import-Export)
  rather than one flat list. There is no separate "Import Postman": **Open ▸
  Collection** takes a Hurl `.hurl` file or a Postman `.json` export and works
  out which it is. Every file dialog reopens in the folder you last used it in.
- **Workspaces can be grown from inside the app.** The workspace panel's **New**
  menu adds a collection, report or environment at the top of the tree, and
  right-clicking any row offers the same three inside *that* folder. Files (and
  folders) can be **dragged onto another folder to move them**, or dropped on
  the empty space below the tree to move them back out to the root; whatever the
  app was holding — the loaded collection, the open report, which folders were
  expanded — follows the file. Nothing can be created or moved outside the
  workspace root, and nothing is ever silently overwritten.
- **Workspaces stay portable.** A report binds to its collection by a path
  *relative to the report* — including `../apis/billing.hurl` for a collection
  in a sibling folder — so a workspace still works when it is zipped up, checked
  in, or handed to someone whose folders are laid out differently. The
  `collection` dropdown lists collections **by name**, offers the ones in the
  report's own workspace first, and keeps anything from outside it behind a
  "Show collections outside this workspace" toggle, since binding to one of
  those is what makes a workspace stop travelling.
- **Git remotes load whole workspaces**, not just single files — the same
  no-local-clone loading and saving the terminal UI offers, for collections,
  environments and entire workspace trees.
- **The window remembers itself** — its size, the widths and heights of every
  panel you dragged (including the block editor's palette and diagnostics
  panel), which view was open, which report or request you were on, and which
  node a Workspace tab was showing are all restored on the next launch. A
  Workspace selection is dropped if that file has since gone, since a workspace
  is a live folder rather than a snapshot.
- **The active environment is unmissable** — it is ticked, coloured and banded
  in the Global Environments list rather than marked with a dot.
- **Every theme applies unchanged** — built-in language presets and custom
  themes are read from the same `ThemeSpec` and mapped onto egui's visual
  style, and the whole GUI is translated (English / French / Danish) through
  the same `i18n` table as the terminal UI.

### The PaperTrail block editor

Reports open in a drag-and-drop block editor (a **Blocks** view alongside the
**Source** and **Results** views). Blocks are dragged from a palette into the
flow, reordered, nested inside `FOR` loops — whole loops move as one, with
their body — and dropped on the trash bar to delete. Both the dashed outline
left behind by a picked-up block and the marker showing where it will land are
drawn as that block's own silhouette — one outline per row, at its own width
and indent — so what you see really is what will land there.

Editable directly on the blocks:

- the request a step runs, its alias, response format, and its `SHOW(…)` /
  `HIDE(…)` / `STATISTICS(…)` column lists;
- a `FOR` loop's binder, source and roles, and the maximum concurrency of a
  `PARALLEL(n)` loop;
- the report's own settings — `collection`, `output`, `environment`, `root`,
  `baseline` and `columns` — stacked in a boxed panel above `BEGIN`. These
  apply to the report as a whole rather than running as a step, so they are
  deliberately not draggable blocks; the box is what says so, and it keeps a
  fixed width so the view doesn't twitch as you edit. `collection` and `output`
  are always shown (a missing `collection` is flagged as an error; `output`
  simply defaults to CSV); the rest are appended from the **Add a report
  setting** button below them, and any setting that is set can be cleared with
  its `×`. Settings with a
  closed set of values are dropdowns — including `output`, which names a
  *format* (`csv`, `json`, `html`, `xlsx`), not a filename: the file is named
  after the report, and only the command-line runner's `-o` takes a path.

Every block and setting has hover help explaining what it is and what its
editable parts do. The **Source** view is syntax-highlighted with exactly the
same colours as the terminal UI, and the line the parser rejected is
underlined.

### Desktop icon on Linux

Wayland has no per-window icon protocol, so shells such as GNOME resolve the
dock/taskbar icon by matching the window's app id (`paperboy`) against an
installed `.desktop` file. The first GUI launch therefore writes, if they are
not already there:

- `$XDG_DATA_HOME/paperboy/paperboy_logo.png` — a copy of the bundled logo, and
- `$XDG_DATA_HOME/applications/paperboy.desktop` — a launcher entry pointing at
  it, with `StartupWMClass=paperboy` so X11 sessions match too.

Both are left alone if they already exist, so you can customise them. Because
the entry only appears *during* the first launch, the shell may not pick it up
until it next rescans — log out and back in (or restart the shell) if the
generic icon is still showing. Deleting the two files and relaunching
regenerates them, which is also how you refresh the `Exec=` line after moving
the binary.

## Working with collections & environments

Open the **File menu** with `f`, then pick **(L)oad** or **(S)ave**. Each
opens a short list of *what* you want to load or save (Request, Collection,
Environment, Workspace, Response). Choosing a kind that can come from — or go
to — more than one place opens a second step to pick the source or
destination, so git and local options no longer clutter one long list:

| Kind | Load sources | Save destinations |
|---|---|---|
| Request | Local file only (goes straight to a path prompt) | Local file only |
| Collection | **Local file…** / **From Git…** | **Save** (original file) / **Save As…** / **To Git…** |
| Environment | **Local file…** / **From Git…** | **Save** (original file) / **Save As…** |
| Workspace | **Local file…** / **From Git…** | **Save As…** (copy the folder) / **To Git…** |
| Response | — | Local file only |

Every step is keyboard-driven: each item has a bracketed mnemonic (e.g.
**(L)ocal file…**, **From (G)it…**, **(S)ave**), and pressing that letter both
selects *and* activates the item — no extra Enter needed. `→` (or `Enter`)
steps into the next level and `←` (or `Esc`) steps back, with the kind you
picked still highlighted.

- **To Git…** is only meaningful for a collection/workspace that was itself
  loaded from git; on anything else it just reports that there's no remembered
  git origin.

Notes on saving:

- **Save** (Collection / Environment) writes back to the file the tab
  was originally loaded from. If there are no new or modified entries, saving
  is a no-op and no confirmation is shown (there's nothing to warn about).
  If there *are* unsaved changes, you'll get a confirmation naming how many
  new/modified requests (for a collection) or variables (for an environment)
  will be written — the two counts are tracked and reported independently, so
  saving a collection only ever mentions collection changes, and saving an
  environment only ever mentions environment changes.
- **Save As…** always prompts for a filename. If the chosen path already
  exists you'll be asked to confirm the overwrite (Yes/No) before anything is
  written.
- The Scratch Space (tab 0) can be saved like any other collection — pick
  Collection → **Save As…** to give it a file for the first time.
- A modified request shows a pencil icon next to it in the Requests list
  until it's saved.

### Settings & preferences

Open the **Settings** menu with `s`. It has a **Theme** editor (see below) and
**Preferences** for a few toggles that persist across restarts:

- **Confirm on exit** / **Confirm on clear** — ask before quitting or before
  clearing all collections.
- **Confirm before deleting an environment** (on by default) — ask before
  deleting a Global Environment with `x`. Turn it off to delete straight away;
  either way the deletion is undoable with `u`.
- **Always save when prompted** (off by default) — whenever an action would
  otherwise pop up a **Save / Discard / Cancel** choice (e.g. switching away
  from a Workspace collection with unsaved changes), automatically pick
  **Save** without asking. Leave it off to keep being prompted each time.
- **Default Request view** — whether the Request panel opens in JSON or Hurl.

### Themes

Settings → **Theme** opens a theme editor. PaperBoy ships three presets — one
per UI language, evoking its country: **Britannia** (English), **Parisian
Purple** (French), and **Dannebrog** (Danish). By default the theme follows
the current language; pick any preset (or a custom theme) from the list to use
it. Presets are read-only — to make your own, base a new theme on one:

- `Ctrl+N` — open the **New theme** popup. Give it a name — the cursor blinks
  in the name field ready for typing — and choose an existing theme to copy its
  colours from (`Tab` switches between the name and the base list). Pressing
  `Enter` creates the theme, activates it, adds it to the list, and drops you
  into the colour rows to edit.
- Editing colours — with a custom theme selected, step into the colour rows
  with `→` (or `Tab`). The first row is the theme's **name** — edit it to rename
  the theme, then press `Enter` (or move off the row) to submit. Below it are
  the colours. Move between rows with `↑`/`↓`. Press
  `Enter` on a colour to open the **colour picker**: adjust the highlighted
  R/G/B channel with `←`/`→` (±1) or `Ctrl`+`←`/`→` / `PageUp`/`PageDown` (±16),
  or type a `0`–`255` value, and switch channels with `↑`/`↓`. The whole UI
  previews live; `Enter` applies (and **auto-saves**), `Esc` cancels. `←` steps
  back from the rows to the list.
- `Ctrl+D` — delete the selected custom theme (presets can't be deleted); focus
  moves to the theme just above the deleted one.
- `Esc` — close the editor.

Once you've chosen a theme by hand it stays put; changing language only
switches the theme while you're still on **Automatic**.

## Loading & saving via git

This is the one workflow that isn't just "open a file", so here's the full
step-by-step. **Loading** fetches a single file out of a remote repository
without cloning it: it lists the repo's branches/tags, fetches just enough
git history to read the file tree for the ref you pick, and checks out only
the one file you select. No other file in the repo ever touches your disk.

1. Press `f` to open the File menu, choose **(L)oad**, then pick the kind
   (**Collection** or **Environment**) and choose **From (G)it…**.
2. **Connect** — type the repository URL (`https://…` or `git@…`). If the
   repo needs an access token, Tab to the token field and enter it (only used
   for this fetch — the wizard supports GitHub-style
   `https://x-access-token:<token>@host/...` URLs and injects the token for
   you). Press `↓` on the URL field to open a dropdown of your recently-used
   URLs and pick one instead of retyping it; press `Enter` to connect.
3. **Pick a ref** — choose a branch or tag from the list (type to filter).
4. **Pick a file** — choose the `.hurl`/`.json` (or `.vars`) file from the
   ref's file tree (type to filter).
5. The file is checked out and opened as a new tab. Its URL is remembered at
   the top of the "recently used" list for next time, and a small ⎇ icon is
   shown in the tab title (or, for an environment, in the Environment panel
   heading) to mark it as loaded from git.
6. When loading a **collection**, you're then asked whether to also load its
   environment — answering Yes reuses the same file listing already fetched
   in step 4 (no second network round-trip) and lets you pick the `.vars`
   file to pair with it. Answering No (or loading an environment on its own
   via Environment → **From Git…**) leaves it as-is.

Loading a directory of files, or importing a whole repo, is not supported
this way — for that, see **Loading a Workspace from git** below.

### Loading a Workspace from git

A **Workspace** is a folder containing many `.hurl`/`.json` collection
files, browsed and chosen from through a single tab (press `w` on a
Workspace tab to reopen its file-tree picker at any time). Workspaces can
also be loaded straight from a remote repository, instead of only from a
local folder:

1. Press `f` to open the File menu, choose **(L)oad**, then pick
   **(W)orkspace** and choose **From (G)it…**.
2. **Connect** and **pick a ref**, exactly as for a single collection/
   environment above.
3. **Choose which files to download** — a repo can hold plenty of large,
   unrelated files that have nothing to do with your collections, so before
   anything is fetched you pick one of: `.hurl` and `.json` files only
   (the default), `.hurl` files only, `.json` files only, or every file.
   Only files matching this choice are ever downloaded — anything else in
   the repo never touches your disk.
4. The matching files are checked out into a throwaway local folder. Before
   the new tab is created you're asked whether to **keep it temporarily**
   (the folder above) or **save it to a permanent location now** — picking
   the latter opens a folder browser and a name prompt, then copies the
   files there immediately and binds the new tab to that permanent folder
   instead. Either way, the file-tree picker then opens immediately so you
   can choose which collection to view.

**⚠️ A temporarily-kept Workspace's files are not cleaned up automatically.**
Unlike the single-file collection/environment load above, if you choose to
keep it temporary its files stay on disk in a temp folder for as long as its
tab exists — including across closing and reopening the tab within the same
session (`u` / `Ctrl+Shift+T` undoes a close, so the folder can't be safely
deleted the moment the tab closes). Restarting PaperBoy does not delete it
either. If you load the same Workspace from git repeatedly, or load many
different ones and keep them all temporary, these folders will accumulate
under your system's temp directory and you may want to clear them out
yourself from time to time — or avoid the problem entirely by choosing
**save it to a permanent location** when asked, or later via **File → Save
→ (W)orkspace → Save (A)s…** (available for any Workspace tab, local or from
git; it copies the whole folder to a destination and name you choose, and
stops tracking it as a temporary git download). Saving a Workspace-loaded
collection back to git is also not currently supported — use **Save →
Collection → Save As…** into your own local clone instead.

### Saving to git

A collection that was itself loaded from git (shown by the ⎇ icon in its tab
title) can be saved back with **Save → Collection → To (G)it…**, which pushes
a new commit straight to the remote — no local clone, staging area, or manual
`git` commands needed. Like loading, this never performs a full checkout: only
the file(s) you're actually writing are ever fetched or touched, no matter how
large the repository is.

1. **Connect** — the repository URL is pre-filled with the one this
   collection was loaded from (editable, e.g. to push to a fork), plus an
   optional access token field.
2. **Choose what to save** — the collection's in-repo path (defaults to
   where it was loaded from), and, if an environment is attached, whether to
   also save it in the same commit (and its in-repo path).
3. **Choose a target** — Tab switches between **Branch** and **Tag**:
   - A **branch** name defaults to the one you loaded from (so pressing
     Enter here just appends a new commit to it); typing a different name
     targets another existing branch (also a plain append) or creates a
     brand-new one. `↓` opens a dropdown of the remote's existing branches
     to pick from instead of typing. No merge or rebase is ever attempted —
     if the branch has moved in a way that makes the push a non-fast-forward,
     it's simply reported as an error.
   - A **tag** name must be brand new. PaperBoy re-`fetch`es the remote
     immediately before checking, so even a tag another user created seconds
     ago is caught — **an existing tag name is always rejected and never
     overwritten**, with no way to force it.
4. **Commit message** — auto-generated (`Update <name> via PaperBoy`) and
   editable before pushing. The commit author is your system git identity
   (`git config user.name`/`user.email`), or a generic `PaperBoy
   <paperboy@localhost>` identity if that isn't configured.
5. Once pushed, a branch-target save updates the collection's (and, if
   included, the environment's) remembered git origin to that branch and
   clears their "new"/"modified" markers, exactly like a local Save. A
   tag-target save clears the markers too but leaves the remembered branch
   origin untouched, so the *next* "Save to Git" still defaults back to your
   working branch.

For a collection that *wasn't* loaded from git (or when you'd rather manage
git yourself), the local workflow still works exactly as before: use **Save →
Collection → Save As…** / **Save → Environment → Save As…** and choose a path
inside your own local clone of the repository, then commit and push with your
normal git tooling outside the app.

## Secrets & environment variables

An environment `.vars` file has one `KEY=value` entry per line. The value can
be:

| Form | Example | Meaning |
|---|---|---|
| Literal | `USERNAME=demo` | Used as-is |
| Process env var | `BASE_URL={{ env:DEMO_BASE_URL }}` | Read from the process environment |
| 1Password (`op` CLI) | `API_TOKEN={{ op://Vault/Item/field }}` | Resolved via the local `op` CLI |
| AWS SSM parameter | `DB_PASSWORD={{ ssm:/path/to/param }}` | Resolved via local AWS auth |

Provider-backed values (`op://…`, `ssm:…`) are resolved in the background
when the environment loads; the Environment panel shows their status while
this happens. Every 1Password reference across every open collection's
environment is resolved with a single `op inject` call (one authorization
prompt), instead of one prompt per collection. If an entry fails to load
(missing tool, declined auth, unreachable parameter store), select it in
the Environment panel and press `r` to retry just that one entry — works
for process env vars, 1Password, and SSM references alike. When editing
such an entry, a **"still secret?"** checkbox lets you keep it masked or,
if you're confident the new value isn't sensitive, save it as a plain
value in the state. Rewriting a value to look like a provider reference
(typing `{{ op://... }}` or `{{ ssm:... }}` into any entry) automatically
triggers a load attempt for it, same as on initial file load.

## Keyboard shortcuts

Open the in-app help overlay any time with `?` or `F1` for the full,
up-to-date list. Highlights:

| Key | Action |
|---|---|
| `Tab` / `Shift+Tab` | Move focus between panes |
| `↑`/`↓`, `j`/`k` | Move selection |
| `←`/`→`, `h`/`l` | Switch tabs / scroll list-panel text horizontally (in the File menu: `←`/`→` leave / enter a submenu) |
| `Enter` | Open the Edit Request wizard for the selected request (or, on a folder row in the Requests list, descend into that folder; on the "‹ .." row, go back up) |
| `→` / `l` (Workspace list) | Open the highlighted workspace folder or collection (same as `Enter`); `Enter` on an already-open collection collapses it |
| `w` (Workspace tab) | Pop up the full workspace file-tree picker to jump to any collection |
| `Shift+N` (Workspace tab) | New collection / report / environment in the highlighted folder — the extension you type (`.hurl`, `.trail`, `.vars`) chooses which |
| `Shift+M` (Workspace tab) | Move the highlighted workspace file or folder — `Space` confirms the destination folder |
| `Backspace` (Requests list) | Go up a folder, from anywhere in the current folder |
| `Shift+R` | Edit the selected request in Raw Mode (Hurl text) |
| `F5`, `Ctrl+Enter` | Run the current request |
| `Alt+F5` | Run every request in the collection in order, in one Hurl execution (like the CLI's batch mode) — pass/fail markers appear in the Requests list and a Passed/Failed/Total summary on the status bar |
| `n` | New request (List/Response/Tabs panes) — or add environment variable (Env pane) |
| `b` | Set the base URL |
| `f` / `s` | File menu / Settings menu |
| `[` / `]`, `Ctrl+←`/`→` | Previous / next tab |
| `PageUp`/`PageDown` | Previous / next tab (same as `[`/`]`) |
| `F2`, `Enter` (on tab bar) | Rename the active collection tab |
| `x` | Delete request / close collection tab |
| `r` (Env pane) | Reload the selected environment entry if it failed to load |
| `x` / `u` (Env pane) | Delete / reopen the selected Global Environment (deletion is undoable and can skip its confirmation via Preferences) |
| `Ctrl+W` / `u` | Close / reopen a collection tab |
| `u` (Requests list) | Restore the most recently deleted request in the active collection |
| `m` / `c` (Requests list, workspace) | Move / copy the selected request to another collection file in the workspace |
| `Ctrl+Shift+←`/`→` | Reorder the active tab |
| `+` / `-` | Grow / shrink the response pane |
| `<` / `>` | Grow / shrink the left column |
| `Ctrl+↑`/`↓` (in wizard) | Jump to the previous/next section (Headers/Cookies/Form/Body/Asserts/Captures) |
| `Alt+1`-`6` (in wizard) | Jump directly to a section (1=Headers, 2=Cookies, 3=Form, 4=Body, 5=Asserts, 6=Captures) — `Alt`, not `Ctrl`, since most terminals can't report `Ctrl`+digit without special keyboard-protocol support |
| `[` / `]` (in wizard) | Switch the wizard's section-view tab, same as `PageUp`/`PageDown` — only when focus is on a non-text field (Method, Target, or a "+ Add …" row), so brackets can still be typed into URLs/bodies/values |
| `PageUp`/`PageDown` (in wizard) | Switch the wizard's section-view tab (`All`/Headers/Cookies/Form/Body/Asserts/Captures); switching to a section tab focuses its first entry |
| `Ctrl+Shift+←`/`→` (in wizard) | Reorder the wizard's section-view tabs |
| `Ctrl+D` (on a Header/Cookie/Form/Assert/Capture row) | Delete that row |
| `Ctrl+E` (on a Header/Cookie/Form row) | Toggle that row's enabled/disabled state and focus its checkbox |
| `←` (from a row's Key cell) | Reach the Enabled checkbox — it's the leftmost visual column, so arrowing left from Key goes straight to it |
| `Ctrl+F`, `Enter` (on a `File`- or `Base64 File`-kind Form Value cell) | Open a file picker to choose the path |
| `F2`, `Ctrl+Enter` | Save the open editor / wizard |
| `Esc` | Cancel |
| `q`, `Ctrl+C` | Quit |

## Headless CLI mode

Run a collection end-to-end from the command line, with no UI:

```sh
paperboy -c collection.hurl
paperboy -c collection.hurl -e environment.vars
```

- `-c`/`--collection` accepts a Hurl `.hurl` file or a Postman collection
  export (`.json`), imported automatically.
- `-e`/`--env` supplies a `.vars` environment file for any `{{ VAR }}`
  references in the collection (required if the collection uses
  `{{ BASE_URL }}` or similar).
- `-b`/`--batch` runs every request as a single Hurl call instead of the
  default streaming mode (see below).

By default, each request's method, URL, status, asserts, captures, and body
(truncated) are printed as soon as that request finishes, instead of
waiting for the whole collection — output is color-formatted (skipped
automatically when not writing to a terminal, or when `NO_COLOR` is set) to
make passes, failures, and sections easier to tell apart at a glance.
Streaming reuses the same `hurl` crate runner one request at a time, so
captures still chain correctly between requests — but it can't carry
Hurl's automatic cookie jar (cookies remembered from `Set-Cookie` response
headers) across requests the way running the whole collection in one call
can; a warning to this effect is printed at startup. An explicit
`[Cookies]` section on a request is unaffected either way. If a collection
relies on cookie continuity between requests, pass `--batch` to run it as
a single call like before, trading incremental output for that guarantee.

The process exits `0` if every request passed, non-zero otherwise.
