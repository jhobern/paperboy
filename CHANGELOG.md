# Changelog

All notable changes to PaperBoy are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Releases before 0.1.2 predate this changelog and are not recorded here.


## [0.4.0] - unreleased

### Fixed

- **A Postman import no longer queues behind its own latency.** Items were
  fetched strictly one at a time, so on a link with high round-trip times the
  import crawled at one or two items a second even with hundreds of calls still
  available in the account's allowance. Up to six fetches are now in flight at
  once. The pacer is still the sole gatekeeper — it hands out slots without
  holding its lock across the wait, so workers sleep in parallel rather than
  queueing on each other — and results are put back into request order before
  anything is written, so the names given to duplicate items don't change from
  run to run.
- **The test suite no longer invokes the real `op` or `aws` command.** Running
  the tests could pop up a 1Password authorisation prompt: the asynchronous
  secret resolution already swapped in a do-nothing resolver under test, but
  the synchronous path used when parsing a `.vars` file or resolving a single
  reference still shelled out for real. Both now go through the same
  test-aware default.
- **A slow Postman import now says what it is waiting for, and how much
  allowance is left.** Listing a large workspace draws on Postman's strictest
  rate limit, so the importer paces itself and backs off when told to — but
  only the *download* screen ever drew that, leaving the listing step sitting
  on "Checking what that workspace holds…" for minutes with nothing to
  distinguish it from a hung app. The busy line now names the reason for the
  wait and how long it has been waiting, and both front-ends show the account's
  own figures, taken from the rate headers we were already reading and
  discarding: calls left before the window resets, calls left this month, and
  the spacing currently being kept between calls. The remaining-time figure is
  also labelled as the estimate it is — it moves about as paced waits land
  inside the average.

- **The Postman wizard stops moving under the cursor.** Choosing where the API
  key lives changes the label beside the key field ("API key" versus
  "1Password item"), and both front-ends were sized to their contents: in the
  terminal the value column jumped sideways, and in the GUI the whole dialog
  grew and shrank. The label column and the dialog width are now reserved for
  the widest source, so switching between them changes nothing but the label.

- **A GUI dialog now behaves like a dialog.** They were anchored to the middle
  of the window and fixed in size, so one could not be dragged aside to read
  what it was covering, and the file/collection pickers inside the git and
  Postman wizards were stuck at a few visible rows however large the window
  was. Dialogs now open centred but can be moved, the two wizards can be
  resized (with their lists growing to fill them), and the app behind is
  covered by a dimmed sheet that blocks clicks and keyboard shortcuts — so a
  second wizard can no longer be opened on top of the first, and Ctrl+S no
  longer saves the tab hidden behind the dialog. The sheet blocks *input*
  only: the app keeps painting and background work (a git fetch, a running
  report) keeps finishing while a dialog is up.

- **A workspace downloaded from git arrives with its environments and
  reports.** The recommended file filter took `.hurl` and `.json` only, so a
  repo holding requests, `.vars` environments and `.trail` reports downloaded
  the requests and quietly dropped everything they needed to run — the only way
  to get them was "All files", which also dragged down every unrelated binary in
  the repo. The recommended filter is now "files PaperBoy can open" and covers
  all four. Workspaces already pinned to the old filter keep working.

### Changed

- **Running a report that asks for values now always stops at the questions
  first.** Run opens the run settings; Run again — from the settings — starts
  the run. The values decide what the run *means* and a run can take minutes,
  so they are worth a glance on the way past; more practically, this is the way
  back to them once results have filled the screen, which previously took
  knowing that `p` existed. Opening the questions discards nothing, so the
  unexported-results warning is held back until the run actually starts.

- **Long pick-lists in a report filter as you type.** The environment and
  choice pickers used to treat any letter as "cancel", which made picking one
  of a dozen loaded environments a scroll. Typing now narrows the list on a
  case-insensitive substring, Backspace widens it again, and `j`/`k` no longer
  move the cursor there — a letter is a letter.

- **Clicking a report's results grid or its questions selects the report.**
  Both moved their own cursor while keyboard focus stayed on the panel beside
  them, so the next keypress went somewhere the user wasn't looking. Run
  settings rows also answer the mouse now: one click selects, a second opens
  the row's editor.

- **Save no longer asks whether you meant it.** Saving a collection,
  environment or report back to the file it came from used to raise a
  confirmation counting the changes about to be written. Overwriting that file
  with those changes is what Save *means*, so the dialog was asking the user to
  confirm the thing they had just asked for — two keystrokes on the way to
  every save. It now writes immediately. *Save As* still confirms before
  overwriting a **different** file, which is a real surprise worth catching.

### Added

- **A comparison can be pointed at its two stacks by parameter.** An `ENVS`
  clause may name an environment (or a `FILE(…)` snapshot) through a parameter
  — `BASELINE("{{BASELINE_ENV}}"), COMPARISON("{{COMPARE_ENV}}")` — so the
  report that compares staging against dev this week compares another pair next
  week without being edited or copied. Both halves of a comparison resolve the
  same way, so the collapse still finds the rows the loop produced. Validation
  judges such a name by what it means rather than by what it says: it must name
  a declared `PARAM` (an `ENVS` clause is read before anything has run, so a
  capture or an assignment would mean something different depending on where
  the run had got to), and a default that currently names an unloaded
  environment is a warning rather than the error a literal name gets.

- **The terminal UI asks for the values a report declares.** A report with
  `PARAM`s opens on a *Run settings* view (`p` from the source, Esc back)
  listing one row per parameter: what it asks for in words, the value this run
  will use, and its raw name and type dimmed beside it. Enter opens whichever
  control the type deserves — a list of a `CHOICE`'s own choices or of the
  loaded environments, the file browser for a `FILE`/`FOLDER` (stored relative
  to the report, like `# root:`), a text field for the rest — and a value the
  type wouldn't accept is refused there rather than at the run. Pressing `r`
  with a required value still unanswered doesn't run: it says which one and
  puts you back on the row. The answers are remembered per report (and survive
  a restart), so a report someone runs every week opens on last week's answers
  and is a glance and an `r`.

- **Reports can ask for a value before they run.** A `PARAM` statement declares
  a variable whose value is offered to the user rather than baked into the
  file, so a report that differs from run to run only by its environment, its
  input folder or an API version no longer has to be edited or copied to be
  re-run:

  ```
  PARAM ENV    TARGET  = "staging_au"            LABEL "Environment"
  PARAM CHOICE("v4.2", "v4.3") VERSION = "v4.3"  LABEL "API version"
  PARAM FOLDER IMAGES  = "./tickets"
  ```

  A parameter is an assignment the user has a say in — `{{TARGET}}` reads it
  like any other variable — with a type (`TEXT`, `NUMBER`, `ENV`, `FOLDER`,
  `FILE` or `CHOICE(…)`) so each front-end can put the right control behind it,
  and an optional `LABEL` to prompt with. Omitting the default makes the
  parameter required. Parameters live in the prelude, so the whole set can be
  read off a report without running it; validation says so when one is written
  too late or hidden in a loop, when two share a name, or when a default
  disagrees with its own type. Where no `LABEL` is written one is derived from
  the name rather than shouting the identifier at whoever is running the
  report — `TICKET_REF` asks for "Ticket ref" — and two parameters that end up
  asking the same question are flagged. A run binds whichever values it was
  given, falling back to the declared defaults, and holds them to the same
  rules as the defaults — a choice off the list, a number that isn't one, or a
  required parameter nobody supplied stops the run with a reason instead of
  producing plausible-looking rows built from a URL with a hole in it. The run
  settings that offer the values, and the `--param` flag that supplies them
  headlessly, are next.

- **The Postman importer remembers where your key lives.** A key read from
  1Password, SSM or the environment is kept as a *reference* — the address, not
  the credential — and offered back next time, the same way the git wizard
  offers the repositories you have used: a dropdown under the key field in the
  terminal (↓ to open), a Recent menu beside it in the GUI. The list is
  filtered to the source you have chosen, so picking SSM doesn't offer
  1Password paths, and only references that Postman actually accepted are
  kept. A pasted key is never written to disk.

- **The Postman importer offers 1Password first.** A pasted API key is a live
  credential sitting in a text field, where a reference is only its address, so
  the wizard now opens on 1Password and lists pasting last. Pasting is still
  supported, and an existing key is still recognised for whatever source it
  came from.

- **The GUI File menu names what it opens before where it comes from.** It used
  to lead with the source — "Open", "Open from Git", and a bare "From
  Postman…" that never said what a Postman import produces. It now follows the
  terminal's shape: **Open ▸ Workspace ▸ From Postman…**, with every source for
  a thing listed together (a file, a folder, Git, Postman), and **Save As ▸
  Collection ▸ To Git…** the same way round. Entries that cannot work — pushing
  a workspace that never came from git, saving a report with none open — stay
  visible but disabled rather than being hidden or silently failing.
- **Reports load from Git in the GUI.** The terminal has always offered it; the
  GUI's git wizard only knew about collections, environments and workspaces, so
  a `.trail` living in a repo had to be cloned by hand first. Open ▸ Report ▸
  From Git… now lists the repo's `.trail` files, opens the chosen one in the
  report editor, and remembers where it came from so Save As ▸ Report ▸ To Git…
  pushes straight back to it.

- **The GUI's git wizard offers the repositories you have already used.** The
  terminal has kept a list of recent remote URLs for a while and offers it under
  the URL field; the GUI kept writing to the same list but never showed it, so a
  git URL had to be typed out (or pasted from somewhere else) every time. Both
  the load and the save wizard now show a **Recent** menu beside the URL field,
  backed by that same shared list — a repo opened in one front-end is offered by
  the other — and both URL and token fields carry their examples inside them.

- **The GUI's Postman importer asks where the key lives, like the terminal
  does.** The connect step wanted an API key and hinted, in prose, that a
  provider reference could be typed instead — which only helps someone who
  already knows the syntax. It now offers the source (pasted key, 1Password,
  AWS SSM, environment variable) beside the field and writes the reference
  itself, so a 1Password user types the item path their password manager shows
  them. The remaining steps lost their stacked explanations: the fields carry
  their examples inside them and the window title says what the dialog is, so
  the options screen no longer repeats it back.

- **Every GUI dialog can be left by Escape or by its corner.** The windows had
  no ✕ and ignored Escape, so a dialog opened by accident could only be
  answered — and the destructive confirmations re-arm themselves when they are
  clicked away, so "wait for it to go" was not an option either. All of them
  (rename, new name, theme editor, export results, revert to saved, the unsaved
  quit/close warnings, the workspace reload offer, the Postman importer, the
  git load wizard and the report node wizard) now carry a close button and
  treat both it and Escape as their own Cancel, so dismissing does exactly what
  the Cancel button did — an unanswered reload offer, for instance, is still
  recorded on the tab and comes back next launch rather than being lost.

