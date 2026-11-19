# Durable headless runs

PaperBoy 0.9 adds a private execution bundle, separate from table JSON and
`.baseline` snapshots. It stores **raw executions on every target**, before
comparison collapse. No GUI feature is required.

## Commands

Create a bundle alongside, **not inside**, the input corpus:

```sh
paperboy -r compare.trail -e baseline.vars -e candidate.vars \
  --run-state runs/nightly --case-key case_id \
  --execution-binding session_id,document_id \
  -o nightly.html -o nightly.json --progress-json
```

The parent directory (`runs`) must exist; `nightly` must not. `--case-key`
names a non-secret variable supplied by the source case, usually a CSV field
or FILES/FOLDERS loop binder. It must identify the case independently of its
target and session. Duplicate case/target/role identities within a comparison
are rejected. Named tuple fields other than this key do not affect pairing.
Path-valued case keys under the producer root are normalized relative to it.

The optional `--execution-binding` declaration lists the names that later
repairs may replace. It is immutable for the bundle. Do not declare case,
truth, input-file, endpoint-authority, generator, or report-column variables.
Replacement values are **request-only**: applied after tuple bindings and
captures, including qualified captures in `USING`. They do not change loop
expansion, assignments, truth, or case identity.

Inspect without loading environments, resolving credentials, or sending HTTP:

```sh
paperboy --run-state runs/nightly --state-action inspect
paperboy --run-state runs/nightly --state-action inspect --inspect-details
```

Stdout is one JSON object, inspection schema **1**, containing `run_id`,
`revision`, `phase`, `shuffle`, `history`, and `executions`. Each execution has
an opaque `execution_id` (for example `x00000003`), a `case_id`, `target`,
`role`, `comparison`, `attempt`, `result_attempt`, `binding_revision`, `status`,
and diagnostic counts. IDs are scoped to the run, not global document IDs.

Statuses are `pending`, `in_flight`, `success`, and `error`. Both of the last
two are **completed**. `result_attempt` identifies the payload's attempt:
while replacing an execution, its previous payload can remain present with a
pending/in-flight status. It is not a result of the new attempt.

`--inspect-details` additionally exposes the case key, rendered cells,
row diagnostics and `details`: intrinsic HTTP status/response and declared
fields for **REPORT REQUEST** statements, even when hidden from the table.
Ordinary unreported setup/authentication request responses and capture pools
are not inspected or persisted. Pre-send failures have diagnostics rather
than an invented HTTP status.

### Repair exactly one side

Use the run ID, current revision, execution ID and target returned by inspect:

```json
{
  "schema": 1,
  "run_id": "RUN-UUID-FROM-INSPECT",
  "revision": 2,
  "overrides": [
    {
      "execution_id": "x00000003",
      "target": "candidate",
      "values": {"session_id": "replacement", "document_id": "uploaded"}
    }
  ]
}
```

```sh
paperboy -r compare.trail -e baseline.vars -e candidate.vars \
  --run-state runs/nightly --state-action rerun --expected-revision 2 \
  --select x00000003 --bindings repairs.json \
  -o nightly.html -o nightly.json
```

Selection is **exact-side by default**. A candidate-only rerun does not
execute its baseline. A baseline-only rerun recomputes every dependent
comparison from the saved candidates without executing them.

Repeat `--select` or pass comma-separated IDs to repair a batch in one
invocation. `--whole-comparison` explicitly expands a selection to all sides
of its declared comparison for the same case. GUI reruns retain their
existing pair-wide behavior.

Bindings require an exact execution ID and matching target (`null` without
ENVS), even with comparison-wide selection. They cannot affect an unselected
side. Values must be literal strings, not provider/template expressions.
Unknown/duplicate selectors, invalid scopes, incompatible source/input
hashes, and stale revisions fail before report requests or state replacement.
Bindings may be omitted for a normal retry with unchanged execution inputs.
There is no arbitrary-result import or separately applicable patch artifact.
An explicit second rerun is another attempt; reusing an old revision fails.

### Resume or render

```sh
paperboy -r compare.trail -e baseline.vars -e candidate.vars \
  --run-state runs/nightly --state-action resume --expected-revision 4 \
  -o resumed.html -o resumed.json

paperboy --run-state runs/nightly --state-action render \
  -o copied.html -o copied.json
```

Resume selects only pending/uncertain executions. It preserves completed
successes **and completed errors**; failed assertions or poor accuracy do not
make a completed case pending. Resume accepts no selectors or comparison
expansion. If unfinished work previously used replacement bindings, supply
them again in a bindings file referencing the current revision: replacement
values are never saved for replay.

Render needs no sources, environment or old image paths. It uses bundled
image bytes and the ordinary comparison, truth, statistics and report
writers. Rendering does not execute, change revisions, or issue a new quality
judgment; successful rendering exits 0 even for a failed/partial saved run.
Outputs must be outside the bundle.

## Checkpoints and recovery guarantees

