# HMAC-SHA256 over a nonce and a timestamp
POST {{baseUrl}}/post
Content-Type: application/json
X-Api-Key: {{API_KEY}}
X-Nonce: {{nonce}}
X-Timestamp: {{ts}}
Authorization: HMAC-SHA256 {{sig}}
# [Gen] 3
# nonce = random_hex(16)
# ts = timestamp
# sig = hmac_sha256(API_SECRET, concat(nonce, ts))
{
  "amount": 1250,
  "currency": "GBP"
}
HTTP 200
[Asserts]
jsonpath "$.headers['x-nonce']" matches /^[0-9a-f]{16}$/
jsonpath "$.headers.authorization" matches /^HMAC-SHA256 [0-9a-f]{64}$/

# The same signature in Base64
POST {{baseUrl}}/post
Content-Type: application/json
X-Api-Key: {{API_KEY}}
X-Nonce: {{nonce}}
X-Timestamp: {{ts}}
Authorization: HMAC-SHA256 {{sig}}
# [Gen] 3
# nonce = random_hex(16)
# ts = timestamp
# sig = hmac_sha256_b64(API_SECRET, concat(nonce, ts))
{
  "amount": 1250,
  "currency": "GBP"
}
HTTP 200
[Asserts]
jsonpath "$.headers.authorization" matches /^HMAC-SHA256 [A-Za-z0-9+\/]{43}=$/

# A Base64 credential and a digest of the body
PUT {{baseUrl}}/put
Content-Type: application/json
Authorization: Basic {{cred}}
Digest: SHA-256={{body_hash}}
# [Gen] 2
# cred = base64(concat(API_KEY, ":", API_SECRET))
# body_hash = sha256_b64("{\"state\":\"settled\"}")
{"state":"settled"}
HTTP 200
[Asserts]
jsonpath "$.headers.digest" matches /^SHA-256=[A-Za-z0-9+\/]{43}=$/

# A signature over a URL-encoded query
GET {{baseUrl}}/get?since={{SINCE}}&ts={{ts}}
X-Api-Key: {{API_KEY}}
Authorization: HMAC-SHA1 {{sig}}
# [Gen] 3
# ts = timestamp
# msg = concat("GET\n/orders\nsince=", urlencode(SINCE), "&ts=", ts)
# sig = hmac_sha1_b64(API_SECRET, msg)
HTTP 200
[Asserts]
jsonpath "$.args.since" == "2026-01-01T00:00:00Z"

# Known-answer check (RFC 4231 case 2)
GET {{baseUrl}}/get?sig={{sig}}
X-Signature: {{sig}}
# [Gen] 1
# sig = hmac_sha256("Jefe", "what do ya want for nothing?")
HTTP 200
[Asserts]
jsonpath "$.args.sig" == "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"

# Deliberately broken/A typo in a function name
GET {{baseUrl}}/get?sig={{oops}}
# [Gen] 1
# oops = hmac_sha526(API_SECRET, "x")
