# Persistent preparation caches

`CACHE` stores explicitly selected response captures after preparation succeeds.
It does not cache report results: normal reporting requests still run every time.

```text
CACHE document_session
    FILES ("{{DFA_FILE}}")
    PERSIST (session_id)

    REQUEST create_session
    REQUEST upload_document
END
```

`FILES` identifies inputs by their content, not their location. It does not
upload anything. The requests inside the block perform creation and upload.
`PERSIST` lists the response captures to save after **all** preparation succeeds.
On a hit, only those values are restored and preparation is skipped. On a miss,
preparation runs, and only those same values become visible outside the block.

Preparation requests must declare their real success expectations in the Hurl
collection, for example `HTTP 200`. An HTTP error that a request accepts is not a
failed preparation or a reason to enter recovery.

## Identity and sharing

The cache container is identified by:

```text
(canonical script path OR explicit namespace) + effective environment fingerprint
```

Within it, an entry is identified by:

```text
cache block name + preparation fingerprint + FILES hashes + KEY values + PERSIST names
```

Environment values are resolved and merged (active/pinned/selected environment)
and sorted before fingerprinting. Formatting and ordering in `.vars` files have
no effect. Changed values, including credentials, change the fingerprint.
Request captures such as OAuth tokens are excluded, as are loop variables and
comparison roles. Identical effective configurations have identical environment
fingerprints, regardless of their display names.

The preparation fingerprint includes the block's preparation statements and
referenced request definitions. Changing unrelated reporting columns does not
invalidate the cache.

Declare additional non-environment inputs that affect preparation:

```text
CACHE document_session
    NAMESPACE "dfa-documents"
    KEY ("{{document_type}}", "{{processing_version}}")
    FILES ("{{FRONT_FILE}}", "{{BACK_FILE}}")
    PERSIST (session_id, document_id)

    REQUEST create_session
    REQUEST upload_front_and_back
END
```

The order of file/key components matters. Changing document contents causes a
miss; moving identical contents does not, provided the preparation definition is
unchanged. Filenames, MIME types, per-row upload options, or logical case IDs
that matter to the API should be explicit `KEY` components.

With no namespace, different saved scripts are isolated. An explicit namespace
permits sharing across scripts/checkouts, but block names, environments,
preparation, and inputs must still match. Unsaved scripts require an explicit
namespace. Separate cache block names isolate independent workflows even if
they happen to use identical preparation requests.

## Refresh, invalidation, and stale entries

```text
CACHE REFRESH document_session
CACHE INVALIDATE document_session
```

These commands reference the named declaration's **current entry** in lexical
scope. They do not affect every document in the cache.

Refresh invalidates that entry, reruns its preparation with current dependencies,
and publishes new persisted captures only after success. If preparation fails,
the invalid entry remains unusable. Invalidation removes the usable entry
without rebuilding it; this is useful after deliberate server-side deletion.

A cache hit means "we previously prepared this resource", not "the server still
has it". Prefer normal execution first and API-specific confirmation of
staleness after failure:

```text
TRY
    REPORT REQUEST dfa_result AS dfa RESPONSE PRETTY WITH
        Status: HttpStatus
        Overall: jsonpath "$.dfa_overall_result" TRUTH "{{CATEGORY}}" DETAIL
        Breakdown: jsonpath "$.dfa_breakdown"
        "Response Time": TimeWait STATISTICS(MEAN, MEDIAN)
    END
RECOVER RETRY(1) failure
    IF "{{failure.http_status}}" == "400" AND "{{failure.body.status}}" == "error" AND "{{failure.body.message}}" == "Can't find document"
        CACHE REFRESH document_session
    ELSE
        RETHROW
    END
END
```

`RECOVER RETRY(1)` permits one recovery followed by one additional attempt of the
protected body. Successful recovery triggers the retry automatically. If that
attempt fails, recovery is **not entered again**. `RETRY(n)` permits at most `n`
additional attempts and requires a positive integer. Recovery failure or
`RETHROW` stops immediately. There is no standalone `RETRY` command.

Plain `RECOVER failure` keeps its existing behavior: recover and continue,
without repeating the protected body.

Failed-attempt report cells are replaced, not accumulated. A successful repair
leaves a warning, while the result and endpoint timing describe the surviving
attempt. HTTP side effects cannot be rolled back: every request in a retry body
must be safe to repeat. Keep preparation in `CACHE`, not inside that body.

## Inspecting the failure

`failure` retains its original textual diagnostic. Structured selectors refer
only to the actual failed request's surviving response:

| Selector | Meaning |
|---|---|
| `failure.message` | Original diagnostic |
| `failure.http_status` | HTTP status, or empty for no response |
| `failure.headers.x-session-state` | Case-insensitive header lookup |
| `failure.body.error.code` | Nested JSON field |
| `failure.body.errors.0.code` | JSON array element's field |

For example:

```text
IF "{{failure.http_status}}" == "401" AND "{{failure.headers.x-token-state}}" == "expired"
    CACHE REFRESH authentication
ELSE
    RETHROW
END
```

Missing optional fields, JSON null, and non-JSON response bodies resolve to empty
values for these selectors; they do not create secondary lookup failures.
Malformed selector syntax is still an error. Avoid treating an empty field as
evidence of expiration. A timeout, `500`, generic `401`, or a dev/staging result
difference alone does not establish that a document session is stale.

If the failure response is ambiguous, use an ordinary probe request with
assertions in recovery before refreshing. Failed confirmation must propagate.

## Non-document resources

`FILES` and `KEY` are optional. For example, place this inside the environment
loop and before the document loop:

```text
CACHE authentication
    PERSIST (token)
    REQUEST oauth2
END
```

This entry is shared across documents in that environment. Refreshing it
obtains a new token; it does not expire uploaded sessions unless the author
explicitly includes a token-dependent value in their document cache key.

Use distinct persisted names when different resources must remain visible at
the same time, or qualify their outputs with the cache name in PaperTrail
expressions. Bare capture names retain last-writer ownership.

## Storage, concurrency, and safety

- Linux: `$XDG_CACHE_HOME/paperboy/trail`, or `~/.cache/paperboy/trail`.
- macOS: `~/Library/Caches/paperboy/trail`.
- Windows: `%LOCALAPPDATA%/paperboy/trail`.
- `PAPERBOY_CACHE_DIR` overrides the cache root directly.

Entries contain only declared captures and bookkeeping, not entire environment
or response objects. These values can be credentials: storage is private
plaintext, **not encryption**. On Unix, cache directories/files must have
owner-only permissions. Do not put the cache in Git, public CI artifacts, or a
directory available to untrusted jobs.

Per-entry locks coordinate preparation and refresh across workers/processes.
A late stale reader does not invalidate an already-published replacement.
Unrelated entries can prepare concurrently. This does not make concurrent use
of mutating/consuming server sessions safe; use separate keys or sequential
execution when the API requires it.

Use local filesystems or shared host volumes with working OS locks. Distributed
cache services and arbitrary network-filesystem coordination are not supported.
Cache corruption and storage failures are explicit errors. Removing the cache
while no runs are active forces preparation next time; do not delete lock files
under active processes.

Dry runs, validation, and saved-result rendering do not access the cache.
Selected live reruns use it without executing untouched comparison sides.
Persisted captures are not automatically exported into reports or checkpoints;
explicitly reporting a sensitive capture still exposes it like any other value.

There is no automatic TTL, proactive token renewal, or guessed stale-response
policy. A process crash can leave a remotely created session whose upload or
local publication did not complete; exactly-once remote preparation requires
API support and is not guaranteed by the local cache.
