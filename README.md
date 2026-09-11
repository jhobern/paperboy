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
cargo install paperboy --locked                       # terminal UI + headless runner
cargo install paperboy --locked --features gui        # …and the graphical UI
cargo install paperboy --locked --no-default-features # headless runner only
```

`--locked` is recommended: it builds the dependency versions PaperBoy was
tested against rather than re-resolving to whatever is newest. (A yanked
`arrayref` release took plain `cargo install` down on 2026-08-20 while
`--locked` kept working — and it took the terminal-only build with it, since
Cargo resolves optional dependencies whether or not their feature is on.)

The `gui` feature is opt-in because eframe/winit/wgpu roughly double the
dependency tree. Both builds share the same state file, so you lose nothing by
switching. Running `--gui` without it prints the command to install one with it.

`--no-default-features` turns the terminal UI off and leaves the headless
runner, which is the shape wanted for CI images and Docker containers: it drops
40 dependencies and about a third of PaperBoy's own source, none of which a
scripted `-c`/`-r` run would ever execute. The resulting binary takes the same
arguments and writes the same reports; only the interactive front-end is
missing, and running it with no arguments says so rather than doing nothing.

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
cargo run --no-default-features -- -c collection.hurl   # headless only
cargo test                          # add --features gui for the GUI's tests
```