- **The wizards say less and show more.** The Postman importer, the git load
  wizard and the git save wizard were each a stack of fields with a sentence of
  explanation underneath every one — a screen of prose to answer two questions.
  They now follow the request editor's shape: a label column with the value
  beside it, and the shortcut keys on the panel's bottom border rather than
  taking a row of the body. What a field wants is shown as a dim example *in*
  the field, where the answer is about to be typed ("https://github.com/owner/
  repo", "Private/Postman/credential", "blank: choose from a list"), so the
  explanation is read at the moment it is needed and costs nothing when it
  isn't. The Postman connect step went from twelve rows to four and the git
  ones from eight to two; the wording that survived is shorter throughout. If
  it took a paragraph to say what a field was for, the field was not clear
  enough.

- **Reports export as PDF.** `# output: pdf`, or a `.pdf` filename in either
  front-end's export dialog, writes a printable landscape A4 document: the
  table sized to its content and fitted to the page, the column headers
  repeated on every page, cells tinted exactly as the spreadsheet and the HTML
  tint them, an `IMAGE` column's pictures embedded at the size the clause asked
  for, and the statistics and ground-truth figures in the footer. `DETAIL`
  columns are left out of the table — paper has no click, and a drill-down
  column is usually a whole response body that would crush every other column
  in a printed grid; the lossless exports still carry them. The file is
  assembled by hand rather than through a PDF crate, so the format costs no new
  dependency: text is wrapped here against Helvetica's real glyph widths, a
  JPEG is embedded byte-for-byte as the DCT stream it already is, and anything
  else reuses a PNG's own compressed pixel stream.

- **The Postman importer takes a key reference, not just a key.** The wizard's
  API key field now accepts the same `{{ … }}` provider references a `.vars`
  file does — `{{ op://Private/Postman/credential }}`, `{{ ssm:/path }}`,
  `{{ env:NAME }}` — resolved when you press Enter, so nobody has to fetch a
  live credential out of 1Password and paste it into a form (the headless
  `--postman-key` has accepted these all along; now the front-ends match). An
  import lists, plans and downloads with a client apiece, so the resolved value
  is held for the run rather than prompting three times, and — like every
  resolved secret — it is never written to disk. A reference the provider won't
  answer is reported as such instead of being sent as a key. The terminal
  wizard no longer expects you to know that syntax: its first row is a **key
  source** — Paste, 1Password, AWS SSM or an environment variable — and once
  you have picked one you type only the part you can read off the provider (the
  item path 1Password's own "Copy Secret Reference" gives you, the parameter
  name, the variable name) and the wizard assembles the reference. Only a
  pasted key is masked on screen; an item path is an address rather than a
  credential, and hiding it would only stop you checking you typed it right. A
  wizard reopened on an existing key shows it the way it was entered, and
  someone who already knows the syntax can still type it in full — it is
  recognised rather than wrapped a second time.

- **A report says what moved, not just how it scored.** Two runs that both score
  98% are not the same run if one of them fixed three rows and broke three
  others, and the accuracy figures cannot tell them apart. The metrics header —
  in the exported HTML, in the GUI and in the terminal — now leads with how the
  run moved against its baseline: how many rows were **Fixed**, how many
  **Regressed**, and how many are **Still wrong**, or a single "Nothing moved"
  where every scored row landed where it did last time. The xlsx `Metrics` sheet
  and the JSON export carry the same figures as numbers, so a CI gate can ask
  "did anything regress?" without counting rows. A run with no baseline says
  nothing about movement: it hasn't stayed still, it has nothing to have moved
  from.

- **The exported HTML report has a dark mode.** It was a white page whatever the
  screen it was opened on, which is a bright thing to hand someone at the end of
  a long day. It now follows the reader's own system setting, and carries a
  **Dark**/**Light** toggle at the end of its toolbar for when the two disagree
  — a dark desktop and a report going on a projector. The choice is remembered
  between openings where the browser allows it. Every colour in the file now
  comes from one palette declared in one place, including the confusion matrix's
  heatmap, which is mixed back toward the page rather than glowing on it.

- **Open an exported report where it belongs.** The interactive HTML export is
  written to be read in a browser and the xlsx in a spreadsheet, but both
  front-ends stopped at naming the file, leaving you to go and find it. After a
  successful export the terminal UI opens it with **Ctrl+O** and the GUI grows
  an **Open** button beside Export, each handing the file to the desktop's
  default application. The offer only stands while the file still describes the
  run on screen — a rerun withdraws it, because an "open" that shows the
  previous run's numbers is worse than none.

- **The terminal results view states the score and filters the rows.** It drew
  the grid and nothing else, so a ground-truthed run had to be exported to find
  out how it did, and there was no way to ask for just the rows that were wrong.
  A summary is now pinned above the grid — compared, incorrect and accuracy per
  scored column — and **`/`** cycles the same filters the GUI's bar and the
  interactive HTML export offer (All, Differences, Incorrect, Regressions). The
  results panel's own title says which filter is up and how many rows of how
  many it leaves on screen, so the grid pays no rows for it. Both read from the shared
  metrics and filter modules, so all three views quote the same accuracy and
  hide the same rows. A report with no ground truth and nothing to filter is
  drawn exactly as before.

- **`TRUTH`, `DETAIL`, `STATISTICS` and `IMAGE` are syntax highlighted.** The
  per-column clauses were the only part of PaperTrail the source view treated as
  plain data, so `TRUTH "pass" DETAIL` read as a column name and a string. They
  now take the same colour their editor chips carry, and their arguments
  (`WIDTH`, `HEIGHT`, `FIT`, `MEAN`, `COUNT`, …) are accented — but only inside
  their own parentheses, so a column honestly called `count` or `width` stays
  plain text.

- **The GUI results view has confusion matrices and per-row drill-down.** Each
  ground-truthed column's matrix is drawn under the metric cards, shaded off the
  same ramp the HTML export uses, and every non-empty count is a button that
  selects the rows it counted. A row with pictures or `DETAIL` columns gets an
  expander that opens a panel holding its pictures at full size, its detail
  columns in full, and a field-by-field diff against the baseline where there is
  one. `DETAIL` columns now leave the in-app grid for that panel, exactly as they
  leave the exported table.

- **The GUI results view has metric cards, row filters and a find box.** The
  figures and filters that made the interactive HTML export worth exporting are
  now drawn natively above the results grid, so a ground-truthed run can be
  judged and picked through without exporting it first. The filters offered are
  the ones the run has something to select (differences, incorrect rows,
  regressions), the find box narrows whatever the filter chose, and the count
  reports how many of the run's rows survive both. Both are computed from the
  same `metrics`/`filter` core the HTML writer uses, so the two views can never
  disagree. A run that is still streaming is shown unfiltered, since filtering
  hides pending rows.

- **`TRUTH`, `IMAGE` and `DETAIL` are editable in both structured editors.**
  The three trailing clauses attach identically to a `WITH` field, a
  `REPORT <var> AS` column and a computed column, so all three forms in the TUI
  node editor and the GUI block editor now show one shared clause block instead
  of carrying the clauses invisibly and writing them back untouched.

  `IMAGE` is a toggle that reveals `FIT`, `HEIGHT` and `WIDTH` — most columns
  are not pictures, and three permanently-visible size rows would bury the two
  rows every column needs. `FIT` and a pixel size answer the same question, so
  picking `FIT` clears the sizes rather than emitting a spec a writer would have
  to arbitrate. A clause on its own is now enough to promote a bare
  `REPORT <var>` to the named form, falling back to the variable's own name.

- **Columns can be marked as supporting evidence with `DETAIL`.** A bare
  `DETAIL` flag, alongside `STATISTICS(…)`, `IMAGE(…)` and `TRUTH "…"` on any
  named column, says the column is worth keeping but not worth a slot in the
  main grid:

  ```
  REPORT REQUEST classify AS c WITH
      Verdict: jsonpath "$.decision" TRUTH "{{ expected }}"
      Payload: jsonpath "$.raw" DETAIL
  END
  ```

  It is *placement*, not content: a `DETAIL` column is still exported to CSV and
  JSON, still compared, and still stored in a baseline snapshot. Writers that
  have somewhere to put it use it; the rest ignore it.

- **The HTML export is interactive.** Clicking a row (or pressing Enter/Space on
  it) opens a drill-down panel holding the row's pictures at full size, its
  `DETAIL` columns pretty-printed, and — when the run compared against a
  baseline — a field-by-field diff of whichever of them are JSON on both sides,
  with the fields that moved highlighted. Lists of `{key, value}` objects are
  aligned by their `key` rather than by position, so a response whose checks
  come back in a different order no longer reads as "everything changed".

  Above the table there is a filter toolbar — **All**, **Differences**,
  **Incorrect**, **Regressions**, each offered only when the report has rows in
  that class — a live text search, and clickable confusion-matrix cells that
  narrow the table to exactly the rows a cell counted.

  The file is still a single self-contained document with no external references
  of any kind, and with scripting disabled it renders every panel expanded
  rather than losing them.

- **xlsx groups `DETAIL` columns.** They move to the right of the summary
  columns and are written as a collapsed outline group — the spreadsheet idiom
  for the same idea as the HTML drill-down.

- **PaperTrail reports can be scored against a ground truth (`TRUTH`).** A
  column can name the answer it *should* have given, and the report says whether
  it was right:

  ```
  # labels: Pass = pass, ok, low risk, real
  # labels: Fail = fail, reject, high risk, fake

  FOR ROW IN TUPLES FROM "labels.csv"
      REPORT REQUEST classify AS c WITH
          Verdict: jsonpath "$.decision" TRUTH "{{ expected }}"
      END
  END
  ```

  The clause goes wherever `STATISTICS(…)` and `IMAGE(…)` go — on a
  `REPORT … AS …`, on a computed column or on a `WITH` field — and the three may
  be written in any order. The template is interpolated **per row**, because the
  label almost always arrives as a loop binding: a named field of a
  `TUPLES FROM "labels.csv"` manifest, or the folder name a `FOLDERS` loop is
  standing in. Nothing new has to be read from disk — the producers that already
  bring in the files bring in their labels too.

  The optional, repeatable `# labels:` setting declares the vocabulary once, so a
  truth file that says `real` and an engine that answers `Low Risk` match without
  a line of translation in the flow. Without it, values still compare as
  themselves, ignoring case and surrounding space.

  Each scored cell is tinted by whether it is **right** rather than by what it
  says — an engine that correctly answers `fail` is a green cell — and a
  reserved `Correct` column (`correct` / `incorrect` / `untested`) summarises
  each row for CSV, which has no colour, and for sorting in a spreadsheet.

  A ground truth is **data, not an assertion**: a mismatch never fails a run,
  never sets an exit code and never emits a diagnostic — `[Asserts]` is where a
  run says something went wrong. A row whose label is missing or blank reads
  `untested` and is never scored as a pass, so the figures can't be inflated by
  exactly the rows nobody has checked.

  Both editors support it: a **Ground truth** row in the `WITH` field form, and
  one settings row per declared label class with an "add label class" entry
  beside "add helper collection".

- **Ground-truth scores are summarised as accuracy and confusion matrices.** A
  report scored with `TRUTH` now reports how well it did, not just cell by cell:
  a per-column *compared / incorrect / accuracy* summary, plus a confusion
  matrix whenever a `# labels:` setting declares the axis. The figures reach
  every format from one shared model, so no two views can disagree: footer rows
  in CSV and the live grids, a `metrics` object in JSON, metric cards and a
  heat-mapped matrix in HTML, and a second *Metrics* worksheet in xlsx.

  The denominator counts only rows that were actually compared, so adding
  unchecked rows can't inflate an accuracy, and a report that scored nothing
  says so rather than claiming 0%. Values the labels don't declare get their own
  axis entry rather than being dropped, so the matrix always adds up to the
  accuracy printed beside it.

- **A comparison says which way each row moved (`Trend`).** This is the feature
  the ground truth was for: *a change towards the truth is good, a change away
  from it is bad.* A report that has both a `TRUTH` column and a comparison
  (`BASELINE`/`COMPARISON` roles or a `# baseline:` snapshot) grows a reserved
  `Trend` column reading `unchanged`, `fixed`, `regressed` or `still wrong`.

  It is additive: the existing `Result` column still holds the structural diff,
  so a row is routinely `Result: changed` *and* `Trend: fixed` — which is the
  useful reading, not a contradiction. Two rows that changed identically as far
  as `Result` is concerned are told apart by `Trend` alone. The row roll-up
  favours the bad news (one regressed column makes the row a regression), and
  `still wrong` is deliberately distinct from `regressed`: it is failing, but it
  is not *new*.

  Reports with no truth, or no comparison, are unchanged — no column appears.

- **A `SHOW(…)` field can carry its own `STATISTICS(…)`.** Writing
  `SHOW(Time STATISTICS(MEAN, MAX), Status)` attaches summary rows to a column
  where it is named, on both `REPORT REQUEST … SHOW(…)` and
  `BASELINE(…) SHOW(…)`. Previously the only way to add a statistic to a shown
  column was the `# columns:` directive — which is an exhaustive whitelist, so
  naming one column there hid every other one. Baseline statistics apply to
  every `baseline.<alias>.<field>` column the comparison produces.

- **The GUI's File menu opens and saves reports, and Ctrl+S saves.** *Open ▸
  Report* loads a `.trail` file into the editor and lists it beside the session's
  other reports (reopening the same file reuses its tab); *Save ▸ Report* writes
  the source back out and adopts the path. **Ctrl+S** saves whatever is in
  front of you — the open report, otherwise the active collection — writing
  straight to its file when it has one, and only asking where when it doesn't.

- **PaperTrail columns can hold pictures (`IMAGE`).** A column whose value is a
  picture *source* — an image URL the response handed back, a path on disk, a
  `data:` URI or a bare base64 blob — can be marked `IMAGE`, and every export
  that can show a picture draws it instead of the text:

  ```
  REPORT REQUEST face AS f WITH
      Frame: jsonpath "$.best_frame.url" IMAGE(HEIGHT 110)
      Score: jsonpath "$.best_frame.score"
  END
  ```

  The clause goes wherever `STATISTICS(…)` goes — on a `REPORT … AS …`, on a
  computed column, on a `WITH` field, or inline in `# columns:` — and takes
  `HEIGHT n`, `WIDTH n` (either alone scales proportionally, both together fix
  the box) or `FIT` to size to the cell. With no options a picture is drawn
  110 px high.

  `IMAGE` is a **render hint, never a value**: the cell keeps its text. That is
  what keeps the CSV and JSON exports byte-for-byte what they were, keeps
  baseline snapshots textual, and lets a format that can't show pictures fall
  back to the text with no rule of its own. The **xlsx** export embeds the
  picture (widening the column and heightening the row to fit) and keeps the
  source as its alt text; **HTML** inlines it as a `data:` URI so the export
  stays a single self-contained file.

  Where the picture comes from is worked out from the **value's shape**, so one
  clause covers every case and a report never has to declare which kind it
  meant. A value that can't be turned into a picture leaves the cell as text and
  records a note — a broken thumbnail must never fail a report. `IMAGE` columns
  are left out of baseline/`ENVS` comparison, because picture URLs are usually
  signed and expiring and diffing them would flag every row while burying the
  changes that matter. A **dry run** still resolves local paths and `data:`
  URIs, but never fetches a URL: a run that reports "no requests sent" must not
  quietly have made a hundred GETs.

- **PaperTrail `FOLDERS` loops can now walk a nested tree.** `FOR CASE IN
  FOLDERS "inputs" MATCH "**/case_*"` takes the same `MATCH` glob `FILES`
  already took: it filters folder **names**, and recurses through the tree when
  the glob contains `**` (a bare `MATCH "**"` filters nothing and simply visits
  every folder at every depth). Previously `FOLDERS` could only see the
  immediate children of one directory, so an input set laid out as
  `<type>/<batch>/<case>/` — the shape most test corpora actually have — had to
  be flattened by hand before a report could be run over it.

  Because a recursive walk necessarily passes through container folders that
  hold no case files, such a folder is now **skipped** rather than failing the
  run: a recursive walk *searches* a tree for the folders that fit the shape.
  A flat `FOLDERS "cases"` walk still *enumerates* a set you named, so there a
  mis-shaped member remains a loud error, exactly as before.

  In both editors the glob box that `FILES` loops offer is now offered for
  `FOLDERS` loops too, so the way to narrow (or deepen) the walk is visible
  rather than something you have to know to type.

- **`FOLDERS … WITH` roles can be marked optional** with a trailing `?`:
  `WITH front="*_front.*", back="*_back.*"?`. An optional role that matches no
  file binds the empty string instead of failing the run, so a genuinely
  optional input — a document with no back side, a case with no expected-result
  file — no longer forces you to either split the corpus in two or drop the
  role entirely. Matching *more* than one file stays an error whether or not
  the role is optional: PaperTrail will not pick one of two candidates for you,
  because the choice would silently depend on directory order.

- **A report can pull requests from more than one collection.** `# collection:`
  is now repeatable, and every collection after the first must be given a name:

  ```
  # collection: ./face.hurl
  # collection: ./shared/auth.hurl AS auth

  REQUEST auth/login
  REPORT REQUEST verify AS v
  ```

  Requests in a helper are referred to as `alias/request` everywhere a request
  name is accepted. The alias is **required** rather than optional so a bare
  name always means the report's own collection: a name can never quietly
  change which request it runs because a helper was added, and a helper's
  request can never be reached by accident. For the same reason an alias that
  collides with a top-level virtual folder is an error rather than a precedence
  rule — PaperTrail does not pick between two readings of a name.

  Both editors list the helpers in *Report Settings* (add, rename the alias,
  re-point the file, remove — the primary collection stays put), and every
  request picker, completion and validity tint offers and resolves the
  qualified names. A helper that is already open as a tab is read from the tab,
  so unsaved edits are seen; otherwise it is read from disk. A `git:` helper
  must be opened first, since validating a report should not do network I/O.

- **Reports can break a request's `Time` into its parts.** Three new per-request
  intrinsics sit alongside `Time` and always sum to it: `TimeSetup` (DNS, TCP
  connect and the TLS handshake), `TimeWait` (request sent → first response
  byte, the closest thing to "what the server took") and `TimeDownload`
  (receiving the body). They answer the question `Time` alone can't: when a wide
  `PARALLEL(…)` run makes response times climb, was that the server, or was it
  your own machine and uplink getting to it?

  Unlike the other intrinsics these are emitted **only** when a `SHOW(…)` or
  `columns:` clause names them, so no existing report gains columns; the request
  form in the TUI and the GUI lists them un-ticked. Like `Time`, they are
  excluded from a comparison run's compared fields, since they always differ.

- **Reindent a PaperTrail script (`F` in the TUI, *Reindent* in the GUI).** Wrap
  an existing block in a new outer loop and its whole body is suddenly one level
  short; this puts every line back at its real block depth, four spaces a level.

  **Only leading whitespace changes** — comments, blank lines and the spacing
  inside a statement are preserved exactly, so it is safe to run over a file you
  didn't write. (That rules out the obvious implementation of parsing to a flow
  and re-serializing it: the AST has nowhere to keep a body comment, so a
  round-trip through it deletes them.) The result is verified by re-parsing it
  and comparing against the original, so reindenting can never change what a
  report does; a script that doesn't parse is left alone rather than guessed at,
  and it is a single undo step.