Native state schema **1** consists of `state.json`, an active
`journal-REVISION.ndjson`, and `writer.lock`. The snapshot contains explicit
safe row/metadata DTOs, raw target roles and paths, source/input SHA-256
fingerprints, rendering metadata, images, and compact attempt provenance.
It does **not** serialize a `RunContext`, the process environment, environment
files, complete row-variable maps, source authentication requests, or capture
pools. Only variable values actually needed by declared output/truth columns
are retained. Source/parameter compatibility is fingerprinted, not restored
by persisting arbitrary runtime values.

An operation reserves a new revision before HTTP and commits another after
assembling its outcome. Inspect returns the currently authoritative revision.
The exclusive OS file lock covers validation, execution and commit, and is
released automatically on process death. Do not delete `writer.lock`, even
when idle: deleting it can defeat locking. Use a local filesystem supporting
file locks and atomic rename; this is not distributed concurrency control.

Snapshot writes use a restrictive sibling staging file, flush, atomic rename,
and directory synchronization on Unix. A crash leaves the preceding snapshot
or the new one, not a half-written JSON document. After commit, old completion
journals are compacted away. Superseded attempts survive as compact metadata,
not extra scored rows.

The active journal records and flushes each start and each completed row,
including its image bytes. It does not copy, finalize or render the whole
report per row. Readers ignore only a torn final record; other journal
corruption/read failures are errors. A disk/checkpoint failure stops new work
and reports failure rather than claiming persistence.

**External calls are not exactly-once.** A process can die after a server acts
but before the completion record is durable. Such in-flight work is replayed
by resume. Image fetching is part of recording completion, so interruption
during it can also leave the corresponding row uncertain. Requests outside
selected loops replay, including setup/authentication; cleanup runs for
visited scopes on ordinary/graceful completion. Hurl's explicitly configured
retries/repeats remain effective. PaperBoy adds no automatic recovery retry.

`--stop-on-stdin`, signals and `--grace` retain their meanings. A stopped-again
run stays resumable. On grace expiry the durable bundle remains available,
but native mode does not wait for a stuck worker to render partial outputs;
use `render` afterward. A second stop retains the existing immediate exit
130. Neither abrupt termination nor immediate exit promises cleanup.
Scope-wide diagnostics not yet returned by an interrupted worker are
uncertain; durable row diagnostics are retained.

If a crash leaves **all rows completed but the operation still active**,
resume refuses to invent setup/cleanup completion: render that partial state
or explicitly rerun selected rows/scopes. Global diagnostics not owned by a
replaced row remain recorded; fixing one row never erases unrelated failures.

## Relocation, credentials and limits

For rerun/resume, supply the original `-r`, collection/helper sources, `-e`
files and explicit `--param`/`--targets`/`--shuffle=SEED` options again.
Current environment/provider references are resolved anew. No old credentials
are silently substituted. Inspect reports the shuffle seed when used.

Relative references follow the supplied report's folder and `# root:`.
Relocate the sources/corpus together, or change only locator headers
(`root`, `collection`, `environment`) and explicit file arguments.
Source bodies, effective parameter configuration, case ordering, targets and
input contents and target endpoint origins must still match. Producer files,
request-file inputs and local report images are fingerprinted. FILES respects
its match pattern; matched FOLDERS cases are fingerprinted recursively.
Keep changing outputs and the bundle outside the corpus. Inputs
must remain immutable throughout an invocation; fingerprints are a preflight
check, not a filesystem transaction. Remote image contents are saved when
obtained, not assumed immutable on their server.

Schema 1 deliberately refuses unsafe/incomplete plans:

- Empty or erroneous offline expansions, capture-derived producers,
  assignments or target identity, and ambiguous logical cases.
- REPORT statements outside leaf blocks: post-loop broadcasts cannot be
  acknowledged correctly as independently completed rows.
- External `.baseline`/ENVS snapshot sides. Use live ENVS raw sides instead.
- Request file paths that cannot be resolved by the offline plan.
- Unsupported schema or a different PaperBoy version. No lossy automatic
  migration is attempted.

Bundles and `--inspect-details` **can contain sensitive document/API response
data**, including secrets intentionally included in report columns or echoed
by an endpoint. They are private operational artifacts, not logs. New bundle
directories/files are mode 0700/0600 on Unix (platform ACLs apply elsewhere).
Report exports retain their usual permissions; secure the output directory.
Restrict access, retention and backups appropriately.

Saved-mode `--progress-json` is **schema 2**: stable raw execution/run/revision
IDs accompany events; row events contain status/attempt, not response cells.
The terminal event uses diagnostic counts rather than full result errors.
Ordinary report mode remains schema 1. A successful execution still uses
the existing exits: 0 clean, 1 setup/write/execution error, 3 skipped,
4 partial/interrupted, 5 explicit quality-gate failure. Untouched errors or
pending rows prevent a selected repair from claiming a clean whole run.

PaperBoy does not interpret an engine-specific error or create replacement
sessions/documents. An external adapter inspects, repairs, then invokes the
generic selected rerun. No interactive repair callback or cloud integration
is involved.

Local regression:

```sh
cargo build --no-default-features
python3 scripts/headless_saved_smoke.py
```

The script uses a loopback HTTP fixture only and cleans up its local files
and server. It verifies separate-process inspection, exact-side repairs,
rendering, relocated roots, graceful interruption and crash recovery.