PaperBoy builds in four shapes — headless, terminal, terminal + GUI, and GUI
alone — and CI checks all four, because a configuration nothing builds is a
configuration that stops compiling. Each must also stay warning-free; the
dead-code analysis is carried by the two shapes that include the terminal UI
(see the note at the top of `src/main.rs` for why).

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
- **Generated value** — a `# [Gen]` row: an expression evaluated just before a
  request is sent, supplying the nonces, timestamps and signatures a pre-request
  script used to. See [Generated values](#generated-values).
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
| `a` (Response) | Build an assert or a capture from what came back |
| `a` (Env pane) | Make an environment active |
| `r` (Env pane) | Retry a failed secret lookup |
| `w` (Workspace tab) | Reopen the file-tree picker |
| `+`/`-`, `<`/`>` | Resize the response pane / left column |
| `q`, `Ctrl+C` | Quit |

In the request wizard:

| Key | Action |
|---|---|
| `[`/`]`, `PageUp`/`PageDown` | Switch section tab (`All│Headers│Cookies│Queries│Options│Form│Body│Asserts│Captures│Reports│Generated`). `[`/`]` only when focus isn't on a text field, so brackets stay typable |
| `Alt+1`–`9`, `Alt+0` | Jump straight to a section (`Alt` because most terminals can't report `Ctrl`+digit) |
| `Ctrl+↑`/`↓` | Previous / next section |
| `Ctrl+D` / `Ctrl+E` | Delete a row / toggle its enabled checkbox |
| `Ctrl+Z` / `Ctrl+Shift+Z` | Undo / redo within the focused text cell |
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
- **Asserts and captures can be built from a response.** With a reply on
  screen, `a` in the Response pane opens a two-step palette: pick a value the
  server actually sent — status, duration, any header, any value in the JSON
  body, listed beside what it currently is — then pick what to say about it.
  The rows are the Hurl lines themselves (`jsonpath "$.data.token" == "ey…"`),
  so what you choose is what gets written. Typing narrows the list, and
  anything selected in the body pre-fills the filter. The last row on every
  value is *keep it in a variable*, which adds a `[Captures]` row under a name
  taken from the field itself — the fastest way to chain one request into the
  next. Choosing the status sets the `HTTP <status>` line rather than adding a
  competing assert.
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
- **Asserts and captures are built by right-clicking the response.** Click a
  value in the body and choose *Assert this…* — the field under the caret is
  worked out from the raw JSON, so it works on a minified body as well as a
  pretty-printed one. Right-clicking a header row does the same for that
  header, and the **Assert…** button beside Copy opens the same builder on the
  whole list of values the reply carried. The list is filterable by name or by
  value, and "keep it in a variable" adds the `[Captures]` row.
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

## Generated values

Some values can't be written down: a nonce, a timestamp, an HMAC over the two.
Postman uses a pre-request script; PaperBoy uses a `# [Gen]` block of named
expressions, evaluated immediately before the request is sent.

```hurl
# [Gen] 3
# nonce = random_hex(16)
# ts = timestamp
# sig = hmac_sha256_b64(API_SECRET, concat(nonce, ts))
POST https://api.example.com/orders
X-Nonce: {{nonce}}
X-Timestamp: {{ts}}
Authorization: HMAC {{sig}}
```

The expressions stay in comments and the request refers to results as ordinary
`{{name}}` placeholders, so the file remains a plain `.hurl` file: stock `hurl`
parses it byte for byte and runs it given `--variable nonce=… --variable sig=…`.
Nothing else could work — Hurl reads a placeholder only as far as the first
character outside `A-Za-z0-9_-` and discards the rest silently, so
`{{ hmac_sha256(K, M) }}` would be sent as the value of `hmac_sha256`. PaperBoy
now refuses to save such a placeholder rather than let it truncate.

The block may sit above the request line, as here, or immediately below it;
both are read. PaperBoy writes it below when it saves, so a hand-written file
in the other order moves its block down the first time it is saved and is
otherwise unchanged.

A bare identifier is a variable reference — an environment variable, a request
parameter, or an earlier row in the same block. Calls nest. Rows are evaluated
in order and a row may only refer to one above it. Values are computed per run,
never previewed, and never written to `state.json`; a secret read through
`{{ op://… }}` is no more exposed by signing with it than by sending it.

What a block computes stays available to the rest of the session, exactly as a
`[Captures]` value does: sign a request, and the request after it can echo the
same `{{nonce}}` — including when you run it on its own. (Memory only, for the
reason above: a fresh PaperBoy computes fresh values.) A `[Captures]` row of
the same name is the later, more specific statement and wins.

One request per name, though. "Run All" and `paperboy -c` normally run one
request at a time, so each block is evaluated in its own window and two
requests may each have their own `nonce`. A **batch** run (the `--batch` flag,
or the Run All batch preference) is a single Hurl call over the whole file with
one variable set, so there the two share the first request's value — a
signature computed over another request's nonce. Both front-ends say so before
starting such a run, and `--batch` prints the warning too; the fix is usually
to not use batch.

The same applies to a name the environment already defines. Running one request
at a time, a block's value overrides the environment's from that request
onwards; a batch has one variable set for the whole file, so it cannot override
from partway through without changing what the *earlier* requests send. Batch
therefore leaves the environment's value in place and says which names it
did that to.

`counter` counts within the process, not within a run: it starts at 1 the first
time it is evaluated and keeps going for as long as PaperBoy is open, so sending
the same request three times gives 1, 2, 3. It is a sequence, not a setting, and
is not saved — a restarted PaperBoy counts from 1 again.

Edit the block in the request wizard's **Generated** section (`Alt+0`), in the
GUI editor's **Generated** tab, or as text. Both editors offer the functions as
you type — with their arguments named — and the GUI's **Function…** menu lists
them all; either way the call is written at the caret, over any part-typed
name, with the caret left between the brackets. Both say what is wrong with a
row while
it is still a typo rather than leaving it to be a 401: an unknown function, the
wrong number of arguments, an expression that doesn't parse. Placeholders that a
generator will fill render in the theme's *generated* colour and keep their
braces, because the value doesn't exist yet.

| | |
|---|---|
| Time | `timestamp`, `timestamp_ms`, `iso8601`, `date(fmt)` (strftime, UTC) |
| Random | `uuid`, `counter`, `random_int(lo, hi)`, `random_hex(n)`, `random_alnum(n)`, `random_base64(n)` |
| Encoding | `base64`, `base64url`, `base64_decode`, `hex`, `urlencode`, `urldecode`, `json_string` |
| Hashes | `md5`, `sha1`, `sha256`, `sha512` |
| MACs | `hmac_sha1(key, msg)`, `hmac_sha256`, `hmac_sha512` |
| Text | `concat(…)`, `upper`, `lower`, `trim`, `split(text, sep, n)`, `regex(text, pattern)` |
| JSON | `jsonpath(text, path)` |
| Request | `method`, `url`, `path`, `query`, `header(name)`, `body`, `request_name` |

`jsonpath(text, path)` reads a value out of a JSON document the block already
has in hand — this request's own `body()`, or a response an earlier request
captured whole. When the value comes straight from a response a `[Captures]`
row is the right tool; this is for the cases a capture can't reach, which is
anything that has to be *computed* from the value: signing part of a payload,
or building this request's body out of pieces of the last one's. It walks `$`,
`.name`, `["name"]`, `[n]` and `[?(@.key == 'x')]` — the last of which is how
you address an API that returns its fields as a list of key/value objects. A
string comes back as its text (not with the quotes still on), an object or
array as compact JSON, and `null` is an error rather than the four characters
`null`. Wildcards, recursive descent, slices and unions are refused by name
rather than half-implemented, pointing you at the `[Captures]` row that has
Hurl's full JSONPath: the same path meaning two different things in one
request is worse than not being able to write it.

Every hash and MAC returns lowercase hex — matching `sha256sum` and CryptoJS's
`.toString()`, so a ported Postman script lands right — and each has a `_b64`
variant returning standard padded Base64 and a `_b64url` variant returning the
URL-safe alphabet without padding — the encoding a JWT segment is made of,
where `+`, `/` and `=` are all wrong. The encoding is in the name rather
than a default because a signature in the wrong one is the right length,
entirely plausible to look at, and rejected with the same `401` as a wrong
secret. Note that `base64(sha256(m))` is *not* `sha256_b64(m)`: the first
encodes 64 hex characters, the second the 32 bytes they spell.

The **Request** functions read the request the block belongs to — the method as
sent, the body as it goes on the wire (no JSON comments, no switched-off
headers) — which is how a signature over "the thing I am about to send" is
written. They read the text as authored, substituted against the rows above
them: a row reading `body()` sees earlier rows filled in and later ones still as
`{{name}}`, so a value can never depend on a row that depends on it. Without a
request behind the block — the editor's live check on a row you are still
typing — they say so rather than answering with nothing, because an HMAC over a
silently empty body is a signature that authorises nothing.

`split` counts pieces from the end when given a negative index, so the last
segment of a path is `split(path(), "/", -1)` — JavaScript's `.pop()`, which is
the shape these scripts are written in. `regex` is the escape hatch for what
`split` can't reach: the first capture group if the pattern has one, otherwise
the whole match. Both treat "no such piece" and "matched nothing" as faults
rather than an empty answer, since that text goes on to be signed or sent.

**A name is written bare, not in braces.** Everywhere else in PaperBoy a
variable is `{{name}}`; inside a generator expression it is just `name`, because
an expression already names things — `concat("Bearer ", TOKEN)`. Writing
`"{{TOKEN}}"` there is refused rather than accepted as a string, since a
signature over the eight characters `{{TOKEN}}` is the right length, entirely
plausible, and rejected with the same `401` as a wrong secret. (`\{` is the
escape, for a string that really does want a brace.) A row can also be a plain
literal — `expected = "APPROVED"` — which is how an assert compares against a
per-request expectation: `jsonpath "$.status" == "{{expected}}"`.

**Canonicalisation is yours.** PaperBoy signs exactly the bytes you assemble; it
will not build a canonical request from the live headers, so AWS SigV4 and
friends are out of scope. Chaining a MAC into the *key* of the next one isn't
expressible either, since every value here is text.

The block works headlessly too. `paperboy -c …` evaluates each request's rows
in its own window, so a generator can read a value an earlier request captured
and two requests each get their own nonce. `--batch` is a single Hurl call over
the whole file and has no such window: there every block is evaluated once
before the run, a name computed by two requests takes the first one's value for
both, and the run says so before it starts.

A row that fails — unknown function, wrong arity, a name nothing defines —
reports rather than blocks the send. It binds nothing, so `{{sig}}` goes out
literally and comes back a loud `401`, which is easier to diagnose than a
refusal.

Importing from Postman maps the dynamic variables that have an exact equivalent:
`$guid`/`$randomUUID` and `$isoTimestamp` become Hurl's own `{{newUuid}}` and
`{{newDate}}`, while `$timestamp`, `$randomInt` and `$randomAlphaNumeric` become
`[Gen]` rows. The rest
are renamed and listed in `CONVERSION-NOTES.md` as values you must supply —
guessing at `$randomFirstName` would send a plausible wrong value, which is
harder to notice than a request that won't run.

Worked examples — collections to import, and the `.hurl` file they should become
— are in [`examples/postman/`](examples/postman/).

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
- **Dynamic variables.** `{{$guid}}` and `{{$isoTimestamp}}` become Hurl's own
  `{{newUuid}}`/`{{newDate}}`; `{{$timestamp}}`, `{{$randomInt}}` and
  `{{$randomAlphaNumeric}}` become [generated values](#generated-values). The rest
  are listed as values to supply.
- **Pre-request scripts**, as far as they reduce to values PaperBoy can compute:
  `pm.environment.set("id", uuid.v4())`, `Date.now()`,
  `Math.floor(Date.now() / 1000)`, `new Date().toISOString()`,
  `pm.variables.replaceIn("{{$guid}}")` and literals become
  [generated values](#generated-values).
- **Test scripts**, as the status and assertions they always make:
  `pm.response.to.have.status(400)` becomes the request's expected status, and
  `pm.expect(...)` checks on the body, headers and response time become
  `[Asserts]`. A deep equality against a literal document
  (`.to.eql({ id: 7, name: "Ada" })`) is written out one leaf at a time, since
  Hurl has no predicate that takes a document — which also makes a failure name
  the field that differed. Only checks that run *unconditionally* are taken —
  anything inside an `if`, a loop or a helper function is left for you, since an
  assertion that was meant for one branch fails every run.
- **`setNextRequest`**, as a note saying which of the four things it was doing:
  polling (which Hurl writes as `[Options] retry`), an order you can write down
  in the file or as `REQUEST` lines in a
  [PaperTrail flow](#the-papertrail-block-editor), a run that stopped early, or
  a request name built as the script ran. PaperBoy runs a
  collection in file order, so none of them convert — but they are four
  different problems with four different fixes.
- **Scripts on a folder or on the collection**, which Postman runs for every
  request inside; they are converted for each request they cover, and reported
  once against the folder that holds them.

Hurl doesn't cover everything Postman does. Anything dropped — the rest of a
script, OAuth 2, GraphQL bodies — is listed per request in
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

#### Dependency graphs

Inside a `GRAPH … END` region the order statements are written in stops being
the order they run in. PaperBoy reads what each request needs and what each
produces, works out the ordering that satisfies those, and runs that. A region
that cannot be ordered — two requests each waiting on the other — is an error,
and nothing in the report is sent: the author has said written order is not the
specification, so falling back to it would be the one answer guaranteed to be
wrong.

```
GRAPH release
    REQUEST auth/login AS login          # captures token
    REQUEST orders/create AS order       # uses {{token}}
    REQUEST orders/get USING(id = {{order.id}})
END
```

`--targets a,b` runs only the named steps and whatever they transitively
depend on, so a release check can ask for one answer without paying for the
whole report. Naming a step that no region declares is an error rather than a
silent empty run. `--dry-run` lists the steps grouped by how deep in the graph
they sit, which is how you check the shape of a region without sending
anything.

`DEPENDS` states an ordering the data doesn't show. Inference only sees values
flowing from a capture to a reference, and some dependencies leave no such
trace — uploading a file that a later request fetches by an id it already had,
say. `REQUEST dfa/result DEPENDS upload` says so outright. Names are the step
names, separated by commas, and a `DEPENDS` is only meaningful inside a region:
outside one, written order already *is* the order, so PaperBoy rejects it
rather than let it look like it did something.

Clauses may be written in any order, and a long statement may gather them into
a bracketed group opening on the statement's own line:

```
GRAPH
    REQUEST dfa/result AS result (
        DEPENDS upload, session
        USING(query.id = "{{session.id}}")
    )
END
```

#### Running a region in parallel

`PARALLEL(n) GRAPH … END` lets up to `n` steps overlap. Each is taken the
moment its dependencies are done — not a wave at a time, which would make the
region cost the slowest step at every depth. A cap is permission, not an
instruction: a chain still runs one at a time however high `n` is set, and the
report is identical at any degree, because rows, columns and errors are merged
in plan order rather than in the order workers happened to finish.

#### Shuffling, and why

A region is a *claim* that its edges — inferred and declared — are the complete
set. PaperBoy cannot verify that claim. It can help you falsify it.

With the default tie-break, ready steps run in written order, so a dependency
nobody declared keeps working by accident and surfaces months later when
something unrelated moves. `--shuffle` picks at random among the steps that are
ready, which turns that into a failure now, and prints the seed:

```
  Shuffle    : seed 4711 (replay with --shuffle=4711)
```

`--shuffle=4711` replays that run. Shuffling only reorders steps that may
legally run in any order; it never runs a step before what it depends on.

How exact the replay is depends on the degree. A sequential region replays
*exactly*: the seed alone decides every choice. A `PARALLEL(n)` region replays
its dispatch *preferences* exactly, but which step becomes ready next also
depends on which request came back first, and no seed controls the network. So
a shuffled failure in a parallel region is far more likely to reproduce under
its seed than without one, but it is not guaranteed to. If you find one and
want it nailed down, re-run the seed with `PARALLEL(1)` — a missing dependency
is a property of the ordering, not of the concurrency, so it will still be
there.

#### Cleanup

`CLEANUP` marks a request that undoes something — deleting a session, releasing
a lock. It is written where it belongs logically but runs at the end of its
block: at the end of the flow at the top level, at the end of each iteration
inside a `FOR`. Cleanups run in reverse dependency order, so a thing is torn
down before whatever it was built on.

```
REQUEST auth/login AS login
CLEANUP auth/logout USING(header.Authorization = "{{login.token}}")
REQUEST orders/create
```

A cleanup whose dependency never succeeded is skipped — there is nothing to
undo — and a cleanup that fails is reported as a warning rather than an error,
because a teardown failing is nearly always a consequence of the real failure
and shouldn't be allowed to bury it.

#### Carrying the requests in the report

A report normally names a collection to draw its requests from. It can instead
carry them itself, in a `REQUESTS` section — plain Hurl, which must be the last
thing in the file:

```
# name: Health check

GRAPH
    REPORT REQUEST ping
END

REQUESTS

# ping
GET https://example.com/ping
[Asserts]
status == 200
```

That runs with no `# collection:` line and no sibling `.hurl` file:
`paperboy -r health.trail`. A report that embeds its requests may still name a
collection as well, in which case both sets are available and a name used by
both is an error — a reference has to mean one thing.

Embed when the requests exist only to serve the flow, so that the whole check
travels as one file and nothing can be moved out from under it. Reference a
collection when the requests *are* the API surface under test and other things
use them too. Note what embedding does and doesn't buy: it removes the sibling
collection file, not a fixture directory that `FOR … IN FILES` reads.

#### Exit codes

| Code | Meaning |
| ---- | ------- |
| `0`  | Everything ran and every assertion passed. |
| `1`  | Something failed: a request, an assertion, or the report itself. |
| `3`  | Steps were skipped because something they depended on failed. |

`3` implies `1` — a skip only ever follows a failure — and says the run is
additionally incomplete, so a pipeline that only cares about pass/fail can
treat any non-zero code the same way while one that reruns can tell the
difference. `2` is left alone: it is what `clap` uses for a bad command line.
