# PaperBoy

A Rust-native alternative to Postman. Collections are [Hurl](https://hurl.dev)
`.hurl` files and environments are `.vars` files (`KEY=value`), so everything is
plain text you can commit, diff and review. No hosted service, no telemetry,
nothing leaves your machine.

One binary, three front-ends over the same core:

| Front-end | How | Notes |
|---|---|---|
| Terminal UI | `paperboy` | The default. Full client. |
| Graphical UI | `paperboy -g` | eframe/egui. Behind the `gui` Cargo feature. |
| Headless runner | `paperboy -c collection.hurl` | For scripts and CI. Also runs reports (`-r`). Exits non-zero on failure. |

Collections, environments, themes and the git workflows behave identically in
all three.

- [Install](#install)
- [Concepts](#concepts)
- [Terminal UI](#terminal-ui)
- [Graphical UI](#graphical-ui)
- [Environments and secrets](#environments-and-secrets)
- [Git remotes](#git-remotes)
- [Importing from Postman](#importing-from-postman)
- [Headless runner](#headless-runner)
- [Changelog](CHANGELOG.md)

## Install

```sh
cargo install paperboy --locked                 # terminal UI + headless runner
cargo install paperboy --locked --features gui  # …and the graphical UI
```

`--locked` is recommended: it builds the dependency versions PaperBoy was
tested against rather than re-resolving to whatever is newest. (A yanked
`arrayref` release took plain `cargo install` down on 2026-08-20 while
`--locked` kept working — and it took the terminal-only build with it, since
Cargo resolves optional dependencies whether or not their feature is on.)

The `gui` feature is opt-in because eframe/winit/wgpu roughly double the
dependency tree. Both builds share the same state file, so you lose nothing by
switching. Running `--gui` without it prints the command to install one with it.

### Build prerequisites

Five things Cargo can't fetch for you:

| Platform | Command |
| --- | --- |
| macOS | `xcode-select --install` then `brew install pkg-config` |
| Debian/Ubuntu | `sudo apt install build-essential pkg-config libxml2-dev libclang-dev perl` |
| Fedora/RHEL | `sudo dnf install pkgconf-pkg-config gcc make perl libxml2-devel clang-devel` |
| Arch | `sudo pacman -S pkgconf base-devel perl libxml2 clang` |
| Alpine | `sudo apk add build-base pkgconfig perl libxml2-dev clang-dev` |
| Windows (MSVC) | `vcpkg install libxml2:x64-windows-static-md` |

**libxml2 + `pkg-config`** because `hurl`/`hurl_core` depend unconditionally on
the `libxml` crate (Hurl's XPath asserts *are* libxml2's XPath engine), and that
crate is a binding to a system libxml2 rather than a vendored copy.
**libclang** because `bindgen` generates those bindings at build time. **A C
compiler, `perl` and `make`** because PaperBoy pulls `curl` in directly with
`static-curl`/`static-ssl`, so libcurl and OpenSSL are compiled from vendored
sources — which is why there is no `libcurl-dev` row above. The `gui` feature
adds no build-time requirement; its X11/Wayland libraries are `dlopen`ed at
runtime.

On macOS the Command Line Tools cover everything except `pkg-config`, which is
the failure most people hit. If your libxml2 came from Homebrew rather than the
SDK:

```sh
export PKG_CONFIG_PATH="$(brew --prefix libxml2)/lib/pkgconfig:$PKG_CONFIG_PATH"
```

PaperBoy's `build.rs` checks for all five before the build gets going and fails
with the package-manager command your machine actually wants (it detects
Homebrew/MacPorts, apt, dnf, yum, zypper, pacman, apk). It fails rather than
warns because Cargo runs build scripts concurrently and doesn't replay their
warnings — a warning lands dozens of `Compiling …` lines above the real error.
It never installs anything, and it can't prompt: a build script has no terminal.
Checks that could be wrong (cross-compilation, target-suffixed `PKG_CONFIG_*`,
the libclang heuristic) only warn, and `PAPERBOY_SKIP_DEP_CHECK=1` disables it
entirely.

If you have a libxml2 lying around and want to skip `pkg-config` and `bindgen`,
`libxml`'s build script takes an explicit path — `--config` reaches transitive
build scripts:

```sh
cargo install paperboy --locked \
  --config 'env.LIBXML2="/opt/homebrew/opt/libxml2/lib/libxml2.dylib"'
```

Vendoring libxml2 instead isn't possible from here: the crate has no vendored
build, no `libxml2-src` exists, and it declares no `links` key, so there's no
`DEP_*` channel to reach into it with. It would have to be added upstream.

From a checkout:

```sh
cargo run                           # terminal UI
cargo run --features gui -- --gui   # graphical UI
cargo test                          # add --features gui for the GUI's tests
```

## Concepts

- **Collection** — a `.hurl` file: an ordered list of requests with method,
  URL, headers, cookies, body/form fields and optional `[Captures]`/`[Asserts]`.
  Postman `.json` exports open directly and are converted on the way in.
- **Environment** — a `.vars` file of `KEY=value` lines supplying `{{ VAR }}`
  values. See [Environments and secrets](#environments-and-secrets).
- **Workspace** — a folder of collections, reports and environments, browsed
  through a single tab as a filesystem tree.
- **Report** — a `.trail` file: a PaperTrail script that runs requests from a
  collection, loops over environments or data, and writes CSV/JSON/HTML/XLSX.
  Editable as text or as [blocks](#the-papertrail-block-editor); runnable from
  the UI or [headlessly](#reports).
- **Scratch Space** — tab 0. A collection with no file behind it until you save
  it.
- **Request names encode folders.** `Auth/Tokens/Refresh` browses as a folder
  path; Postman's folder structure imports into this automatically.

## Terminal UI

Press `?` or `F1` for the full, current key list. The essentials:

| Key | Action |
|---|---|
| `Tab` / `Shift+Tab` | Move focus between panes |
| `↑`/`↓`, `j`/`k` | Move selection |
| `←`/`→`, `h`/`l` | Switch tabs / scroll list text horizontally |
| `Enter` | Edit the selected request (or descend into a folder row) |
| `Shift+R` | Edit as raw Hurl text — for anything the form doesn't expose |
| `F5`, `Ctrl+Enter` | Run the current request |
| `Alt+F5` | Run the whole collection in one Hurl execution |
| `n` / `b` | New request / set the base URL |
| `f` / `s` | File menu / Settings menu |
| `Ctrl+S` | Save the open report, else the active collection |
| `[` / `]`, `PageUp`/`PageDown` | Previous / next tab |
| `Ctrl+Shift+←`/`→` | Reorder the active tab |
| `x` / `u` | Delete / undo — requests, tabs and environments each keep their own undo stack |
| `/` | Find a request anywhere in the collection (whole tree on a Workspace tab) |
| `Alt+↑`/`↓` | Reorder requests — the order `Alt+F5` and the CLI follow |
| `m` / `c` | Move / copy a request to another collection in the workspace |
| `p` (Requests) | Link an environment to this collection |
| `a` (Env pane) | Make an environment active |
| `r` (Env pane) | Retry a failed secret lookup |
| `w` (Workspace tab) | Reopen the file-tree picker |
| `+`/`-`, `<`/`>` | Resize the response pane / left column |
| `q`, `Ctrl+C` | Quit |

In the request wizard:

| Key | Action |
|---|---|
| `[`/`]`, `PageUp`/`PageDown` | Switch section tab (`All│Headers│Cookies│Form│Body│Asserts│Captures`). `[`/`]` only when focus isn't on a text field, so brackets stay typable |
| `Alt+1`–`6` | Jump straight to a section (`Alt` because most terminals can't report `Ctrl`+digit) |
| `Ctrl+↑`/`↓` | Previous / next section |
| `Ctrl+D` / `Ctrl+E` | Delete a row / toggle its enabled checkbox |
| `←` from a Key cell | Reach the enabled checkbox — it's the leftmost column |
| `Ctrl+F` or `Enter` on a File value | Open a file picker |
| `F2`, `Ctrl+Enter` | Save |
| `Esc` | Cancel (asks first if there are unsaved edits) |

Worth knowing:

- **`[Form]`/`[Multipart]`, `[Cookies]`, `[Captures]`, `[Asserts]` and
  `[BasicAuth]`** are all editable as tables in the wizard; the expected status
  is just an assert (`status == 200`). Saving picks the right Hurl section:
  all-text fields become `[Form]`, any file field promotes it to `[Multipart]`.
  File paths are colour-coded by whether they resolve and are readable
  (relative to the collection's directory, matching where Hurl looks). A
  `Base64 File` field is encoded at send time behind a configurable prefix, so
  `data:image/png;base64,` yields a ready-made data URI.
- **The request preview substitutes `{{ VAR }}`** and colours each by status —
  green loaded, cyan literal, orange loading, red missing — while the editor
  keeps the original text. Secrets are masked as eight dots.
- **Sections start empty** and dropdowns only auto-open on an empty cell, so
  arrowing through a populated table doesn't keep reopening them.
- **Settings ▸ Preferences** persists: confirm on exit/clear, confirm before
  deleting an environment or a request, always-save-when-prompted, whether
  `Alt+F5` runs the collection in batch mode (chaining cookies and captures),
  whether Esc discards request edits without asking (off by default), and the
  default Request view (JSON or Hurl).
- **Settings ▸ Theme** ships three presets — Britannia, Parisian Purple,
  Dannebrog — one per UI language (English/French/Danish), and follows the
  language until you pick one by hand. `Ctrl+N` clones a preset into an
  editable custom theme; `Enter` on a colour opens an RGB picker that previews
  live and auto-saves. `Ctrl+D` deletes a custom theme.
- **Saving.** **Save** overwrites the file the tab came from without
  confirmation; **Save As…** always prompts, and confirms an overwrite. Every
  File-menu item has a bracketed mnemonic that both selects and activates it.

## Graphical UI

`paperboy -g`, from a build with the `gui` feature. Feature-for-feature
equivalent to the terminal UI — same tabs, folder tree, request editor,
response viewer, environments panel, reports, theme editor, git remotes, and
the same three languages. What differs:

- **Panels and result columns are resized by dragging**; double-click a column
  border to hand it back to the automatic fit. Hand-set widths persist as long
  as the report keeps producing the same columns.
- **`Tab`/`Shift+Tab` cycle panels** in the terminal UI's order. The focused
  request list is arrow-driven: `Home`/`End`, `Enter` to run, `F2` to rename,
  `Delete` to delete, `Ctrl+Z` to undo. A Workspace tree adds `Left`/`Right` to
  collapse and expand, and `PageUp`/`PageDown` for ten rows.
- **Single-letter shortcuts don't carry over** — in a desktop window those keys
  are text. Globally: `F5`/`Ctrl+Enter` run, `Ctrl+S`/`Ctrl+Shift+S` save,
  `Ctrl+W` closes, `Ctrl+Z` undoes a delete, `Alt+F` opens the File menu, `F1`
  shows every shortcut.
- **The File menu is grouped by verb** (New / Import / Open / Save). Open ▸
  Collection and Load ▸ Environment take Postman exports too — they work out
  what the file holds. Every dialog reopens where you left it.
- **Workspaces are editable in place**: New adds a collection, report or
  environment; drag files and folders onto another folder to move them, or onto
  the empty space to move them back to the root. Nothing escapes the workspace
  root and nothing is silently overwritten.
- **Reports bind to their collection by a relative path** (`../apis/billing.hurl`
  included), so a workspace survives being zipped up or handed over. The
  `collection` dropdown offers the report's own workspace first and hides
  outside collections behind a toggle.
- **The window remembers itself** — size, every splitter you dragged, the open
  view, the selected report/request and the Workspace node.

### The PaperTrail block editor

Reports get a **Blocks** view alongside **Source** and **Results**: a
drag-and-drop editor where blocks are dragged from a palette, reordered, nested
inside `FOR` loops (which move as one, body included) and dropped on the trash
bar to delete. The drag outline and the drop marker are both drawn as the
block's own silhouette, at its real width and indent.

Editable on the blocks: the request a step runs, its alias, response format and
`SHOW(…)`/`HIDE(…)`/`STATISTICS(…)` lists; a `FOR` loop's binder, source, roles
and `PARALLEL(n)` concurrency; and the report's own settings — `collection`,
`output`, `environment`, `root`, `baseline`, `columns` — in a boxed panel at the
top of the flow. Those apply to the report rather than running as a step, so
they're deliberately not blocks. `output` names a *format* (`csv`, `json`,
`html`, `xlsx`), not a filename; only the CLI's `-o` takes a path. Everything has
hover help, and **Source** is highlighted with the terminal UI's colours,
underlining whatever the parser rejected.

### Desktop icon on Linux

Wayland has no per-window icon protocol, so shells match the window's app id
against an installed `.desktop` file. The first GUI launch writes
`$XDG_DATA_HOME/paperboy/paperboy_logo.png` and
`$XDG_DATA_HOME/applications/paperboy.desktop` (with `StartupWMClass=paperboy`
for X11) if they aren't already there, and never touches them again — so you
can customise them. The shell may need a rescan (log out, or restart it) to
notice. Delete both and relaunch to regenerate, which is also how you refresh
`Exec=` after moving the binary.

## Environments and secrets

A `.vars` file is one `KEY=value` per line. Values can be:

| Form | Example | Resolved by |
|---|---|---|
| Literal | `USERNAME=demo` | — |
| Process env var | `BASE_URL={{ env:DEMO_BASE_URL }}` | The process environment |
| 1Password | `API_TOKEN={{ op://Vault/Item/field }}` | The local `op` CLI |
| AWS SSM | `DB_PASSWORD={{ ssm:/path/to/param }}` | Local AWS auth |

Provider references resolve in the background at load time, and the resolved
values are never persisted — `state.json` keeps only the reference. Every
1Password reference across every open collection resolves in a single `op
inject` call, so you get one authorization prompt rather than one per
collection. `r` in the Environment panel retries a single failed entry. Editing
a value into something that looks like a reference triggers a load attempt, and
a "still secret?" checkbox decides whether the new value stays masked.

**Loading a `.vars` file substitutes nothing on its own.** It only joins the
Global Environments list. It has to be either:

- **active** — `a` in the Global Environments panel (GUI: the **Active**
  button). One at a time, shared by every tab; or
- **linked** — `p` in the Requests list pins one to the active collection (GUI:
  **Linked**).

Both at once merge, with the linked value winning. A collection still showing
raw `{{ VAR }}`, or a red "variables in this request are undefined" band, nearly
always means this step was missed.

A variable that is *defined but empty* is not undefined and warns about nothing
— it substitutes as an empty string. With Basic Auth that produces a
well-formed request that comes back `401`.

## Git remotes

Load and save collections, environments and whole workspaces straight from a
remote, with **no local clone**: PaperBoy lists refs, fetches just enough
history to read the file tree, and checks out only the files you actually
asked for. Nothing else in the repo touches your disk, however large it is.

**Loading** (File ▸ Load ▸ *kind* ▸ From Git…): give the URL — `https://…` or
`git@…`, with an optional access token used only for that fetch (GitHub-style
`https://x-access-token:<token>@host/…` is handled for you) — then pick a ref
and a file, both filterable as you type. `↓` on the URL field offers your
recent URLs. Loading a collection then offers to pair an environment from the
same listing, with no second round-trip. Anything loaded from git shows a ⎇ in
its tab title and remembers its origin.

**Workspaces** ask which files to fetch first — `.hurl` and `.json` (default),
`.hurl` only, `.json` only, or everything — and then whether to keep the
download temporarily or copy it somewhere permanent immediately.

> **A temporary workspace is never cleaned up.** Its files live in a temp
> folder for as long as the tab exists — including across a close and undo, and
> across restarts. They accumulate. Choose "save to a permanent location" when
> asked, or later via File ▸ Save ▸ Workspace ▸ Save As…, which copies the
> folder and stops tracking it as temporary.

**Saving** (File ▸ Save ▸ Collection ▸ To Git…) pushes a commit directly to the
remote. The URL is prefilled from where the collection came from, so you can
redirect it to a fork. You choose the in-repo path, whether the attached
environment goes in the same commit, and a branch or tag:

- A **branch** defaults to the one you loaded from, so `Enter` just appends a
  commit. `↓` lists the remote's branches. No merge or rebase is attempted — a
  non-fast-forward is reported as an error.
- A **tag** must be new. The remote is re-fetched immediately before the check,
  and an existing tag is always rejected with no way to force it.

The message defaults to `Update <name> via PaperBoy` and is editable. The author
is your git identity, or `PaperBoy <paperboy@localhost>` if you have none. A
branch push updates the remembered origin and clears the modified markers; a tag
push clears the markers but leaves the origin on your working branch.

To Git… only works for something loaded from git. For anything else, Save As…
into your own clone and use git normally.

## Importing from Postman

Already have an export? Just open it — **Open ▸ Collection** and **Load ▸
Environment** both work out what the file holds, and File ▸ Import from Postman
▸ *From an exported file* says so explicitly. No API key, no account.

To pull from an account, File ▸ Import ▸ Postman account… (terminal) or File ▸
Import from Postman ▸ From my Postman account… (GUI). Give it an API key, pick a
workspace, choose what to bring and where, and the result opens as a workspace.
Paste a workspace id — or its Postman address — on the first step to skip the
listing entirely.

Migrating off Postman altogether: `Ctrl+A` on the workspace list, or **Import
all** in the GUI. Everything the list is *showing* is imported (so the filter is
honoured), each workspace into its own folder, so two "Billing API" collections
from different workspaces both survive.

Postman rate-limits its API, so the wizard shows what it found and roughly how
long the download will take before fetching anything, then reports the remaining
time from the rate it is actually achieving and says when it is pausing to stay
inside the limit. A Postman API key carries its owner's full access and can't be
scoped, so a missing workspace is one your account isn't a member of.

The same import runs headlessly:

```sh
export POSTMAN_API_KEY='PMAK-…'

paperboy --postman-import                                       # list workspaces
paperboy --postman-import --postman-workspace 12ece9e1-… -o ~/API
paperboy --postman-import --postman-all -o ~/Postman            # every workspace
```

| Flag | Effect |
|---|---|
| `--postman-key` | The key, instead of `$POSTMAN_API_KEY`. Takes the same `{{ … }}` provider references as a `.vars` file — `'{{ op://Private/Postman/credential }}'` keeps it out of your shell history. Never written to disk, stripped from error messages. |
| `--postman-all` | Every visible workspace, each into its own folder under `-o`. Empty workspaces are skipped and inaccessible ones reported rather than fatal, so forty workspaces aren't stopped by one. Excludes `--postman-workspace`. |
| `--postman-what` | `collections`, `environments` or `all`. |
| `--postman-format` | `postman` (default) keeps the JSON byte for byte; `hurl` converts. |
| `--overwrite` | Replace a non-empty destination, which is otherwise refused. |
| `--postman-base-url` | Another tenant, e.g. `https://api.eu.postman.com` for EU Enterprise. |

The result is a folder of `Collections/` and `Environments/`; open it with Open
▸ Workspace.

### Converting to Hurl

`--postman-format hurl` brings across requests, folders (as `Folder/Name`
titles), headers, query parameters, raw bodies and form/multipart fields, plus:

- **Auth, including inheritance.** Collection- and folder-level auth is applied
  to requests that don't set their own, and `noauth` opts back out. `basic`,
  `bearer` and `apikey` (header or query) are mapped.
- **Collection variables**, which have nowhere to live in a `.hurl` file, as
  `<name> (collection variables).vars` beside the environments.
- `pm.<store>.set("NAME", body.a.b)` calls in test scripts, as `[Captures]`.

Hurl doesn't cover everything Postman does. Anything dropped — pre-request
scripts, OAuth 2, GraphQL bodies — is listed per request in
`CONVERSION-NOTES.md` at the root of the import; no file means nothing was lost.
A collection this build can't read is written out as its original JSON, so
converting can't cost you data.

## Headless runner

```sh
paperboy -c collection.hurl
paperboy -c collection.hurl -e environment.vars
paperboy -c collection.hurl --batch
```

`-c` takes a `.hurl` file or a Postman export. `-e` supplies the environment.
Exit status is `0` only if every request passed.

By default each request's method, URL, status, asserts, captures and truncated
body print as it finishes, coloured unless the output isn't a terminal or
`NO_COLOR` is set. Streaming runs one request at a time through the same `hurl`
runner, so captures still chain — but it can't carry Hurl's automatic cookie jar
between requests, and says so at startup. An explicit `[Cookies]` section is
unaffected. `-b`/`--batch` runs the collection as a single Hurl call, trading
incremental output for cookie continuity.

### Reports

`-r report.trail` runs a PaperTrail report and exits.

```sh
paperboy -r report.trail                                  # collection from the report's headers
paperboy -c api.hurl -r report.trail -o out.csv           # or given explicitly; - is stdout
paperboy -c api.hurl -e prod.vars -e staging.vars -r report.trail
paperboy -c api.hurl -r report.trail --dry-run            # expand it, send nothing
```

Without `-c`/`-e` the report's own `# collection:` / `# environment:` headers
apply, resolved relative to the report. `-e` is repeatable: each file is named
by its stem and becomes selectable in an `ENVS` loop, so `-e prod.vars -e
staging.vars` satisfies `FOR … IN ENVS BASELINE("prod"), COMPARISON("staging")`;
the first is also the base variable layer. `-o`'s extension picks the format
(`.csv`, `.json`, `.html`, `.xlsx`), `-` writes CSV to stdout, and omitting it
derives the filename from the report's own headers.