- **`WITH` blocks open out in the TUI node editor.** A
  `REPORT REQUEST … WITH … END` used to collapse to a single `… WITH …` row, so
  its fields were invisible from the outline and there was no obvious way to add
  one. Each field is now a row of its own, under the request and above the
  block's `END`, with an `add a field…` row at the end: Enter edits a field,
  Delete removes just that field, Shift+↑/↓ reorders the column within its
  block, and `a` anywhere in the block adds another.

- **Import a whole Postman workspace from inside PaperBoy.** Until now this
  was CLI-only; both front-ends now have a wizard for it. Give it a Postman API
  key, pick a workspace from the list it fetches, choose what to bring across
  and where to put it, and the imported folder opens as a workspace. In the
  terminal it's **File ▸ Load ▸ Workspace ▸ From Postman…**; in the GUI it's
  **Open from Git ▸ From Postman…** (beside the other whole-folder sources).

- **The wizard says what an import will cost before it spends anything.**
  Postman rate-limits its API, so the confirmation step lists what was found,
  explains that the download is paced deliberately, estimates how long it will
  take, and warns when the import would eat an uncomfortable share of the
  account's remaining monthly API budget. Nothing is downloaded until that
  question is answered, so backing out there is free.

- **A live estimate while it runs.** The progress view counts items, names the
  one in flight, and shows a remaining time extrapolated from the rate actually
  being achieved rather than the published one — a throttled account is exactly
  when an estimate is worth having. When the importer is deliberately idle it
  says so, rather than looking hung.

- **A workspace id can be given up front to skip the list.** Paste an id or a
  Postman workspace address into the optional field on the first step and the
  wizard goes straight to the options — that listing call sits on Postman's
  tightest rate-limit bucket, so it isn't made when the answer is already
  known. The workspace's real name is picked up later and used for the folder.

- The wizard offers the same **convert-to-Hurl** option as
  `--postman-format hurl`, so an import can drop Postman's JSON entirely.

- **A Postman import can now be converted to Hurl on the way in.** Pass
  `--postman-format hurl` and collections arrive as `.hurl` files and
  environments as `.vars` files, so the imported folder owes nothing to
  Postman. The default is unchanged: Postman's own JSON, byte for byte.

- **Anything the conversion can't carry across is written down.** Hurl has no
  pre-request scripts and no OAuth 2, so a converted import leaves a
  `CONVERSION-NOTES.md` listing, request by request, exactly what was dropped —
  and no file at all when nothing was. A collection this build can't read is
  kept as its original JSON rather than converted into an empty one, so
  converting can never cost you data.

- **Hovering a block in the visual report editor highlights it — and everything
  that would move with it.** Pointing at a line lights that block, and pointing
  at a `FOR` loop also lights its whole body and its `END`, so what a drag is
  about to pick up is visible before the drag starts rather than only once the
  block is in mid-air.

- **`STATISTICS` can be dropped onto a `WITH` field.** A request's `WITH` fields
  are the columns it actually produces, and the grammar has always allowed a
  summary on one — but the block editor had nowhere to drop it, because a field
  isn't a block. Each field row is now its own drop target, and the request line
  above it explains why it refuses (it names no single column of its own).

- **The desktop app can now save Workspaces and Reports to Git, not just
  collections.** Saving to Git in the desktop app could only ever push a single
  collection to a single branch. It now offers the same choices the terminal
  app has always had: push a whole Workspace folder back to where it came from,
  push an open Report, save the collection's environment alongside it in the
  same commit, and tag a release instead of committing to a branch. The
  Workspace and Report entries appear under *Save to Git* and are only
  selectable when there is something for them to push.

- **Download a whole Postman workspace from the command line.** Moving an
  account into PaperBoy meant exporting each collection by hand, or writing a
  script against the Postman API. `paperboy --postman-import` lists the
  workspaces your API key can see, and
  `paperboy --postman-import --postman-workspace <ID|URL> -o <FOLDER>`
  downloads that workspace's collections and environments into a folder you can
  open as a PaperBoy workspace. The workspace can be named by its id or by its
  address in Postman, so the browser address bar can simply be pasted.

  The key comes from `--postman-key` or `$POSTMAN_API_KEY`, and may be a
  `{{ … }}` provider reference — `--postman-key '{{ op://Private/Postman/credential }}'`
  works exactly as it would in a `.vars` file, so the key need not appear in
  your shell history. It is never written to disk and is stripped from any error
  message.

  Postman rate-limits its API, and does so far more tightly for listing than for
  fetching, so the import paces the two separately and adapts to the limits
  reported in each response — a sixty-collection workspace takes about fifteen
  seconds rather than the seventy a single uniform delay would cost. The run
  says up front how many items it will fetch and roughly how long that will
  take, and warns if it would consume most of what is left of your plan's
  monthly API allowance.

  A collection that has been deleted since the listing is reported and skipped
  rather than ending the run; a rejected key or an exhausted monthly quota stops
  it immediately, because neither improves by being retried. Everything is
  written to a staging folder and moved into place in one step, so an
  interrupted import never leaves a folder that looks like a workspace but is
  missing half its collections, and an existing destination is left alone unless
  `--overwrite` is given. `--postman-what` limits the import to just collections
  or just environments.

- **The Environments panel has a filter box, and shows the open workspace's
  environments.** With an account's worth of environments loaded, finding the
  one you want meant scrolling a list of hundreds. The panel now filters by
  name — `/` in the terminal UI, a box above the list in the GUI — and, when a
  Workspace tab is open, lists every environment file in that workspace
  alongside the global ones, marked with a `⌂` (a folder icon in the GUI) so the
  two can be told apart. Workspace files that haven't been opened yet are listed
  too, dimmed, and open when you select them, so a folder of environments is
  browsable without hunting through the tree for each one. In the terminal UI
  `Esc` clears an applied filter, and Postman `.json` environments are now
  recognised by the Workspace tree, so they appear as environments rather than
  as collections.

- **An environment file in a Workspace tree can be made the active environment
  directly.** Previously it could only be opened, after which it still had to be
  found again in the Environments panel and activated there. Right-clicking one
  now offers "Set as active environment" in the GUI, and in the terminal UI both
  `a` and a right-click on the row do the same — loading the file first if it
  isn't open yet, reusing it if it is, and leaving the screen where it was.

- **A `FOR` loop's variable, folder and file pattern are now edited on the chip
  itself.** The loop head used to be one long label, which gave no hint that the
  folder it reads from or the name it binds were things you could change — both
  were only ever found by opening the wizard. `FOR` is now followed by a box for
  the loop variable, the source keyword, a box and a picker button for the
  folder (or file, for `TUPLES FROM`), and a `FILES` loop's `MATCH` pattern. The
  picker starts in the folder the loop already names and writes what you choose
  back relative to the report, so a report stays portable. Parts that no single
  box could speak for — a destructuring pattern like `FOR (NAME, URL) IN …`, a
  list literal, a `FOLDERS … WITH` role list — are still shown as text and still
  edited through the wizard.

