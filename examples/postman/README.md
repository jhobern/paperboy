# Example Postman collections

Three files to import, plus one to compare against. They exist to exercise the
`# [Gen]` block and the Postman dynamic-variable mapping — see
[Computed values](../../README.md#computed-values).

| File | What it is for |
|---|---|
| `dynamic-variables.postman_collection.json` | Every outcome the importer can reach for a `{{$dynamic}}` variable |
| `signed-requests.postman_collection.json` | Requests signed by a pre-request script — what actually has to be reproduced by hand |
| `signed-requests.hurl` | The same collection with its `[Gen]` blocks written in: the finished article |
| `paperboy-examples.postman_environment.json` / `.vars` | Values the two signing collections need |

Open the `.json` files with **Open ▸ Collection** — no Postman account, no API
key. Everything points at `postman-echo.com`, which reflects the request back,
so the response shows the headers that were actually sent.

## Dynamic variables

Import it and read `CONVERSION-NOTES.md`. The requests are named for the three
outcomes:

- **Built in** — `$guid`, `$randomUUID`, `$isoTimestamp` become Hurl's own
  `{{newUuid}}` / `{{newDate}}` and need nothing supplied. The *same GUID twice*
  request is there to show one name never produces two rows.
- **Computed** — `$timestamp` and `$randomInt` have no Hurl equivalent and
  arrive as `[Gen]` rows (`timestamp`, `random_int(0, 1000)`). The request runs
  as imported.
- **Supplied** — faker variables have no honest equivalent, so they are renamed
  to plain `{{randomFirstName}}` and listed as values you must bind. Guessing at
  a name would send a plausible wrong value, which is harder to notice than a
  request that refuses to run.

The last request mixes all three, so the preview shows all three placeholder
colours at once.

## Signed requests

Every request here is signed by a Postman pre-request script. Hurl cannot run
one, so on import the script is dropped, reported in `CONVERSION-NOTES.md`, and
the placeholders it used to fill arrive undefined. Writing the `[Gen]` block is
the manual step — `Alt+0` in the request wizard, or edit the file.

`signed-requests.hurl` is the result, and runs as-is:

```sh
paperboy -c signed-requests.hurl -e paperboy-examples.vars
```

Five requests pass; the sixth fails on purpose. Between them they cover:

- **HMAC-SHA256, hex and Base64.** The same signature both ways, because
  `hmac_sha256` and `hmac_sha256_b64` are the same MAC differently encoded and
  a server accepts exactly one of them.
- **A Base64 credential and a body digest.** Note the digest hashes a literal
  string that must match the body byte for byte: PaperBoy signs what you give
  it and will not assemble the body for you. That is the same reason it will not
  build an AWS SigV4 canonical request.
- **A signature over a URL-encoded query**, assembled with `concat` and
  `urlencode`, because only you know what the server expects in the canonical
  string and in what order.
- **A known-answer check** — RFC 4231 case 2, key `Jefe`, message
  `what do ya want for nothing?`, asserted against the published digest. A
  signing implementation that is self-consistently wrong passes every test you
  write from its own output, so send this one first when a real signature is
  being rejected and it is not obvious which side is at fault.
- **A typo**, `hmac_sha526`. The row is reported before the request is sent,
  naming the function, and then the request goes out anyway with `{{oops}}`
  literal — a visible failure is easier to diagnose than a refusal.

The nonce in the header and the nonce inside the signature are the *same* nonce:
a row is evaluated once per send, not once per use. The Postman script only
manages that by remembering to set both from one variable.