- **Workspaces can be organised into folders from the app.** Right-clicking any
  row in a Workspace tree (or the tree's own `New` menu) now offers "New
  folder…" alongside the three file kinds, so a workspace can be tidied up
  without leaving PaperBoy for a file manager. The new folder is revealed and
  opened ready to have things dragged into it, and its name goes through the
  same containment checks as a new file, so it can't be created outside the
  workspace.

- **"Save all changes" on the unsaved-changes warning.** Quitting with edits
  that really would be lost now offers to write them out rather than only
  offering to discard them. It saves exactly what the warning counts — every
  edited file a Workspace tab is holding, including the ones it isn't currently
  showing — and if a file can't be written the quit is called off and the file
  that refused is named, so nothing is lost to a failed save.

- **The Environments panel can be narrowed by source.** When a Workspace holds
  hundreds of imported environments, hand-made global ones were buried in the
  same list, and switching the name filter back and forth was a poor substitute
  for saying which source you wanted. Both front-ends now offer a compact
  Both/Global/Workspace source toggle that composes with the name filter and
  remembers the choice across restarts.

- **The GUI's top-level menus can be reached from the keyboard.** Pressing `Alt`
  on its own arms the menu bar and underlines each menu's mnemonic letter, which
  then opens that menu; `Alt`+letter as a single chord does the same thing in
  one keystroke. `Esc`, a second `Alt`, or opening a menu puts the underlines
  away again. The mnemonics are translated alongside the menu titles rather than
  derived from them, so each language gets letters that suit its own words
  (`F`/`V`/`S` in English, `F`/`A`/`P` in French, `F`/`V`/`I` in Danish).

### Changed

- **The test suite no longer opens a file chooser.** One GUI test exercises the
  guard that stops a second Browse click opening a rival dialog — and it did so
  by opening a real one, which put a native folder chooser in front of whatever
  its author was doing and took the keyboard with it, with nobody in the test to
  answer it. Under `cfg(test)` a picker (and a native error alert) now opens
  nothing and resolves as a cancel, so the polling and unwind paths stay
  covered without anything reaching the screen.

- **The active environment is always on screen.** The Environments panel is a
  scrolling list, and importing a Postman workspace can put a few hundred
  entries in it — so the one carrying the checkmark was almost never visible,
  and a filter could hide it outright, leaving no way to answer "what am I
  about to run against?" without going to look for it. The panel now pins the
  active environment on a line of its own above the list, in green with its
  checkmark (and its git icon where it came from a remote); clicking that line
  jumps to its row. With nothing active it says so, rather than disappearing
  and shifting the list under the cursor. The GUI already carried this in its
  status bar.

- **A picture column in the HTML report is sized to the picture.** It was sized
  to the text behind the picture — the path (or the base64 blob) the image was
  resolved from — so a column drawing a 60-pixel thumbnail claimed seventy
  characters of width and pushed the columns that carry the answers off the
  screen. It is now measured from the thumbnail the `IMAGE` clause asks for.

- **A wide report is fitted rather than left to sprawl.** Thirty columns each as
  wide as their longest value make a table that is read by scrolling past a lot
  of padding. The columns are now fitted to a budget by taking the width back
  from the widest first, down to a floor a wrapped cell is still readable in;
  every column that was already narrow keeps exactly the width it measured, and
  a table that fits is left untouched.

- **"Comparison matched baseline" is no longer green.** A match says the answer
  did not change, which is neither good nor bad on its own — a row that has been
  wrong since the first run matches its baseline perfectly, and tinting that
  green reported a good run where nothing had happened. The `Result` column now
  tints only its unusual values (no baseline, no candidate, a real difference).
  Whether an answer is *right* is the `Correct` column's job, and whether it
  *improved* is `Trend`'s.

- **The HTML report's drill-down is no longer mostly gaps.** Each section in an
  expanded row claimed an equal share of the row's width, so on a wide screen a
  picture and a short JSON blob were shoved to opposite ends with a void
  between them, and a short section was stretched to the height of the tallest.
  Sections now size to their own content, sit next to each other, and wrap when
  they run out of room — with a reading-width cap so one long block cannot take
  the whole panel.

- **An import lands next to the last one.** The Postman importer suggested a
  folder inside the app's working directory — which, for anyone starting the
  terminal UI from a project, meant downloaded workspaces landing inside that
  repository. It now remembers the folder each import was written into and
  suggests it for the next one (the GUI too, which was defaulting to the home
  directory), so a shelf of downloaded workspaces stays a shelf. The choice is
  kept with the rest of the session state, so it survives a restart.

- **The confirmation screen asks for the key it wants.** "Enter import · Esc
  cancel" sat in the dim footer style used for status text, so the last screen
  before a long download read as though the download were already happening.
  It now says **"Press Enter to start the download"** in the accent, in bold.

- **Left and Right change an option in the Postman importer**, as they do on
  every other two-way choice in the app — the format row draws Hurl and JSON
  side by side, so an arrow pointed at one of them now picks it. The
  destination row still takes arrows as cursor movement, since it is a path
  being typed.

- **A folder picker takes Space for "here, under this name".** Every
  "save into a folder" picker — exporting a report, saving a collection or a
  workspace, choosing where a Postman import lands — opens with the name already
  worked out in its inline field, and then asked you to Tab into that field and
  press Enter to accept it. **Space** now saves into the folder on screen under
  the name already there, and **Enter on a file row** — which previously did
  nothing at all, and so read as a stuck dialog — does the same. Enter on a
  *folder* row still descends into it, and Tab is still there for renaming
  first.

- **The Postman importer's estimate counts a collection and an environment
  separately.** It priced every fetch at the pacing interval alone, which is the
  floor, not the cost: the round trip is what the time actually goes on, and a
  collection — a whole document of requests, scripts and examples — takes far
  longer to come back than an environment's short list of variables. A
  23-collection, 500-environment workspace was quoted at "about 2 minutes" for a
  download that ran for well over ten. The running ETA is now extrapolated per
  kind too, so the minutes-remaining figure measured on the collections at the
  front of the queue is no longer applied to the environments behind them.

- **The Postman importer reads like the rest of the terminal UI.** Its connect
  form spaced three fields out over sixteen rows with blank lines between them
  and clipped the API-key hint mid-word at the panel edge ("it is never wri…").
  The hints now wrap — in every language, however long the translation — each
  field's own accented label is what starts its group, and the padding is gone.
  The confirmation screen no longer borrows the git dialog's "Tab switch field ·
  Enter connect" hint on a screen with no fields and nothing left to connect to;
  it says Enter imports. The download screen gives its progress bar the width of
  the terminal rather than a fixed 74 columns, and draws only the rows it has
  something to put in, instead of reserving empty ones for an ETA that hasn't
  been worked out yet.

- **The `Trend` column is tinted by which way a row moved, and nothing else.**
  It coloured a still-failing row red, which repeated in red exactly what the
  `Correct` cell immediately to its left already said — spending the column's
  colour on a fact the reader had just read, and leaving nothing to mark the
  rows that actually moved. Green now means a row got better, red that it got
  worse, and a row that didn't move is plain in both directions, right or
  wrong.

- **A still-failing row's `Trend` reads `unchanged`.** The column answers one
  question — *did this row move?* — and for a row that was wrong before and is
  wrong now the answer is no, so `still wrong` was answering a different
  question in the column's own words. It now reads `unchanged` like the other
  row that didn't move, with `Correct: incorrect` beside it saying which of the
  two it is, and the cell still tinted red. Nothing else is lost: the tint and
  the row filters read the scored cells rather than the column's text, so a
  still-failing row is never confused with a passing one.

- **The confusion matrix in an HTML export is drawn at readable size.** It was
  set at the grid's own type size, so the handful of numbers a reader scans
  across and down to find the one wrong cell were the smallest thing on the
  page. The matrix now has roughly twice the type size and matching cell
  padding — which is also most of the click target, since every cell filters the
  table below it.

- **The GUI asks for an export filename itself.** Exporting a report's results
  used the desktop's own save dialog, whose format dropdown — tucked in the far
  bottom corner — only *filtered* the listing: picking "Excel" left the name
  ending `.csv`, and since PaperBoy chooses its writer by extension, the file
  came out a CSV. Export now opens PaperBoy's own dialog, with the format
  sitting next to the filename and changing it rewriting the extension, the way
  the terminal UI has always worked. `Browse…` still opens the desktop picker
  for anyone who wants to go looking for a folder.

- **The row drill-down uses the width it has, opens on a click, and resizes.**
  The in-app panel stacked its sections down the pane, so a full-height
  photograph buried the response body a screen below it; it now lays them across
  the pane in readable-width columns, the same layout the interactive HTML
  export's flex row produces. Clicking anywhere in a row that has a drill-down
  now opens it, rather than only the caret doing so — a row with nothing to
  drill into keeps the cell inspector, which is otherwise the only way to read a
  truncated value. The divider between the grid and the panel can be dragged,
  and the height is saved with the rest of the GUI's geometry, so a reader who
  wants a big picture pane keeps it between runs and between sessions.

- **The report editor's header is one row, not three.** The title, the
  Blocks/Source/Results toggle and (in the results view) a near-empty band
  holding Baseline and Export were three stacked strips above a table. They are
  now one line — what the document is, which way you are looking at it, what you
  can do to it — which hands two rows of the window back to the rows being read.
  The run's status moved up beside the Run button that started it.

- **The results view's summary block can be resized.** The metric cards, the
  filter bar and the confusion matrices took whatever height they wanted, so a
  report with a matrix per ground-truthed column left almost nothing for the
  rows the matrices are describing — and opening a drill-down under it left less
  still. The block now scrolls within a height of its own, with a splitter under
  it to set that height, saved with the rest of the GUI's geometry. A report
  with no matrices still reserves no empty space.

- **A drill-down picture's path is legible.** The caption under a full-size
  photograph was set at the small text style, leaving the one line that says
  *which* file of a thousand this row is as a fine grey thread. It is now body
  size and wraps rather than being cut off.

- **A confusion-matrix cell is clickable across the whole cell.** The click
  sense sat on the digits rather than on the block of colour around them, so
  selecting the rows a cell counted meant hitting a target the width of the
  number — a "7" was a few pixels wide in the middle of something that looked
  like a button. The whole cell now takes the click, and shows the pointing-hand
  cursor to say so.

- **The in-app confusion matrix is drawn large enough to read.** The counts and
  their axis labels were set at the small text style, so the matrix — a thing
  read by comparing one cell against another at a glance — came out as a grid of
  little grey numbers. The cells are now uniformly wide, evenly padded and set
  above body size, with the counts centred in their block of colour. The sizes
  are derived from the app's body text height, so the matrix still follows the
  text scale rather than being pinned to a fixed pixel size.

- **A report embeds each picture once, downscaled.** `IMAGE(HEIGHT 110)` used
  to be a sizing *hint* only: the full-resolution source file was embedded
  regardless, and the interactive HTML export then embedded a second copy of the
  same bytes for the drill-down panel. A thousand-row report over 2 MB
  photographs produced a file measured in gigabytes to show pictures 110 pixels
  tall.

  Pictures are now re-encoded to at most 640 pixels on their longest edge when
  resolved, which is sized for the drill-down view — the grid scales the same
  copy down in CSS, and the HTML panel now borrows its picture from the row's
  own cell instead of carrying a duplicate. A picture already within the cap,
  or in a format this build has no encoder for, is still embedded exactly as it
  was: unshrinkable must never mean unusable.

- **A label class is edited as two fields in the GUI's report settings.**
  `# labels:` used to be a single text box, so declaring a vocabulary meant
  remembering both the `=` and the comma rules. The class is now split around
  its `=` — the label on the left, the spellings that mean it on the right —
  the same way a helper collection is split around its `AS`, with example hints
  (`Low Risk` / `real, genuine, pass`) rather than descriptions.

- **The GUI's "Report settings" title is a heading**, the same size every
  wizard panel titles itself with, rather than small caption text.

- **Summary statistics are behind a toggle in the TUI's `WITH` field editor.**
  The six `STATISTICS(…)` choices used to be listed unconditionally, which made
  the wizard mostly a wall of checkboxes for the common case of a plain column.
  There is now a *Summary statistics* switch, and the choices only appear while
  it is on. Turning it on seeds `COUNT` so the clause is never empty, and
  turning it off clears the ticks — a hidden list can't leave a clause behind.

- **A hovered tile in the GUI block editor recolours itself.** The hover
  highlight used to be a band painted *behind* the row, which was easy to miss
  on a densely packed row. The tiles that a drag would carry now deepen their
  own fill and outline as well, so what is about to move is obvious before the
  mouse button goes down. The tile keeps its own colour — only the intensity
  changes — and the label is untouched.

- **The GUI's *Report Settings* panel fills the pane and says what it is.** It
  was laid out in a fixed-width box with a wide empty margin to its right, and
  carried no heading to explain what the directives above the report were.

- **The GUI's last hardcoded display strings now come from the translation
  table**, so French and Danish get them too: the language names in Settings,
  the Raw Request view's JSON / Hurl toggle, the URL, assertion and file-path
  field hints, and the name a new report is created with.
- **A custom HTTP verb's badge follows the theme.** Methods PaperBoy has no
  colour for fell back to a fixed grey, which was the only colour in the GUI
  that ignored the active theme; they now use the theme's dim colour.

- **The Postman wizard's destination is now chosen in the file browser**, in
  the terminal UI, instead of being typed as a path. It was the only place in
  PaperBoy that asked you to know a folder path before you had seen one; it now
  opens the same picker as every other "save into a folder", seeded at the
  suggested destination and offering the folder name back for editing.
  Cancelling the picker leaves the wizard exactly as it was. (The GUI already
  used the native folder picker.)

- The Postman import's state machine is now **shared between the terminal UI
  and the GUI**, joining the git load and save flows. Every decision about what
  an import does — validation, ordering, pacing, cancellation, what counts as a
  failure — lives in one place, so the two front-ends cannot drift apart.

- **The visual report editor's Send button is gone from the menu bar.** It ran
  whatever request was selected in the active collection — the same thing the
  Send button beside the URL does — but it was drawn on every screen, including
  ones with no request in sight, so it could fire at something off-screen. The
  keyboard shortcuts it advertised now appear on the real Send button.

- **Key/value tables give their Description column room to be read.** The key
  and value columns between them claimed the entire width, leaving the note a
  sliver a word wouldn't fit in; all three now take a share. The column titles
  also sit where the fields below them will be even when the table is empty, so
  adding the first row no longer makes the headings jump apart.

- **The workspace tree and the results grid are no longer rebuilt on every
  frame.** Drawing the workspace list re-read the whole folder tree off disk —
  a recursive directory scan sixty times a second, so simply moving the mouse
  over a workspace was continuous filesystem I/O. Drawing a results grid
  re-measured every cell in the table to size its columns, and computed the
  `STATISTICS` summary rows once *per column* while doing it. Both now reuse
  their last answer until something they depend on actually changes: about 8×
  less work for the tree and 6× for the grid, and more on a large workspace or
  a long report.

  The tree is re-read whenever PaperBoy itself creates, moves or deletes a
  file, and otherwise at most a few times a second, so a change made outside
  PaperBoy (another editor, a `git pull`) still appears without a refresh.

- **Syntax highlighting is no longer recomputed on every frame.** Both the
  PaperTrail source editor and the request Code view re-ran their highlighter
  over the entire buffer each frame — every keyword, `{{ VAR }}` and error line
  re-classified sixty times a second for text that hadn't changed. Each now
  keeps its last colouring and rebuilds only when something it depends on
  moves: the text, the error line, the loaded environments, the request names,
  the theme, the font size or the wrap width.

  The Code view was additionally running a *second*, complete highlighting pass
  purely to find out which kinds of substitution appeared in the buffer, so it
  could label the legend beneath it — and threw the coloured result away. That
  pass is now a plain scan that only answers that question.

- **Every terminal-UI file picker can be filtered by typing.** Only the three
  "open an existing collection / environment / report" pickers took a typed
  filter; the folder pickers did not — including the one that chooses the
  source folder for a `FOR … IN FILES/FOLDERS` loop, which is browsed against a
  real corpus tree and is exactly where sifting a crowded directory matters. All
  of them now narrow as you type, with the same `Filter:` strip showing what is
  being matched.

  Two keys keep their existing meaning: in a "save to folder" picker typing only
  filters while the *list* has focus (Tab moves to the filename field, where the
  same keys type a name), and in the three folder pickers `Space` still confirms
  the current directory rather than entering a space.

- **Both apps now run the same saving logic.** Loading from Git was unified in
  the previous release; saving now follows, so a fix or an improvement to
  saving reaches the terminal and the desktop at the same time instead of one
  of them drifting behind.

- **Both front-ends now drive one shared "load from a git remote" flow.** The
  terminal UI and the GUI each had their own copy of the wizard, and the copies
  had drifted. The steps, the background work and the recorded provenance now
  live in one place, so a fix or an improvement to the flow reaches both.

- **The report editor has been restyled to look like a working tool.** The
  drag-and-drop view is the part of PaperBoy used by people who don't otherwise
  write code, and several of them reported it looked childish enough that they
  were reluctant to show it to colleagues. Nothing about how it works has
  changed — no gesture, command or layout is different — but six things about
  how it looks are:

  - A block's category is now carried by a colour bar down its leading edge
    rather than by the fill of the whole block. The hues are unchanged and just
    as easy to tell apart, but a flow of ten blocks is no longer ten filled
    panels of colour.
  - Chip and block corners are much less rounded, and spacing and control
    padding are tighter throughout the GUI, which fits more of a report on
    screen.
  - Chip labels are now set as two kinds of word: the editor's own keywords in
    the interface font, and names you supplied in a monospace one, so
    `BASELINE(staging)` no longer reads as a single phrase. Inline fields are
    monospace for the same reason.
  - Chip label contrast has been fixed. Category-coloured text on a tint of the
    same colour fell as low as 2.3:1 in places, which is below the accessibility
    floor for body text; every category on every built-in theme now passes WCAG
    AA, and there is a test to keep it that way.
  - Icons throughout the GUI are drawn in a lighter weight so they stop
    competing with the labels beside them.
  - The synthetic row at the top of a flow is drawn as a caption rather than in
    a keyword's colour. It marks where the report starts and, unlike the `END`
    that closes a loop, is not something that appears in the file — colouring it
    like syntax implied there was a `BEGIN` keyword to write, and there isn't.

- **PaperBoy now opens on a neutral dark theme.** The three language themes are
  saturated flag colours with a bright yellow selection, which look striking and
  read as unserious in an office. A new "Graphite" preset — a near-neutral grey
  ground with a single restrained blue accent — is what a fresh install starts
  on. The language themes remain, "Follow language" remains a choice you can
  make, and an existing install keeps whatever theme it already had.

- **The Workspace filter no longer hides folders.** With the filter on, a folder
  containing nothing it matched was left out of the tree entirely, which made
  the tree impossible to organise with: a folder created to tidy files into
  disappeared the moment it was made, and there was nowhere to drop the first
  one. The filter still applies to files, which is what it was for.

- **A clause that qualifies another chip is now drawn as one pill with it.** In
  the report editor's drag-and-drop view, `SHOW(…)` belongs to the `BASELINE`
  before it, and `STATISTICS(…)` to the column before it, but the thin bracket
  that said so was drawn beside the chips and read as decoration — it never made
  clear which of the two owned the other. The pair now sits flush inside a single
  outline with the meeting corners squared off, the way a segmented control does,
  and hovering either half outlines both. Each chip keeps the colour its own kind
  always has, so a `SHOW` still reads as a `SHOW`.

- **The two front-ends now share one copy of the application state.** The
  terminal UI kept its own duplicate of everything it had in common with the
  graphical one — the open tabs, the global environments, the themes and every
  persisted preference: 26 fields, plus a hand-written second copy of the code
  that reads and writes `state.json`. The two could drift, and had: a default
  changed in one place stayed unchanged in the other, and the terminal UI
  carried a field of the GUI's window geometry that nothing there ever read,
  purely so saving from the terminal didn't wipe it. `TuiApp` now owns a
  `Session` and reads straight through it, so there is a single copy in the
  process and a single writer of the state file. Nothing about either front-end
  behaves differently, with one exception noted below.

- **A Workspace whose folder has vanished now says so in the GUI too.** The
  explanation ("the folder for this Workspace is gone") was written when the
  terminal UI restored a session and was lost when the graphical one did, which
  left an empty tab and no reason for it. Both front-ends now restore state
  through the same code, so both report it.

- **The GUI is now behind a `gui` Cargo feature, and is no longer built by
  default.** `cargo install paperboy` builds only the terminal UI and the
  headless runner; `cargo install paperboy --features gui` adds the graphical
  one. eframe/winit/wgpu more than doubled the dependency tree (225 crates to
  482) and dominated the build, which is a poor trade for anyone who only wants
  PaperBoy in a terminal. Nothing else changes: the saved-state format is
  identical, so a terminal-only build still round-trips a GUI user's window and
  panel geometry, and `--gui` on a build without the feature prints the command
  to install one that has it rather than failing to parse the flag.

### Added

- **Revert a workspace file, or one request, from the tree.** Right-clicking a
  row that carries the "edited" pencil offers to put it back the way it is on
  disk: a request row reverts that request, a collection file row reverts every
  edit in the file — including one edited and then switched away from, whose
  changes were until now only reachable by reopening it. The GUI hangs this on
  the row's context menu; a terminal has nowhere to put one, so the gesture
  raises the same confirmation `Ctrl+R` does. A clean row offers nothing rather
  than a greyed-out entry.

### Fixed

- **A report line that runs off the edge now says so.** The terminal UI's
  report Source view clips long lines rather than wrapping them (they are code,
  and wrapping would break the 1:1 relationship between a row on screen and a
  line in the file), but it clipped them silently — the only way to discover
  that something had been cut off was to enter edit mode and walk the cursor
  along the line. A clipped row now ends in the same dim `…` the wizard's
  truncated cells use. The results grid's rows, header included, are marked the
  same way when columns run past the right edge; a row of trailing padding
  doesn't count as something you're missing.

- **Every other long list scrolled the way the workspace tree used to.** The
  tree was fixed to let the cursor travel through the visible rows; the same
  defect (a list rebuilt from scratch each frame, so it always scrolled the
  minimum needed to reveal the selection and pinned it to the bottom edge) was
  in nine more lists: the Global Environments panel, the environment popup's
  variables, the workspace file picker, the git branch/tag and file pickers,
  the Postman workspace picker, the theme list and the new-theme base list, and
  the wizard's header-name and content-type dropdowns. All ten now share one
  remembered scroll position, which also keeps their mouse hit-testing honest —
  a click lands on the row it was drawn on rather than on where the old
  formula assumed the list had scrolled to. Lists that can't overflow their
  popup (a three-row dropdown, a menu sized to its own items) are unchanged.

- **Three panels stopped redoing the same work on every frame.** The Global
  Environments panel rescanned the whole workspace folder from disk several
  times a frame; it now reads the workspace scan cache both front-ends already
  share (and the terminal UI, which did the same, reads it too). The git load
  wizard re-filtered and re-lowercased every path in the remote repository on
  every frame of the file picker; the filtered list is now kept until the
  filter, the selection or the file list itself changes — keyed on an explicit
  generation counter rather than on anything derived from the list's contents,
  because two different repositories can hold the same number of files. And the
  report editor's Source view rebuilt its highlighter's set of known request
  names — a formatted string per request, per helper — on every frame; it is
  now rebuilt only when a name in it actually changes.

- **The workspace tree scrolled with the cursor instead of the cursor moving
  through the tree.** Walking up a long tree kept the selected row pinned to the
  bottom edge of the panel while everything else slid past it. The list's scroll
  position now persists between frames, so the cursor travels through the
  visible rows and the tree only scrolls once the cursor reaches an edge — what
  every other list does. Switching tabs still starts at the top: another tree's
  scroll position means nothing in this one.

- **Saving an edited request threw the workspace selection to the top.** The two
  lists index differently — a Workspace tab's cursor walks the file tree, an
  ordinary tab's walks the requests — and committing the request wizard wrote a
  requests-list index into the workspace's cursor. The selection now stays on
  the request that was just saved.

- **A workspace report wouldn't run from the tree.** With a report showing in
  the right pane, `r` and `d` did nothing unless focus had first been moved
  into the report body — even though the tree's selection *was* that report.
  Both keys now run and preview it from either pane. Only those two: every
  other report key already means something to the tree, and a letter that
  changes meaning with the right pane's contents is how a key map becomes
  unlearnable.

- **A spreadsheet's picture column was as wide as a file path.** A column of
  thumbnails was sized to its pictures only when they had a fixed height and
  had all been fetched; a `FIT` column, or one whose pictures failed to load,
  fell back to the text underneath them — which in a real run is a hundred
  characters of directory nobody reads, leaving the rest of the report pushed
  off the screen. Such a column now gets a modest fixed width, the same rule
  the HTML export follows.

- **An import that stopped on a full folder offered to run again unchanged.**
  Nobody presses a key while a download runs, so the failure was blamed on the
  last screen that *had* seen one — the confirmation — and dismissing "already
  exists and is not empty" put you back on "start the import", pointed at the
  same occupied folder. A failure is now attributed to the screen actually on
  display, so it returns to the options, with the destination still filled in,
  ready to be pointed somewhere else or told to overwrite.

- **A converted Postman collection could arrive unreadable, and say nothing.**
  Two things Postman exports quite normally came out the other side as Hurl that
  Hurl cannot parse: its **dynamic variables** (`{{$guid}}`, `{{$timestamp}}` —
  a `$` is not a legal template name) and a **file part with no file chosen**
  (`key: file,;`). Either one failed the *whole* file, so a collection of sixty
  requests opened as none, with nothing on disk to suggest why — the same
  workspace imported as raw JSON was fine. Dynamic variables are now renamed to
  ordinary ones (`{{$guid}}` → `{{guid}}`, so the value has to be supplied, and
  the conversion notes say so), a file part with no file is written switched off
  so it can be filled in later, and — as a backstop for anything else — a
  conversion whose output does not read back keeps the original JSON, which
  always opens, and records the parse error against the collection's name.

- **Dismissing a Postman import error goes back to what can be fixed.** A
  rejected API key fails while the workspaces are being listed, so the step it
  interrupted was "choose a workspace" — of a list that was never fetched.
  Pressing Esc dropped the user on an empty picker with nothing to pick, no way
  forward and no way back. It now returns to the key prompt, with the key still
  in the field to correct; a failure with no workspace chosen goes back the same
  way, and a failed download returns to the options it was started from rather
  than a progress bar that cannot be resumed.

- **A results row shows the pointing hand across its whole width.** The row was
  clickable end to end but only looked it over the words in a cell, because the
  cells take the hover from the row target wherever they overlap it. The cursor
  now follows the row, not its text.

- **An export offers the report's name from the folder the report works in.** A
  saved report already exported beside itself; an unsaved one offered a bare
  filename, which would have landed wherever PaperBoy was started from rather
  than in the report's `# root:` directory the terminal UI's picker opens in.

- **A whole results row opens its details, not just the words in it.** The click
  target for expanding a row was the *text* of a cell, so the gaps between
  columns, and the empty space in a short column, did nothing — a row of mostly
  blank cells was nearly unclickable. The row itself is now the target; the
  cells still win where they overlap, so each keeps its own hover text and its
  value inspector.

- **A `FOR` loop can be picked up and dragged.** The loop head's only drag
  handle was the word `FOR` itself, making a loop the one block in the editor
  you had to hit a three-letter target to move. Every fixed word on the head —
  `FOR`, `IN FILES`/`IN FOLDERS`/`IN ENVS`, `MATCH` and the tail — is now a
  handle, while the boxes between them are not, so a click meant for a field
  still never starts a drag. Clicking a loop head also selects it (and
  double-clicking opens its wizard), which it never did before.

- **A column's clauses no longer look merged into the column.** `STATISTICS`,
  `IMAGE`, `TRUTH` and `DETAIL` are drawn tethered to the column they belong to,
  as one pill split into segments — but with nothing marking the splits, a
  column carrying all four came out as a single long blob of text. Each segment
  now has a visible seam down its leading edge, so the pill reads as the
  segmented control it is meant to be.

- **Hovering a chip highlights what a drag would actually pick up.** Resting the
  pointer anywhere on a line lit the whole block, including when the pointer was
  on a clause that a plain drag would pull out on its own — the highlight
  promised to move the line and letting go would have detached one chip. A
  detachable chip now outlines itself, and holding Ctrl (which is what turns the
  same gesture into a whole-line drag) hands the highlight back to the block.

- **A Workspace tab comes back to the report it was on.** A report opened from
  a Workspace folder tree belongs to that tab, so it is closed when you leave —
  but nothing reopened it when you came back, and the tab returned showing
  whichever collection its tree had loaded last. Switching tabs now restores the
  tab you arrive at from the same selection a restart restores from, so a report
  you glance away from is still there when you look back.

- **File > Save writes straight to the file, and Save As asks.** The File menu
  offered only the asking kind, so saving a report you had just edited meant
  walking through a file dialog to name the file it already had. There is now a
  top-level **Save** (Ctrl+S) that writes whatever is in front of you — the open
  report, otherwise the active collection — back where it came from, and a
  **Save As…** submenu (Ctrl+Shift+S) holding the pickers that were there
  before. Something that has never been saved still falls through to the picker,
  since there is no file to write to. Ctrl+S and the menu entry now run the same
  code, so they cannot disagree about what "save" means.

- **The block editor draws a column's `IMAGE`, `TRUTH` and `DETAIL` clauses.**
  Only `STATISTICS` was rendered, so a ground-truthed or picture-bearing column
  looked identical to a plain one and the clauses could be edited but never
  seen. Named columns now carry a chip per clause (detachable, like the
  statistics chip), and a `WITH` field shows its clauses in its own row, where
  its `STATISTICS(…)` already was.

- **A plain HTML report no longer shows a lone "All" filter button.** The row
  filters are offered only when a report actually has something to filter by (a
  baseline to differ from, a ground truth to be wrong against, or a trend to
  regress in); without one of those, "All" was a button whose only possible
  effect was the state it was already in. The Find box and the row count are
  unaffected — those are useful in every report.

- **The Open and Save dialogs no longer freeze the window.** Every native file
  picker was called straight from the frame loop and blocked it until the user
  chose a file, so the window stopped repainting and the desktop offered to
  force-quit the "not responding" application. It also stalled every other
  per-frame poll — including the one that collects a finished report run, which
  is why exporting results could report there was nothing to export.

  Every dialog now runs on a worker thread and is collected when it answers:
  File > Open and File > Save, the report editor's `root:` / `baseline:` /
  `collection:` settings and its loop folder pickers, the `FILES`/`FOLDERS`
  node wizard, Form and Multipart file values, the workspace's New Collection /
  Report / Environment, the Postman import destination, and the git workspace
  storage prompt.

  Because the window now stays live while a chooser is open, pressing Browse a
  second time no longer opens a rival dialog, and a path that arrives for a row
  which has since been deleted (or a collection that has been closed) is
  dropped rather than written somewhere it no longer belongs.

- **Live summary statistics no longer count rows that haven't run yet.** The
  results grid fills in a skeleton row per pending request, whose `Time` and
  `Status` are numerically zero rather than empty — so a `STATISTICS(MEAN)` on
  `Time` was dragged toward zero and only became correct once the last row
  landed. Pending rows are now excluded from every summary figure, so a running
  report's statistics describe the rows it has actually finished.

- **Ctrl+Z in the GUI report source editor really undoes.** The edit appeared to
  flash away and the cursor jumped, but the text came back: the editor ran its
  own undo stack over the same buffer as egui's `TextEdit`, whose built-in
  undoer (which can't be turned off) wrote its history back within the frame.
  The source view now uses egui's undo alone; the editor's own stack still
  serves the structural blocks view, where there is no `TextEdit` to argue with.

- `FOR ROW IN TUPLES FROM "manifest.csv"` — the documented way to read a
  manifest's columns by name — no longer reports a destructuring mismatch. It
  put one error on the run for every row of the manifest, which was the loudest
  possible complaint about the usage the cookbook recommends. A pattern that
  really does destructure is still checked.

- **A commented-out column inside a `WITH` block is no longer destroyed by the
  editors.** Comments survived everywhere else in a report, but a `WITH` block
  had nowhere in the syntax tree to keep one, so commenting a column out and
  then touching that request from either editor silently deleted the line. A
  commented `WITH` field is now kept verbatim, in place, and shown dimmed in the
  outline so it can be found again to uncomment. It is not offered to the field
  editor, since there is no field there to edit.

- **A renamed timing column no longer makes every row of a comparison
  differ.** Timing intrinsics were excluded from baseline and `ENVS`
  comparisons by *column name*, so `"Response Time": Time STATISTICS(MEAN)` in
  a `WITH` block produced a column whose name is not an intrinsic and was
  compared — and an elapsed time never repeats, so every row read as changed
  and buried the differences that mattered. Timing columns are now recorded by
  **provenance** when the report runs: `Time`, `TimeSetup`, `TimeWait` and
  `TimeDownload` are left out of the diff under any name. Non-timing intrinsics
  are unaffected — a renamed `Status` still shows when it changes.

- **The TUI `WITH` field wizard closes back to where it was opened from.**
  Pressing Enter or Escape in the wizard reached from *+ add a field* in the
  outline dropped you into the full request form, which then had to be closed
  as well. It also left the cursor on the request line rather than the field
  that had just been edited.

- **HTML exports no longer squeeze their columns.** The table was laid out at
  `width:100%`, so a browser narrowed every column to fit the page and then
  broke text inside words — an `Environment` header came out as "Enviro" /
  "ment" with every value under it wrapped. Columns are now sized to their
  content the same way the xlsx export sizes them (clamped to a readable range,
  so one enormous JSON body can't push everything else off the page), headers
  are never broken mid-word, and a wide report scrolls sideways instead of
  being crushed.

- **Comments in a report body are no longer deleted.** A `#` line among the
  statements was treated as whitespace and thrown away when the script was
  parsed, so the moment you touched the report in either node editor — which
  re-writes the script from the parsed flow after every edit — the comment was
  gone. Commenting a block out to disable it therefore destroyed it, with no
  way to get it back. Comments are now part of the flow, keep their place among
  the statements, are shown as their own (dimmed) row in the outline so they can
  be found and uncommented again, and round-trip byte for byte — including the
  indentation inside a commented-out block. Comments *inside* a `WITH` block are
  still dropped.

- **The TUI's report node editor can now edit the report's settings.** The
  outline gained a Settings section above the flow, listing every header
  directive the language has — `collection`, `output`, `environment`, `root`,
  `baseline` and `columns` — driven by the same four keys as the flow below it
  (Enter configure, `e` edit as text, Delete clear, `a` add). Previously only
  the collection could be bound from the outline and everything else meant
  dropping back to the raw source. The directive table now lives in the shared
  core rather than in the GUI, so the two editors can't drift apart again.

- **The report Validation panel was stuck one row tall.** It was sized by
  counting diagnostics, but a parse error is a single diagnostic a whole
  sentence long — so in the one state where you most need to read the message it
  got one row and clipped the rest. The panel is now sized to the text it
  actually draws, wrapping included, and capped at five rows so it can't take
  over the editor above it. Anything past the cap still scrolls.

- **The `../` row ignored the file browser's filter.** Typing to narrow a picker
  left the parent entry pinned to the top of the results — the one row that
  never matched what you'd asked for. It is now filtered on the name it shows,
  so `..` or `.` keeps it and anything else drops it. A query that matches
  nothing at all draws a "No matching files" box that keeps the folder name and
  the key hints, and Backspace, Esc and ← still get you out of it.

- **Typing in a report block and pressing Run threw the typing away.** The
  header's buttons acted before the blocks below them were redrawn, so an
  inline field that had not yet been committed — it commits when it loses
  focus, which is the very click that pressed the button — never got the
  chance: Run ran the *previous* version of the report and the edit was lost
  with it. Run, Dry run, Save and Close now act at the end of the frame that
  pressed them, on what is actually on screen.
- **Deleting or moving a block while typing in another one could rename the
  wrong block.** The same collision inside the Blocks view: the field's commit
  was applied *after* the structural edit, so it landed on whichever block had
  shuffled into that position — renaming a block the user never touched, and
  losing the edit they had actually made. Field commits are now applied first,
  while the positions they were written against still hold, and the toolbar's
  delete/move act on the block that was highlighted when the button was
  pressed.
- **Clicking the `SHOW(…)` on an ENVS comparison loop did nothing.** A report
  request's `SHOW` opens the field checklist when clicked; the baseline's own
  `SHOW` — whose checklist lives in the ENVS form — did not, leaving no way to
  reach it by clicking the thing it belongs to.

- **Saving a workspace or a report to Git said "Save collection to Git"**. The
  dialog had one hardcoded title for all three save targets, so two of the
  three announced themselves as something they weren't. Each target now titles
  its own dialog.
- **A dialog could crash the GUI instead of asking its question.** The shared
  modal shell unwrapped egui's "did the window draw?" result twice; a frame in
  which the window wasn't drawn panicked. An undrawn frame is now simply not an
  answer — the dialog stays open and asks again.

- **The hint under the Postman import options said "Enter connect"**, which was
  true on none of the rows it appeared over — Enter actually chose a folder,
  flipped a toggle, or started the import depending on where the cursor was.
  The hint now describes the row the cursor is on.

- **A file browser filter no longer makes the folder you open look empty.** The
  typed filter stayed applied when you moved to another directory, so descending
  into a folder you had just filtered *to* showed nothing but `../` — the query
  that matched the folder's name rarely matches anything inside it. The filter
  now belongs to the directory it was typed in and clears when you arrive
  somewhere new.

- **The `FOR … IN FILES` source-folder picker no longer says it is choosing a
  Workspace.** It borrowed the Workspace picker's hint line wholesale, so it
  offered "Space choose as Workspace" while actually setting a loop's source
  folder.

- **Stopping a report in the visual editor stops it now, not eventually.** Stop
  raised the worker's cancel flag but kept hold of the run, so the button stayed
  a Stop button — and the report stayed unrunnable — until the worker actually
  wound down. Cancelling never aborts a request already in flight, and a
  `PARALLEL` batch has a lot of them, so the wait could be a long one. The run
  is now retired the moment you stop it and the next click starts a fresh one,
  matching the terminal UI. Rows that had already come back are kept, so you can
  still read, save or export a partial result.

- **The report validation panel no longer flickers when the mouse moves.** Two
  separate causes: the warnings were re-derived on every frame (a full
  revalidation, dozens of times a second, even though nothing had changed), and
  the per-request variable warnings were emitted straight out of a hash set, so
  each rebuild shuffled them into a different order. Validation is now re-run
  only when the report, the requests it uses or the loaded environments actually
  change, and those warnings come out in alphabetical order.

- **A running report is no longer cancelled by clicking a tab.** The report
  editor is closed and rebuilt whenever you navigate — click a tab, open a
  request, pick another file — and closing it dropped the run with it, which
  cancelled the worker mid-flight and threw away every row it had already
  collected. Runs now live alongside the editor rather than inside it: they keep
  going while you are elsewhere, keep collecting rows, and are still there
  (still streaming, if they haven't finished) when you come back. Two reports
  can be in flight at once.

- **`ENVS BASELINE(…) SHOW(Time)` now actually shows the baseline field.** The
  clause is meant to put the baseline's value beside the candidate's as
  `baseline.<request>.<field>`, but the copy could only see fields the rows
  already carried — and an intrinsic like `Time` is suppressed on any request
  that declares its own `[Reports]`/`WITH` fields. The result was that the
  documented example produced no baseline column at all, in the results grid or
  in any export. Naming an intrinsic in a baseline `SHOW(…)` now keeps it on the
  rows, exactly as naming it in the request's own `SHOW(…)` does.

- **The dry-run preview now shows its grid the way the results view does.**
  The preview wrapped every line, so a wide grid folded over several lines and
  lost its column alignment. Only the prose around the grid wraps now; the grid
  clips, and `←`/`→` scroll it sideways.

- **The report results grid now scrolls sideways.** A report with more columns
  than fit on screen clipped the rest away with no way to reach them; the
  arrow keys now carry the view along with the cell cursor, so walking right
  brings the remaining columns into view (and walking back returns).

- **Clicking the Request or Response panel in the terminal UI now selects it.**
  Those two panels — and the report view's — were the only ones a click didn't
  focus, so they could only be reached from the keyboard.

- **A click no longer overwrites your clipboard.** Releasing the mouse used to
  copy unconditionally, and with nothing selected that meant copying the whole
  panel, so simply clicking on a response replaced whatever you had copied
  earlier. Dragging out a selection still copies it on release, and `y` still
  copies the whole focused panel when nothing is selected.

- **Imported Postman requests now carry the auth they inherit.** A collection
  or folder that sets its auth once at the top applies it to every request
  underneath, which is how nearly every real collection is organised. PaperBoy
  was only reading auth written on the request itself, so such a collection
  imported as a set of unauthenticated requests. A request that opts out with
  Postman's "No Auth" is respected too.

- **API-key auth is now imported.** Previously only basic and bearer auth came
  across; an API key sent as a header or a query parameter is now mapped as
  well.

- **Collection-level variables are no longer dropped on import.** A Postman
  collection can define its own `{{base}}`-style defaults, without which its
  URLs don't resolve. Converting to Hurl now writes them out as an environment
  you can select.

- **The GUI's report source editor now indents like the terminal one.** Pressing
  Enter carried the caret back to column 0, so every line of a `FOR` or `WITH`
  body had to be re-indented by hand. A new line now inherits the current line's
  indentation and gains a level after a block opener, and a line that becomes
  `END` snaps back to the indent of the block it closes. Splitting a line in the
  middle is still a plain newline, and Shift+Enter remains an escape hatch. Both
  front-ends now share one set of indentation rules.

- **Tab in the GUI report source editor now indents four spaces.** It inserted a
  literal tab character, which nothing else in PaperBoy writes — so a report
  saved from the source view was indented inconsistently with the same report
  serialised by the block editor. Shift+Tab and Backspace clear a level in the
  same four-column steps the terminal editor uses. Tab does not leave the field
  (press Escape for that).

- **The GUI now exports report results in the format the report asks for.** Its
  save dialog always defaulted to CSV, ignoring the `# output:` directive, so a
  report declaring `# output: xlsx` had to have its extension retyped by hand
  every time — while the terminal UI had offered the declared format as the
  default all along. The dialog now leads with that format and suggests the same
  filename the terminal UI does, `{time}` token included, with the other three
  formats still one click away.

- **The GUI can now save a run as a `.baseline` snapshot.** Only the terminal UI
  could, so producing the file that `BASELINE(FILE(…))` and the `# baseline:`
  directive read meant switching front-ends. A Baseline button now sits beside
  Export in the results view, suggesting the same `<report>.baseline` name.

- **A rejected file path no longer sends the desktop app back to the start.**
  Entering a path that would write outside the repository now reports the
  problem on the path step itself, so it can be corrected in place.

- **The terminal app now refuses file paths that escape the repository.** This
  check existed only in the desktop app; a path like `../elsewhere.hurl` is now
  rejected in both.

- **Saving to Git from the desktop app no longer fails on a collection that was
  never loaded from Git.** It now starts from a suggested file name and the
  repository's default branch, as the terminal app does.

- **Exported spreadsheets no longer arrive as a row of tiny columns.** Every
  column in an `.xlsx` export was left at the spreadsheet's default width
  regardless of what it held, so a URL or a response body became a tall, narrow
  ribbon of wrapped text while the same run's HTML export looked fine. Columns
  are now sized to their widest content — header, cells and statistics rows
  alike — with a floor so short columns stay readable and a ceiling so one very
  long value can't push everything else off the screen. Anything past the
  ceiling still wraps, as before, so nothing is hidden. The header row is also
  frozen and given a filter, so a long run can be scrolled and sifted without
  losing track of which column is which.

- **A report that reuses a saved `.baseline` snapshot now warns before it
  runs.** Referring to a snapshot file that isn't there — a renamed file, or a
  typo — was the one baseline reference that got no advance check, so the
  problem only appeared as an unmatched comparison once the whole run had
  finished. The path is now checked alongside every other reference, once the
  report has been saved somewhere for a relative path to resolve against.

- **Copying no longer leaves a trail of unkillable processes behind.** Every
  copy — from the Request, Response and Reports panels alike — stranded a
  clipboard helper that was never cleaned up, so a long session accumulated
  dozens of them in the system's process and application lists. They could not
  be closed, because a process in that state can only be cleared by the program
  that started it. PaperBoy now clears each helper as soon as it has done its
  job.

- **Copying no longer flashes an entry into the desktop's application bar.**
  On GNOME and other desktops that lack the newer clipboard protocol, the
  Wayland helper has to open a real window to take ownership of the clipboard,
  which the desktop then shows as a running application for as long as the copy
  survives. PaperBoy now prefers the X11 helper where one is reachable, which
  needs no window at all and still reaches Wayland applications through the
  usual clipboard bridge. Set `TUI_PANEL_SELECT_CLIPBOARD=wayland` to force the
  previous behaviour on a desktop where the bridge is unavailable.

- **Running the test suite no longer overwrites your clipboard.** Tests that
  exercised a copy path wrote to the real desktop clipboard, discarding
  whatever you had copied, and spawned a helper process for each one. Copies
  are now disabled while the tests run.

- **Menus in the terminal UI no longer close the instant they open.** The File
  and Settings menus, and the quit confirmation, disappeared within a fraction
  of a second of being opened, which left no way to reach any of them — or to
  quit PaperBoy from its own interface. The wizard-polling that runs on every
  pass of the event loop took the open overlay before checking whether the
  overlay was the one it was interested in, and dropped whatever it found.

- **The `FOR` loop's inline fields are no longer cramped or cryptic.** Three
  small things made loops harder to use than they needed to be. The boxes were
  fixed-width, so any real folder path or an alias longer than a word was
  visibly cut off with nothing to indicate there was more; they now grow to fit
  what's in them, up to a limit that keeps the rest of the statement on screen.
  The full explanation of a field was being used as the placeholder *inside* it,
  where it rendered as an unreadable stub — most notably the `MATCH` pattern,
  which read "Only files ..." — so the boxes now show a short placeholder
  (`*.json`, `folder`, `name`) and the explanation appears on hover, on both the
  box and the `MATCH` keyword itself. That explanation has been rewritten to say
  what the field *is* — a filename pattern, with examples — rather than
  describing what it filters out. Finally, the folder picker beside the path was
  drawn without a frame and read as a printed-on icon rather than a button;
  it now has a proper button frame, a pointing-hand cursor and a tooltip saying
  what it opens.

- **Environment rows now keep the same expand/collapse shape before and after
  opening.** Workspace environment files used to be plain dimmed load rows with
  no disclosure triangle, then silently changed into collapsible rows after
  being clicked. Every environment row now carries the same right/down caret
  icons as the Workspace tree, and expanding an unopened workspace environment
  loads the file and reveals its variables in one gesture.

- **The Response pane's compact view no longer shortens object keys.** It
  shortened every long quoted string, so a body with descriptive key names came
  out as rows of `"auth...ifier": "aneh...rucg"` — unreadable, which is the
  opposite of what an overview is for. Only values are compacted now; a key is
  left whole however long it is, recognised by the `:` that follows it (with any
  spacing between). Copying still yields the full, uncompacted text.

- **Postman environments now load at all.** PaperBoy only ever read `.vars`
  files, so a Postman environment export — a JSON document of `key`/`value`
  entries — was rejected as "not an environment file", and the Load Environment
  pickers hid `.json` outright so it couldn't even be selected. Postman
  environments now import into the same model: enabled variables become
  environment entries, and a value written as a provider reference
  (`{{ op://… }}`, `{{ ssm:… }}`, `{{ env:… }}`) still resolves exactly as it
  would in a `.vars` file. Variables Postman had disabled are skipped, since a
  `.vars` environment has no "present but switched off" state. Both the bare
  export and the `{"environment": …}` envelope an account backup writes are
  accepted.

- **Postman collections taken from an account backup now import.** Postman's
  "Export all data" backup and its API wrap each collection in a
  `{"collection": …}` envelope, where a single "Export collection" writes the
  bare `{"info": …, "item": …}` document. PaperBoy only recognised the bare
  shape, so every file in a backup's `Collections/` folder was not detected as
  Postman at all, fell through to the Hurl parser and opened as "not a
  collection". Both shapes are now unwrapped and imported.

- **Chips no longer sit a pixel out of line with each other.** A chip built
  around a dropdown — `REQUEST`, `BASELINE`, `COMPARISON` — was sized by egui to
  the same formula as every other chip, so the two agreed at egui's default text
  size and drifted a fraction of a pixel apart once the app scaled its text up,
  leaving an uneven bottom edge. The dropdown's height is now derived from the
  chip height rather than arriving at it independently.

- **A tethered clause now really does sit flush against the chip it qualifies.**
  The reduced spacing was applied while laying out the clause itself, but egui
  fixes the gap between two widgets when the *first* of them is added, so it had
  no effect and the pair was always drawn a full gap apart.

- **Quitting no longer warns about edits that quitting would not lose.** The
  warning counted every unsaved request edit, but an ordinary tab's requests are
  written to the session state exactly as they are, edit markers included, so
  those edits are still waiting — still flagged as unsaved — next time PaperBoy
  starts. The dialog therefore reappeared on every quit, for the same requests,
  no matter how many times it was dismissed. Only a Workspace tab genuinely
  loses work on exit, because it is bound to a live folder and re-reads its
  selected file from disk, so only its edits are counted now.

## [0.3.0] - 2026-08-06

### Added

- **A warning before unsaved request edits are thrown away.** Quitting, or
  closing a tab, while requests hold edits that were never written to a file now
  asks first and says how many are at stake. A Workspace tab's requests are
  deliberately not carried across restarts, and edits parked for a file you have
  switched away from live only in memory, so this was the point at which work
  disappeared without a word. The terminal UI folds the same warning into its
  exit confirmation, which now appears even with confirm-on-exit switched off
  (matching how unsaved *secret* edits already behaved).

- **Drag a clause from one line onto another.** A `SHOW`, `PARALLEL`,
  `STATISTICS`, `WITH` field, `BASELINE`/`COMPARISON` role — anything you can
  pull off a block — can now be dropped on another block instead of only in the
  bin, and it arrives with the value it was carrying (`SHOW(Time, Status)` keeps
  its columns). Hold **Shift** as you drop to leave the original in place and
  drop a copy, so one loop's `PARALLEL(4)` can be cloned onto its neighbours.

- **The line opens up to show where a modifier will land.** Dragging a modifier
  over a block now slides its chips aside and draws a dashed placeholder, the
  size of the block that will fill it and labelled with what it will say, so you
  can see how the drop changes the statement before letting go.

- **`REPORT <var>` is a palette block.** Previously the only way to report a
  variable was to drop `REPORT` on a `VARIABLE` block, which left no way to
  report a captured value or a loop variable.

- **A pencil marks anything edited but not yet saved.** It shows next to a
  request in the Requests list and the Workspace tree, next to a Workspace
  collection file that still has unsaved edits, and on the tab of any
  collection holding them — so it is obvious what a Save would write.

- **`STATISTICS` is now a block you can drag.** It joins the modifier palette
  and drops onto a `REPORT <var> AS <name>` or a computed column.

- The `BASELINE`/`SHOW` bracket in the block editor is now drawn above the pair
  as well as below it, so the two chips read as enclosed rather than merely
  underlined — much easier to pick out in a long compare line.

- Every chip that can be pulled out of a line now says so on hover, spelling out
  the plain-drag / Ctrl-drag distinction that was otherwise invisible.

- Reported variables and computed columns now have wizards of their own in both
  front-ends, closing the last two palette blocks that could only be edited as
  raw text. `REPORT <var>` offers a checklist of the variables actually in scope
  at that point in the flow — assignments, loop binders, `FOLDERS` role names and
  the `[Captures]` of requests earlier in the flow — plus a free-text row for
  names bound only at run time, and an `AS` name with `STATISTICS(…)` when a
  single variable is picked. `REPORT "…" AS <name>` gets a template field, a
  column name and the statistics checklist. The scope scan (`vars_in_scope`)
  lives in the shared core, so the terminal UI and the GUI offer the same list.

- **The PaperTrail block editor is now a complete way to write a report.**
  Reports open in a drag-and-drop **Blocks** view in the GUI, and it no longer
  makes you drop into Source to finish the job:

  - **Collections are chosen by name, not by path.** The `collection` dropdown
    lists the collections in the report's own workspace first, by name with
    their location underneath, and hides ones from outside that workspace
    behind a toggle. What it *writes* is still a path — a relative one, now
    including `../` for a collection in a sibling folder, so a workspace stays
    portable between machines instead of baking in paths that only exist on the
    one it was built on.
  - **Report settings are editable.** A fixed-width panel above `BEGIN` exposes
    the header directives — `collection`, `output`, `environment`, `root`,
    `baseline` and `columns` — stacked one per line, boxed so they read as
    settings for the report rather than the first steps of it, and lined up
    with the flow below. `collection` and `output` are always shown (only a
    missing `collection` is highlighted as an error — `output` defaults to
    CSV); the others are appended from the **Add a report setting** button
    beneath them. Any setting that is set can be cleared with its `×`.
    Anything with a closed set of valid values is a dropdown (`collection`,
    `environment`, and `output`, which names a format — `csv`, `json`, `html`
    or `xlsx` — rather than a filename); `root` and `baseline` are paths with a
    Browse button, stored relative to the report when they live beside it.
    These apply to the report as a whole rather than running as a step, so they
    are deliberately a fixed strip and not draggable blocks.
  - **A matching `END`** now closes the flow under the last block, mirroring
    the `BEGIN` at the top.
  - **`PARALLEL(n)`** loops expose their maximum concurrency as an editable
    number, and report-request blocks expose their `STATISTICS(…)` and
    `HIDE(…)` column lists alongside `SHOW(…)`.
  - **Every block and setting has hover help** explaining what it is and what
    each editable part of it does.
  - **The Source view is syntax-highlighted** with exactly the same colours as
    the terminal UI (the two share one highlighter), and the line the parser
    rejected is underlined.

- **Whole workspaces load from a git remote in the GUI**, matching the terminal
  UI: collections, environments and entire workspace trees, all without a local
  clone.

- **The GUI remembers your layout.** Window size, every panel width and height
  you dragged, which view was open, and which request or report you were on are
  saved to `state.json` and restored on the next launch. The terminal UI carries
  this geometry through untouched rather than rounding it into its own
  character-cell layout.

- **Every GUI file dialog reopens where you left it**, per kind of file.

- **A Workspace tab reopens on whatever you last selected in its tree.** A
  `.trail` report or `.vars` environment opened from the tree previously left no
  trace at all, so the tab came back with an empty right-hand pane. The
  selection is saved relative to the workspace root, and dropped on load if that
  file has since been deleted or renamed — a workspace is a live folder.

- **Workspaces can be grown and rearranged in the GUI.** A **New** menu on the
  workspace panel adds a collection, report or environment at the root, and a
  right-click on any row offers the same inside that row's folder. Items can be
  dragged onto a folder to move them there, or dropped below the tree to move
  them back to the root; open tabs, the open report and the expanded-folder set
  are all repointed at the file's new home. Creating and moving are both held
  inside the workspace root — symlinks included — and neither will overwrite an
  existing file. The terminal UI has the same two gestures: **Shift+N** names a
  new file in the highlighted folder (its extension — `.hurl`, `.trail` or
  `.vars` — chooses whether it is a collection, report or environment), and
  **Shift+M** moves the highlighted file or folder into a destination picked
  with `Space`, refusing to escape the workspace, overwrite, or nest a folder
  inside itself exactly as the GUI does.
- **A dry run in the GUI.** **Dry run**, beside Run in the report editor,
  previews what the report would produce — every loop expanded, every variable
  resolved, the projected row count and the output grid itself — without
  sending a single request. Producer and resolution problems, and any
  variable-availability warnings, are listed above the grid. This is the
  terminal UI's `d` preview, and both now share one implementation.
- **`BASELINE(…) SHOW(…)` is editable.** A `SHOW` on a comparison's
  baseline used to be invisible in the GUI: preserved if you had written it by
  hand, but with nothing to view or change it. It now appears as its own chip,
  placed immediately after the `BASELINE` it belongs to and joined to it by a
  bracket (it keeps `SHOW`'s own colour: three same-coloured chips in a row read
  as three peers, so it is the bracket, not the hue, that says which one it
  qualifies), and the ENVS form offers its fields as a checklist boxed and
  indented beneath that baseline's row — so there is no mistaking it for something governing the
  comparisons.
- **Row descriptions are saved.** Headers, cookies, query parameters, options
  and form fields have always had a Description column in the terminal UI's
  request wizard, but whatever you typed there was thrown away on submit — it
  had nowhere to live in the Hurl format and no field in PaperBoy's model. Each
  row now carries its note through save, reload, request export/import and the
  session state, written as a `# @desc …` comment line directly above the row
  (a trailing comment would be ambiguous, since a header value may legitimately
  contain `#`). The GUI's key/value tables gained the matching Description
  column, and a section that already carries notes opens with that column
  showing. Postman's per-parameter documentation is imported into it instead of
  being discarded.

- **Double-clicking an environment in a workspace opens it.** It now reveals and
  scrolls to that environment in the Global Environments panel, expanding it if
  it was collapsed — the same gesture the collection and report rows already
  had.
- **The block editor helps with the first report.** An empty flow says what to
  drag in rather than showing a bare `BEGIN`/`END`; the `collection` dropdown
  always offers **Browse…** (and explains itself when no collection is loaded,
  where it used to be simply an empty menu); a flow that sends requests but
  reports none warns that the report will have no columns, which is the single
  most common reason a first report comes back blank; and a modifier chip
  dropped on a block that won't take it now says why — distinguishing "wrong
  kind of block" from "that clause is already there" — instead of springing
  back in silence.

- **The terminal UI's node editor reaches the GUI block editor's feature set.**
  Following the GUI's chips block by block, but presented as forms rather than
  as things to drag onto each other:

  - The request form gained the `HIDE(…)` checklist beside its `SHOW(…)` one
    (they are separate clauses in the language, so they are separate lists —
    each row names the clause it writes), and a `WITH … END` field list with an
    add row. A `WITH` field opens its own small form for the column name, the
    Hurl query behind it and the `STATISTICS(…)` checklist; Enter returns to
    the request form it came from, Esc likewise.
  - `PARALLEL` is no longer only on/off. Ticking it reveals a digits-only
    max-concurrency row that writes `PARALLEL(n)`; blank still means "use the
    prelude's `MAX_PARALLEL`", and is labelled as such rather than shown as an
    empty box.
  - **A comparison's `BASELINE(…) SHOW(…)` is edited in the `FOR … IN ENVS`
    form** — where the baseline it qualifies already lives — rather than as a
    block of its own. The checklist lists what the loop's *body* reports, and
    appears only once there is a baseline for it to apply to. Nothing is ticked
    by default, because an empty baseline `SHOW` means "carry nothing across".
  - `FOR … IN FOLDERS` now opens the loop form (variable, folder picker,
    `PARALLEL`) instead of only a bare folder browser; its `WITH role="glob"`
    clauses are preserved.
  - `VARIABLE = VALUE` and `LIST NAME = [ … ]` gained forms, so the two most
    common non-request blocks no longer route straight to the raw line editor.
    A tuple list or a computed producer still does — a flat form would flatten
    structure those carry.

  The raw "edit as a line" escape hatch is unchanged and still reaches anything
  the forms don't, but it has moved off `Enter` (which now opens the block's
  form) onto `e`, where it already was for every other block.

### Fixed

- **Request rows now carry stable widget ids.** A row's decorations (method
  badge, run marker, edit pencil) come and go with which collection is loaded,
  and `egui` numbers widgets by how many came before them, so switching
  collections renumbered every row after them. Each row is now keyed to the
  request it draws. (This also silenced a red id-clash outline that flashed
  around the edit pencil in debug builds; `egui` only runs that check when
  `debug_assertions` are on, so release builds were never affected.)

- **A drop placeholder was drawn at the wrong indent.** A block dropped just
  after `BEGIN` (or just after a `FOR` header) lands one level in, but its
  silhouette was drawn flush against the margin. The preview now uses the depth
  the block will actually land at.

- **Refusing a duplicate `REPORT` explained the wrong thing.** Dragging `REPORT`
  onto an already-reported request answered "REPORT can only be added to a
  request or a SET assignment" instead of saying it is already there.

- **Edits were thrown away when you opened another collection from a
  Workspace.** A Workspace tab shows one file's requests at a time, and loading
  the next file overwrote them without saving. The outgoing file's unsaved
  edits are now parked and handed back when you reopen it. Saving a collection
  (locally or to git) now also clears its edit markers in the GUI, which it
  previously never did.

- **A `STATISTICS(…)` clause was invisible in the block editor.** It was in the
  source and in the wizards, but nothing drew it — on a named report column it
  now gets its own chip (tethered to the column it summarises), and on a `WITH`
  field it is shown on that field's row.

- **A chip only snaps off a line if the statement survives without it.** Pulling
  `REPORT` off a reported *column* would have taken the whole statement with it,
  so grabbing that chip now moves the line instead — the same rule, applied
  centrally, for every chip present and future.

- **Dragging the `WITH` chip now carries its fields with it.** Detaching `WITH`
  removes the whole block, but only the keyword chip used to move; the fields
  now float with it and their space is ghosted, so what moves is what goes.

- **A chip dragged out of a line now looks like it has been picked up.** It
  floats under the pointer and leaves the same dashed grey ghost in its slot
  that a dragged block does, so the gesture is visible (and obviously
  reversible) instead of appearing to do nothing until the drop.

- **Dragging one block out of a long line works again.** The `BASELINE`,
  `COMPARISON`, baseline `SHOW` and `WITH` chips had no detach action, so a
  plain drag on any of them fell through to "move the whole line" — which, on a
  compare loop, is every chip there is. They are now detachable (and carry a `×`
  like every other modifier), so a plain drag pulls just that clause out and
  Ctrl/Cmd+drag still moves the line. Dropping either half of a comparison
  degrades the loop to a plain pass over the environments that remain, rather
  than writing a `BASELINE(…)` with no `COMPARISON(…)`.

- **`columns:` could not be added from the report settings panel.** It was
  seeded with an empty value, and an empty value means "remove this directive",
  so choosing it from the add menu did nothing at all.

- Optional report settings keep their `×` while still unset, so one added by
  mistake can be taken off again instead of being stuck showing its prompt.

- **The GUI dry-run button appeared to do nothing.** It built the preview
  correctly but, unlike a real run, never switched to the Results view — and
  the toolbar it lives on spans every view, so from the Blocks view (where you
  press it) the button looked dead. Both paths now go through one place.
- **The run marker no longer hides under the scrollbar.** The pass/fail tick
  next to a request sits at the right edge of the list, where egui draws its
  scrollbar over the content rather than reserving a gutter for it, so a
  scrolling collection put the bar straight through the marker. Both the
  request tree and the workspace tree now leave it room.
- **The GUI's Response panel follows the selection.** It showed whatever came
  back most recently, so clicking through a collection left one request's body
  and status sitting under a different request's name. It now shows the
  selected request's own last response — and its own "sending" state, so a
  request still on the wire spins while selecting a settled one shows what that
  one actually got back. This is what the terminal UI has always done; both
  read the same per-entry record.

- **Report validation speaks the reader's language.** The diagnostics under the
  block editor were the last hardcoded English strings in the UI; they are now
  rows in the shared string table like everything else, and were reworded away
  from source syntax at the same time — a report with no collection says "No
  collection chosen" rather than "missing '# collection:' header", which is not
  a header the user of the block editor ever sees.

### Changed

- **The dry run is no longer a popup.** In both front-ends the preview now
  appears in the report's own results pane, in the place — and with the same
  scrolling — the real results occupy, and is dismissed with Esc or superseded
  by a real run. As a modal it stacked above the cell viewer it could spawn,
  which left no way back to the cell.
- **The GUI results table fills the window and keeps its columns in it.**
  Columns are measured and then fitted to the available width: a narrow table
  is stretched to fill the window instead of huddling at the left edge, and a
  wide one is squeezed — taking the room from the widest columns first, so a
  three-character `Status` column isn't punished for a sprawling response
  column beside it. Only when even the minimum widths can't fit does the table
  overflow into a horizontal scroll bar. Truncated cells are, as before, one
  click away from the cell viewer.
- **The GUI File menu is grouped by verb** (New / Open / Save / Import-Export)
  instead of one flat list.
- **The active environment is unmissable in the GUI** — ticked, coloured and
  banded in the Global Environments list, rather than marked with a small dot.
- **Terminal UI file browsers filter folders as well as files.** Typing a filter
  now hides non-matching folders too, which is what makes it usable in a large
  tree. The `../` parent entry is always exempt so you can never filter yourself
  into a dead end.

### Removed

- **"Import Postman" from the GUI File menu.** **Open ▸ Collection** accepts a
  Hurl `.hurl` file or a Postman `.json` export and works out which it is, so
  the separate entry was a trap for anyone who didn't find it.

### Fixed

- **The drop marker in the block editor is the *shape* of the block being
  dragged.** It used to be a fixed one-row gap running the full width of the
  editor. It is now drawn as the block's own silhouette — one rounded outline
  per row it will occupy, at that row's own width and indent — so a dragged
  `FOR` loop shows its short head, its indented body and its short `END` rather
  than a solid slab nothing like the block in hand. As the gap animates open
  the marker is revealed rather than stretched, so the block never appears to
  change size on the way in.
- **The dashed outline left behind by a picked-up block traces the block.** It
  started at the editor's far-left margin regardless of how deeply the block was
  nested (the indent is laid out *inside* the row, so it was included in the
  measured rect), and its corners were square inside a rounded fill. It now
  starts at the block's own indent and is dashed around a rounded path with the
  same corner radius the blocks have.
- **Dragging a `FOR` loop no longer comes apart.** The lifted rows were being
  transformed from inside the loop's head row, so egui only moved the shapes it
  had already painted and the body appeared to lag behind the head. The whole
  lifted subtree is now transformed once, after all of it is painted.
- **The block editor's palette and diagnostics panel widths now persist.** They
  were only reapplied on the one startup path that restored a session report, so
  opening a report any other way — from the reports list, freshly created, or
  from a Workspace tree — silently reset them to the defaults, which read as the
  setting not being saved at all. Every report now opens through one place.
- **The desktop icon on Linux** is documented and works: the first GUI launch
  installs a `.desktop` entry matching the window's Wayland app id (see the
  README for the rescan caveat).

## [0.2.0] - 2026-08-04

### Added

- **Native file & folder pickers in the GUI.** Every place the GUI chooses a
  path now opens the operating system's own file dialog instead of a typed-path
  text box: opening a collection, environment or workspace folder; saving a
  collection, environment, response or exported report results; picking the
  folder for a report's `FOR … IN FILES` / `FOLDERS` node; and choosing the
  file for a `File` / `Base64` form field. (The terminal UI keeps its in-app
  browser overlay, which is the right fit for a terminal.)

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

- **Disabled request rows now read as greyed-out in both front-ends.** A
  key/value row whose enable checkbox is unticked (a header, query param,
  cookie, option or multipart form field) isn't sent, so its key and value are
  now drawn in the dim colour — obvious at a glance that it's inactive, instead
  of looking identical to an active row. Applied in the GUI's key/value editors
  and mirrored in the terminal UI's request wizard.

- **The GUI request Code view is now editable.** The **Code** tab (Hurl source
  or the resolved-JSON preview) was previously read-only; it is now a full-width
  editor that fills the panel at a fixed size (rather than shrinking to fit its
  text). Edits are re-parsed on the fly straight back into the request — Hurl via
  the shared Hurl parser, JSON via the request-JSON round-trip — so typing a new
  header, method or body updates the rest of the editor live. Your text stays put
  while you type (it only re-syncs from the request when you switch request or
  representation, or leave and return to the tab), and an unparseable edit keeps
  your text on screen with an inline error rather than discarding it. `{{ VAR }}`
  placeholders are still colour-coded by resolution status (without being
  substituted, so the buffer edits cleanly). This brings the GUI in line with the
  terminal UI's editable raw Hurl/JSON views.

- **PaperTrail chips: plain-drag detaches, Ctrl-drag moves the line, and
  SHOW/HIDE/RESPONSE are click-to-edit.** You no longer have to grab the base
  block to rearrange a report line, and every chip is now individually useful.
  Plain-dragging a modifier chip (e.g. `RESPONSE PRETTY`, `SHOW(Time)`, an `AS`
  alias) picks up *that chip* to detach it; holding **Ctrl/Cmd** while dragging
  *any* chip lifts the whole line (or subtree) to move it — so a long
  `REPORT REQUEST … RESPONSE PRETTY SHOW(Time) …` can be reordered by grabbing
  any of its chips, not just the `REQUEST` one. Clicking a `SHOW`, `HIDE` or
  `RESPONSE` chip now opens the request wizard on its field pickers so you can
  change which fields are shown/hidden without retyping the clause.

- **PaperTrail block palette adds SHOW, HIDE, RESPONSE and a computed-column
  block.** Auditing the drag/drop report editor turned up constructs you could
  render but not *create* from the palette. You can now drop **RESPONSE** (adds a
  `RESPONSE PRETTY` clause), **SHOW** and **HIDE** modifiers onto a `REPORT
  REQUEST` — seeding a sensible default field you then refine in the request
  wizard — and drop a new **`REPORT "…" AS`** computed-column block for a value
  computed from other columns. Each round-trips through the report text and can
  be detached again (SHOW/HIDE/RESPONSE) like the other modifiers. Both
  front-ends share the underlying model, so the terminal UI's node-insert menu
  offers the computed-column block too.

- **GUI report blocks now lift and float while you drag them.** Dragging a block
  in the PaperTrail block editor now picks it up: the block follows the pointer
  in a floating layer and its original slot goes blank, so it reads as physically
  moved rather than staying put with only a payload cursor. On drop it lands in
  the target gap as before. (The whole line — including any nested `WITH` fields
  — floats together.)

- **Dragging a `FOR` loop now lifts the whole loop.** Picking up a loop header
  used to float only the header while its body and `END` stayed behind; a loop
  now floats as a single unit — the header, every nested statement inside it and
  the closing `END` all lift together under the pointer and leave one contiguous
  blank gap where they were, so it reads as physically picking up the entire
  loop (matching how a single block already behaved).

- **A dragged block now leaves a dashed "ghost" where it came from.** While a
  block (or a whole `FOR` loop) floats under the pointer, its original slot is
  marked with a faint dashed placeholder, so a block picked up by accident is
  obviously reversible — the outline shows where it started and where dropping
  it back would return it.

- **A `REPORT REQUEST … WITH` now reads as one enclosed unit.** A report request
  that carries a `WITH … END` block is drawn inside a single subtle bordered
  container spanning the request line and all its `WITH` fields, so it's clear
  the whole thing is one block you drop *around* — never into the middle of its
  `WITH` statements. (Dropping near it already landed after the whole unit; the
  border makes that visually obvious.)

- **GUI report editor: the end-of-list drop marker now matches a block's
  height.** When you drag a block (or a new palette block) past the last row,
  the highlighted "drop here" mark at the bottom of the report is now sized to a
  full block instead of a fixed 26px sliver, so it matches the insert-strip gap
  and reads as the same-sized ghost of the block being dropped.

- **The GUI request "Code" section is now called "Raw Request" and opens on
  Hurl by default.** The renamed section (Raw Request / Requête brute / Rå
  anmodning) still toggles between the Hurl source and the resolved-JSON
  preview, but now honours the **Default Request View** preference for which one
  it opens on — and that preference now defaults to **Hurl**, PaperBoy's native
  request format, instead of JSON. Both front-ends share the preference, so the
  terminal UI's Main panel also renders and copies Hurl out of the box.

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
