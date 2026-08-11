//! Read-only client for the Postman API (<https://api.postman.com>), used by
//! the "import a whole Postman workspace" flow to list and download an
//! account's collections and environments.
//!
//! Only the handful of `GET` endpoints that flow needs are modelled. The
//! `get_*` calls deliberately return the response body **verbatim** rather
//! than a parsed model: a Postman collection/environment is written to disk
//! exactly as the API sent it, and [`crate::postman`] already understands both
//! the bare export shape and the `{"collection": …}` / `{"environment": …}`
//! envelope the API wraps them in. Keeping the bytes untouched means an import
//! never silently loses a field this module didn't happen to model.
//!
//! Responses are parsed through `serde_json::Value` rather than strict structs
//! for the same reason [`crate::postman`] defaults every field: Postman's docs
//! state that documented enum values "should be considered partial lists and
//! may change over time", so an unexpected shape degrades (an item is skipped)
//! instead of failing a whole import.
//!
//! Everything here is transport-agnostic via the [`Transport`] trait, so the
//! pagination, header-parsing and error-mapping logic is unit-tested with no
//! network and no API key.

// Nothing consumes this module yet — it is the first piece of the Postman
// bulk-import feature, and the import engine that calls it lands next. Remove
// this once that engine exists.

use std::collections::HashSet;
use std::time::Duration;

use serde_json::Value;

/// The default Postman API host.
pub const DEFAULT_BASE_URL: &str = "https://api.postman.com";

/// The host EU-resident Enterprise tenants must use instead of
/// [`DEFAULT_BASE_URL`]. Offered as a setting rather than auto-detected:
/// there is no way to tell from an API key which one applies, and calling the
/// wrong host simply fails to authenticate.
/// Named for the doc comment on [`PostmanClient::new`] and the test that pins
/// the URL building; users type this host into the wizard's "API host" field
/// rather than picking it from a list.
#[cfg_attr(not(test), allow(dead_code))]
pub const EU_BASE_URL: &str = "https://api.eu.postman.com";

/// Minimum spacing between calls to the endpoints Postman rate-limits to
/// **10 calls per 10 seconds**: `GET /workspaces`, `GET /workspaces/{id}` and
/// the `GET /collections` *listing*. Note this is a far tighter budget than
/// the general limit below, and it is the reason a bulk import makes as few
/// listing calls as possible.
pub const STRICT_MIN_INTERVAL: Duration = Duration::from_millis(1_000);

/// Minimum spacing between calls to everything else, which shares the general
/// **300 requests per minute** limit — notably `GET /collections/{uid}` (the
/// single-collection fetch that dominates a bulk import) and
/// `GET /environments`. Five times faster than [`STRICT_MIN_INTERVAL`], so
/// pacing every call as if it were a listing call would make an import take
/// roughly five times longer than it needs to.
pub const GENERAL_MIN_INTERVAL: Duration = Duration::from_millis(200);

/// What one collection fetch actually costs, end to end: the pacing interval
/// above *plus* the round trip, which is what really dominates. A collection is
/// a whole document — every request, script and example in it — so the response
/// is large and the server takes its time building it.
///
/// Calibrated against a real 523-item import whose measured ETA settled around
/// a second an item, an order of magnitude away from what pacing alone
/// predicted. Estimating from [`GENERAL_MIN_INTERVAL`] alone told the user "2
/// minutes" for a download that ran for well over ten, which is worse than no
/// estimate: it is a promise the import cannot keep.
pub const COLLECTION_FETCH_COST: Duration = Duration::from_millis(1_000);

/// The same for one environment fetch. An environment is a short list of
/// key/value pairs, so it comes back markedly faster than a collection — which
/// is why the estimate counts the two kinds separately rather than averaging
/// them into a single per-item figure.
pub const ENVIRONMENT_FETCH_COST: Duration = Duration::from_millis(500);

/// Which rate-limit budget an endpoint draws from. Exposed so the import
/// engine can pace the two independently rather than applying the strictest
/// limit to everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateBucket {
    /// 10 calls per 10 seconds — see [`STRICT_MIN_INTERVAL`].
    Strict,
    /// 300 calls per minute — see [`GENERAL_MIN_INTERVAL`].
    General,
}

impl RateBucket {
    pub fn min_interval(self) -> Duration {
        match self {
            RateBucket::Strict => STRICT_MIN_INTERVAL,
            RateBucket::General => GENERAL_MIN_INTERVAL,
        }
    }
}

/// A raw HTTP response, as produced by a [`Transport`].
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    /// Header names are lowercased so lookups don't have to worry about the
    /// `RateLimit-Remaining` / `ratelimit-remaining` spelling the server used.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// How a request actually gets sent. Implemented by [`CurlTransport`] in
/// production and by a fake in tests, so every behaviour in this module
/// (pagination, retries, error mapping) is testable offline.
pub trait Transport: Send + Sync {
    fn get(&self, url: &str, api_key: &str) -> Result<HttpResponse, String>;
}

/// The real transport: libcurl, which is already in the dependency tree (and
/// statically linked) for the Hurl runner, so this costs no new dependency.
pub struct CurlTransport {
    timeout: Duration,
}

impl Default for CurlTransport {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
        }
    }
}

impl Transport for CurlTransport {
    fn get(&self, url: &str, api_key: &str) -> Result<HttpResponse, String> {
        use curl::easy::{Easy, List};

        let mut easy = Easy::new();
        easy.url(url).map_err(|e| e.to_string())?;
        easy.follow_location(true).map_err(|e| e.to_string())?;
        easy.timeout(self.timeout).map_err(|e| e.to_string())?;
        easy.connect_timeout(Duration::from_secs(20))
            .map_err(|e| e.to_string())?;
        easy.useragent(concat!("paperboy/", env!("CARGO_PKG_VERSION")))
            .map_err(|e| e.to_string())?;

        let mut list = List::new();
        list.append(&format!("x-api-key: {api_key}"))
            .map_err(|e| e.to_string())?;
        list.append("Accept: application/json")
            .map_err(|e| e.to_string())?;
        easy.http_headers(list).map_err(|e| e.to_string())?;

        let mut body = Vec::new();
        let mut headers: Vec<(String, String)> = Vec::new();
        {
            let mut transfer = easy.transfer();
            transfer
                .write_function(|data| {
                    body.extend_from_slice(data);
                    Ok(data.len())
                })
                .map_err(|e| e.to_string())?;
            transfer
                .header_function(|raw| {
                    if let Some((name, value)) = parse_header_line(raw) {
                        headers.push((name, value));
                    }
                    true
                })
                .map_err(|e| e.to_string())?;
            transfer.perform().map_err(|e| e.to_string())?;
        }

        let status = easy.response_code().map_err(|e| e.to_string())? as u16;
        Ok(HttpResponse {
            status,
            headers,
            // A Postman error page could be any encoding; never fail the whole
            // import over a non-UTF-8 byte in a message we only display.
            body: String::from_utf8_lossy(&body).into_owned(),
        })
    }
}

/// Split one raw header line into a lowercased name and its value. Returns
/// `None` for the status line and the blank separator line, which carry no
/// colon.
fn parse_header_line(raw: &[u8]) -> Option<(String, String)> {
    let line = String::from_utf8_lossy(raw);
    let (name, value) = line.split_once(':')?;
    Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
}

/// What went wrong with a Postman API call.
///
/// Every variant's message is redacted (see [`redact`]) before it is built, so
/// an `ApiError` can be shown or logged without leaking the API key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// The key is missing, malformed, expired or revoked.
    Unauthorized,
    /// Authenticated, but not allowed — typically a feature that isn't
    /// available on this plan or in this region.
    Forbidden(String),
    /// No such workspace/collection/environment, or it isn't visible to this
    /// key's user.
    NotFound(String),
    /// A rate or usage limit was hit. `retry_after` is the server's own
    /// "wait this many seconds" figure when it supplied one. `monthly` marks
    /// the *usage* limit (Postman's `serviceLimitExhausted`), which — unlike
    /// the per-minute limit — will not clear by waiting a moment.
    RateLimited {
        retry_after: Option<u64>,
        monthly: bool,
    },
    /// Any other non-success status.
    Http { status: u16, message: String },
    /// The request never completed (DNS, TLS, timeout, connection reset).
    Transport(String),
    /// A 2xx response whose body wasn't the JSON shape this endpoint promises.
    Parse(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized => write!(f, "invalid or expired Postman API key"),
            ApiError::Forbidden(m) => write!(f, "not permitted: {m}"),
            ApiError::NotFound(m) => write!(f, "not found: {m}"),
            ApiError::RateLimited { monthly: true, .. } => {
                write!(f, "Postman API monthly usage limit reached")
            }
            ApiError::RateLimited {
                retry_after: Some(s),
                ..
            } => write!(f, "rate limited by Postman (retry in {s}s)"),
            ApiError::RateLimited { .. } => write!(f, "rate limited by Postman"),
            ApiError::Http { status, message } if message.is_empty() => {
                write!(f, "Postman API returned HTTP {status}")
            }
            ApiError::Http { status, message } => {
                write!(f, "Postman API returned HTTP {status}: {message}")
            }
            ApiError::Transport(m) => write!(f, "could not reach the Postman API: {m}"),
            ApiError::Parse(m) => write!(f, "unexpected response from the Postman API: {m}"),
        }
    }
}

impl ApiError {
    /// Whether retrying the identical call could plausibly succeed. The
    /// monthly usage limit is deliberately *not* retryable — waiting will not
    /// clear it, so an import should stop and say so rather than spin.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ApiError::RateLimited { monthly: false, .. } | ApiError::Transport(_)
        )
    }
}

/// Remove `key` from `msg`, so an API key that found its way into a URL or a
/// transport error can never be displayed, logged or written to a file.
/// Mirrors [`crate::git_remote`]'s handling of access tokens.
pub fn redact(msg: &str, key: &str) -> String {
    if key.is_empty() {
        return msg.to_string();
    }
    msg.replace(key, "***")
}

/// The visibility of a Postman workspace.
///
/// Postman documents this as a partial list, so an unrecognised value is kept
/// verbatim in [`WorkspaceKind::Other`] rather than being dropped or guessed —
/// the workspace still needs to be listed and importable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceKind {
    Personal,
    Team,
    Private,
    Public,
    Partner,
    Other(String),
}

impl WorkspaceKind {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "personal" => WorkspaceKind::Personal,
            "team" => WorkspaceKind::Team,
            "private" => WorkspaceKind::Private,
            "public" => WorkspaceKind::Public,
            "partner" => WorkspaceKind::Partner,
            other => WorkspaceKind::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            WorkspaceKind::Personal => "personal",
            WorkspaceKind::Team => "team",
            WorkspaceKind::Private => "private",
            WorkspaceKind::Public => "public",
            WorkspaceKind::Partner => "partner",
            WorkspaceKind::Other(s) => s,
        }
    }

    /// The kinds a bulk import offers by default. `public` is excluded: a user
    /// can have access to a great many public workspaces they have no interest
    /// in backing up, which would bury their own in the picker.
    pub fn default_selection() -> Vec<WorkspaceKind> {
        vec![
            WorkspaceKind::Personal,
            WorkspaceKind::Team,
            WorkspaceKind::Private,
            WorkspaceKind::Partner,
        ]
    }
}

/// One workspace as returned by `GET /workspaces`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSummary {
    pub id: String,
    pub name: String,
    pub kind: WorkspaceKind,
}

/// One collection or environment in a workspace listing.
///
/// `uid` is the full `{user_id}-{item_id}` identifier and is what the `get_*`
/// calls take; `id` is kept for display and de-duplication. Postman has been
/// known to omit one or the other, so [`ItemSummary::fetch_id`] prefers `uid`
/// and falls back to `id` (the same choice the original backup script made).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemSummary {
    pub uid: String,
    pub id: String,
    pub name: String,
}

impl ItemSummary {
    pub fn fetch_id(&self) -> &str {
        if self.uid.is_empty() {
            &self.id
        } else {
            &self.uid
        }
    }
}

/// The rate-limit state Postman reports on every response.
///
/// Both the `RateLimit-*` and `X-RateLimit-*` spellings are accepted, as is
/// the combined `RateLimit: limit=300, remaining=299, reset=52` form, because
/// Postman documents all three and which you get varies by endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateInfo {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    /// Seconds until the current window resets.
    pub reset_secs: Option<u64>,
    pub limit_month: Option<u64>,
    pub remaining_month: Option<u64>,
    /// Seconds to wait after a 429, when the server supplied one.
    pub retry_after: Option<u64>,
}

impl RateInfo {
    pub fn from_response(res: &HttpResponse) -> Self {
        let num = |name: &str| res.header(name).and_then(|v| v.trim().parse::<u64>().ok());
        let first = |names: &[&str]| names.iter().find_map(|n| num(n));

        let mut info = RateInfo {
            limit: first(&["ratelimit-limit", "x-ratelimit-limit"]),
            remaining: first(&["ratelimit-remaining", "x-ratelimit-remaining"]),
            reset_secs: first(&["ratelimit-reset", "x-ratelimit-reset"]).map(normalize_reset),
            limit_month: first(&["ratelimit-limit-month", "x-ratelimit-limit-month"]),
            remaining_month: first(&["ratelimit-remaining-month", "x-ratelimit-remaining-month"]),
            retry_after: first(&["x-ratelimit-retryafter", "retry-after"]),
        };

        // The combined header fills in whatever the individual ones didn't.
        if let Some(combined) = res.header("ratelimit") {
            let parsed = parse_combined_ratelimit(combined);
            info.limit = info.limit.or(parsed.0);
            info.remaining = info.remaining.or(parsed.1);
            info.reset_secs = info.reset_secs.or(parsed.2.map(normalize_reset));
        }
        info
    }
}

/// Postman documents `RateLimit-Reset` as "seconds until reset" in one place
/// and "UTC epoch seconds" in another, and the two really do both occur. Treat
/// anything implausibly large to be a duration as an absolute timestamp and
/// convert it back to a delta, so a caller always gets "seconds from now".
fn normalize_reset(v: u64) -> u64 {
    // ~31.7 years as a duration is nonsense; as an epoch it is 1971.
    const EPOCH_THRESHOLD: u64 = 1_000_000_000;
    if v < EPOCH_THRESHOLD {
        return v;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    v.saturating_sub(now)
}

/// Parse `limit=300, remaining=299, reset=52` into its three numbers.
fn parse_combined_ratelimit(value: &str) -> (Option<u64>, Option<u64>, Option<u64>) {
    let mut limit = None;
    let mut remaining = None;
    let mut reset = None;
    for part in value.split(',') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let parsed = v.trim().parse::<u64>().ok();
        match k.trim().to_ascii_lowercase().as_str() {
            "limit" => limit = parsed,
            "remaining" => remaining = parsed,
            "reset" => reset = parsed,
            _ => {}
        }
    }
    (limit, remaining, reset)
}

/// A read-only Postman API client bound to one API key and host.
pub struct PostmanClient {
    api_key: String,
    base_url: String,
    transport: Box<dyn Transport>,
}

impl PostmanClient {
    /// A client talking to the real API over libcurl. `base_url` defaults to
    /// [`DEFAULT_BASE_URL`]; EU Enterprise tenants pass [`EU_BASE_URL`].
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        Self::with_transport(api_key, base_url, Box::new(CurlTransport::default()))
    }

    pub fn with_transport(
        api_key: String,
        base_url: Option<String>,
        transport: Box<dyn Transport>,
    ) -> Self {
        let base = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Self {
            api_key,
            base_url: base.trim_end_matches('/').to_string(),
            transport,
        }
    }

    /// Send one request and classify the outcome. The API key is redacted from
    /// every error before it escapes.
    fn call(&self, path_and_query: &str) -> Result<HttpResponse, ApiError> {
        let url = format!("{}{}", self.base_url, path_and_query);
        let res = self
            .transport
            .get(&url, &self.api_key)
            .map_err(|e| ApiError::Transport(redact(&e, &self.api_key)))?;

        if (200..300).contains(&res.status) {
            return Ok(res);
        }

        let message = redact(&extract_error_message(&res.body), &self.api_key);
        Err(match res.status {
            401 => ApiError::Unauthorized,
            403 => ApiError::Forbidden(message),
            404 => ApiError::NotFound(message),
            429 => {
                let info = RateInfo::from_response(&res);
                ApiError::RateLimited {
                    retry_after: info.retry_after.or(info.reset_secs),
                    // Postman distinguishes the per-minute `rateLimited` from
                    // the plan's `serviceLimitExhausted`; only the former
                    // clears on its own.
                    monthly: error_name(&res.body)
                        .is_some_and(|n| n.eq_ignore_ascii_case("serviceLimitExhausted")),
                }
            }
            status => ApiError::Http { status, message },
        })
    }

    /// Every workspace the key's user can see.
    ///
    /// `kinds` filters the result **client-side**. The API does accept a
    /// `type` parameter, but it takes a single value, so filtering server-side
    /// would cost one request per kind — and `GET /workspaces` draws on the
    /// tight 10-calls-per-10-seconds budget (see [`RateBucket::Strict`]). One
    /// unfiltered call plus a local filter is both faster and cheaper. An
    /// empty `kinds` returns everything.
    pub fn list_workspaces(
        &self,
        kinds: &[WorkspaceKind],
    ) -> Result<(Vec<WorkspaceSummary>, RateInfo), ApiError> {
        let mut out: Vec<WorkspaceSummary> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut cursor: Option<String> = None;
        let mut last_rate = RateInfo::default();
        // `GET /workspaces` paginates by cursor. The bound stops a server that
        // keeps handing back a cursor from spinning forever; 100 pages of 100
        // is far past any real account.
        for _ in 0..100 {
            let mut query = String::from("/workspaces?limit=100");
            if let Some(c) = &cursor {
                query.push_str("&cursor=");
                query.push_str(&percent_encode(c));
            }
            let res = self.call(&query)?;
            last_rate = RateInfo::from_response(&res);
            let body = parse_json(&res.body)?;

            let items = array_field(&body, "workspaces");
            let page_len = items.len();
            for item in items {
                let Some(id) = string_field(item, "id") else {
                    continue;
                };
                // A cursor that fails to advance would otherwise re-add the
                // same page until the loop bound is hit.
                if !seen_ids.insert(id.clone()) {
                    continue;
                }
                out.push(WorkspaceSummary {
                    name: string_field(item, "name").unwrap_or_else(|| id.clone()),
                    kind: WorkspaceKind::parse(
                        &string_field(item, "type")
                            .or_else(|| string_field(item, "visibility"))
                            .unwrap_or_default(),
                    ),
                    id,
                });
            }

            let next = next_cursor(&body);
            match next {
                // No cursor, an empty page, or a cursor that didn't move: done.
                Some(c) if page_len > 0 && Some(&c) != cursor.as_ref() => cursor = Some(c),
                _ => break,
            }
        }

        if !kinds.is_empty() {
            out.retain(|w| kinds.contains(&w.kind));
        }
        Ok((out, last_rate))
    }

    /// Every collection in `workspace_id`.
    ///
    /// Note an unknown workspace id is not an error to Postman — it answers
    /// `200` with an empty list — so an empty result here means "no
    /// collections, or no such workspace", and the caller should say so.
    pub fn list_collections(
        &self,
        workspace_id: &str,
    ) -> Result<(Vec<ItemSummary>, RateInfo), ApiError> {
        self.list_items(workspace_id, "collections")
    }

    /// Every environment in `workspace_id`. Unlike `/collections`, this
    /// endpoint does not paginate — one call returns them all.
    pub fn list_environments(
        &self,
        workspace_id: &str,
    ) -> Result<(Vec<ItemSummary>, RateInfo), ApiError> {
        let res = self.call(&format!(
            "/environments?workspace={}",
            percent_encode(workspace_id)
        ))?;
        let rate = RateInfo::from_response(&res);
        let body = parse_json(&res.body)?;
        Ok((collect_items(&body, "environments"), rate))
    }

    /// Shared `limit`/`offset` pagination for the listing endpoints that use
    /// it, driven by `meta.total`. A response with no `meta` (Postman's older
    /// unpaginated behaviour) is treated as a single complete page.
    fn list_items(
        &self,
        workspace_id: &str,
        field: &str,
    ) -> Result<(Vec<ItemSummary>, RateInfo), ApiError> {
        const PAGE: usize = 100;
        let mut out: Vec<ItemSummary> = Vec::new();
        let mut offset = 0usize;
        let mut last_rate;
        loop {
            let res = self.call(&format!(
                "/{field}?workspace={}&limit={PAGE}&offset={offset}",
                percent_encode(workspace_id)
            ))?;
            last_rate = RateInfo::from_response(&res);
            let body = parse_json(&res.body)?;

            let page = collect_items(&body, field);
            let page_len = page.len();
            out.extend(page);

            let total = body
                .get("meta")
                .and_then(|m| m.get("total"))
                .and_then(Value::as_u64)
                .map(|t| t as usize);

            match total {
                Some(total) if out.len() < total && page_len > 0 => offset += PAGE,
                _ => break,
            }
        }
        Ok((out, last_rate))
    }

    /// One collection's full JSON, exactly as the API returned it.
    pub fn get_collection(&self, uid: &str) -> Result<(String, RateInfo), ApiError> {
        self.get_raw("collections", uid)
    }

    /// One environment's full JSON, exactly as the API returned it.
    pub fn get_environment(&self, uid: &str) -> Result<(String, RateInfo), ApiError> {
        self.get_raw("environments", uid)
    }

    fn get_raw(&self, field: &str, uid: &str) -> Result<(String, RateInfo), ApiError> {
        let res = self.call(&format!("/{field}/{}", percent_encode(uid)))?;
        let rate = RateInfo::from_response(&res);
        // Confirm it really is JSON before a caller writes it to disk — a
        // proxy's HTML error page with a 200 status would otherwise be saved
        // as a `.json` collection and fail confusingly much later.
        let value = parse_json(&res.body)?;
        if !value.is_object() {
            return Err(ApiError::Parse(format!(
                "expected a JSON object for {field}/{uid}"
            )));
        }
        Ok((res.body, rate))
    }
}

/// Pull the human-readable part out of Postman's `{"error": {"name": …,
/// "message": …}}` envelope, falling back to the raw body (truncated) when it
/// isn't that shape.
fn extract_error_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body)
        && let Some(err) = v.get("error")
    {
        if let Some(m) = err.get("message").and_then(Value::as_str) {
            return m.to_string();
        }
        if let Some(m) = err.as_str() {
            return m.to_string();
        }
    }
    let trimmed = body.trim();
    if trimmed.len() > 200 {
        format!("{}…", &trimmed[..200])
    } else {
        trimmed.to_string()
    }
}

fn error_name(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("error")?
        .get("name")?
        .as_str()
        .map(str::to_string)
}

fn parse_json(body: &str) -> Result<Value, ApiError> {
    serde_json::from_str::<Value>(body).map_err(|e| ApiError::Parse(e.to_string()))
}

fn array_field<'a>(v: &'a Value, name: &str) -> &'a [Value] {
    v.get(name)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn string_field(v: &Value, name: &str) -> Option<String> {
    v.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The cursor for the next page, wherever this response happens to carry it.
/// Postman documents cursor pagination without pinning down the field's
/// location, so both the `meta` wrapper and the top level are checked.
fn next_cursor(body: &Value) -> Option<String> {
    body.get("meta")
        .and_then(|m| string_field(m, "nextCursor"))
        .or_else(|| string_field(body, "nextCursor"))
}

/// Read a listing array into [`ItemSummary`]s, skipping entries with neither a
/// `uid` nor an `id` (nothing could be fetched for them anyway).
fn collect_items(body: &Value, field: &str) -> Vec<ItemSummary> {
    array_field(body, field)
        .iter()
        .filter_map(|item| {
            let uid = string_field(item, "uid").unwrap_or_default();
            let id = string_field(item, "id").unwrap_or_default();
            if uid.is_empty() && id.is_empty() {
                return None;
            }
            let fallback = if uid.is_empty() { &id } else { &uid };
            Some(ItemSummary {
                name: string_field(item, "name").unwrap_or_else(|| fallback.clone()),
                uid,
                id,
            })
        })
        .collect()
}

/// Percent-encode everything outside the unreserved set, so an id or cursor
/// containing `/`, `&` or a space can't alter the query it lands in.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Turn a Postman item name into something safe to use as a filename on any
/// platform: path separators and control characters become `_`, runs of
/// whitespace collapse, and the result is capped well short of the usual
/// 255-byte limit to leave room for an extension and a de-duplicating suffix.
///
/// Windows additionally rejects names ending in a dot or space and reserves a
/// handful of device names, so both are handled here rather than leaving a
/// workspace that imports on Linux but not elsewhere.
pub fn sanitize_file_name(name: &str) -> String {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    let replaced: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();

    // Collapse whitespace runs so "Orders   API" doesn't become a filename
    // with a stretch of spaces in it.
    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = collapsed.trim().chars().take(180).collect();
    // A leading dot would make the file hidden (and invisible to the workspace
    // scanner, which skips dotfiles).
    while out.starts_with('.') {
        out.remove(0);
    }
    let trimmed = out.trim_end_matches([' ', '.']);
    out = trimmed.to_string();

    if out.is_empty() {
        return "untitled".to_string();
    }
    if RESERVED.iter().any(|r| out.eq_ignore_ascii_case(r)) {
        out.insert(0, '_');
    }
    out
}

/// A filename that isn't in `taken` yet, appending ` (2)`, ` (3)` … as needed,
/// and recording the result.
///
/// Postman happily allows two collections in one workspace to share a name;
/// writing both to `<name>.json` would silently import only the second.
pub fn unique_file_name(stem: &str, extension: &str, taken: &mut HashSet<String>) -> String {
    let base = sanitize_file_name(stem);
    let mut candidate = format!("{base}.{extension}");
    let mut n = 2;
    while !taken.insert(candidate.to_lowercase()) {
        candidate = format!("{base} ({n}).{extension}");
        n += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A [`Transport`] that replays queued responses and records the URLs it
    /// was asked for, so pagination and query building can be asserted without
    /// a network or an API key.
    ///
    /// The recorder is an `Arc` the test keeps a handle on, because the
    /// transport itself is moved into the [`PostmanClient`].
    #[derive(Clone, Default)]
    struct Recorder {
        calls: Arc<Mutex<Vec<String>>>,
        keys: Arc<Mutex<Vec<String>>>,
    }

    impl Recorder {
        fn urls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    struct FakeTransport {
        responses: Mutex<Vec<Result<HttpResponse, String>>>,
        recorder: Recorder,
    }

    impl FakeTransport {
        fn new(responses: Vec<Result<HttpResponse, String>>) -> (Box<Self>, Recorder) {
            let recorder = Recorder::default();
            (
                Box::new(Self {
                    responses: Mutex::new(responses),
                    recorder: recorder.clone(),
                }),
                recorder,
            )
        }
    }

    impl Transport for FakeTransport {
        fn get(&self, url: &str, api_key: &str) -> Result<HttpResponse, String> {
            self.recorder.calls.lock().unwrap().push(url.to_string());
            self.recorder.keys.lock().unwrap().push(api_key.to_string());
            let mut queued = self.responses.lock().unwrap();
            if queued.is_empty() {
                return Err("no queued response".to_string());
            }
            queued.remove(0)
        }
    }

    fn ok(body: &str) -> Result<HttpResponse, String> {
        Ok(HttpResponse {
            status: 200,
            headers: Vec::new(),
            body: body.to_string(),
        })
    }

    fn ok_with(body: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, String> {
        Ok(HttpResponse {
            status: 200,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.to_string(),
        })
    }

    fn fail(status: u16, body: &str) -> Result<HttpResponse, String> {
        Ok(HttpResponse {
            status,
            headers: Vec::new(),
            body: body.to_string(),
        })
    }

    fn client(responses: Vec<Result<HttpResponse, String>>) -> (PostmanClient, Recorder) {
        let (transport, recorder) = FakeTransport::new(responses);
        let c = PostmanClient::with_transport("secret-key".to_string(), None, transport);
        (c, recorder)
    }

    #[test]
    fn a_workspace_listing_is_read_into_summaries_with_its_visibility() {
        let (c, _) = client(vec![ok(r#"{"workspaces":[
                {"id":"w1","name":"Team API","type":"team"},
                {"id":"w2","name":"Mine","type":"personal"}
            ]}"#)]);
        let (ws, _) = c.list_workspaces(&[]).unwrap();
        assert_eq!(
            ws,
            vec![
                WorkspaceSummary {
                    id: "w1".into(),
                    name: "Team API".into(),
                    kind: WorkspaceKind::Team
                },
                WorkspaceSummary {
                    id: "w2".into(),
                    name: "Mine".into(),
                    kind: WorkspaceKind::Personal
                },
            ]
        );
    }

    #[test]
    fn workspace_kinds_filter_client_side_and_default_selection_excludes_public() {
        let body = r#"{"workspaces":[
            {"id":"w1","name":"Mine","type":"personal"},
            {"id":"w2","name":"Someone else's","type":"public"}
        ]}"#;
        let (c, _) = client(vec![ok(body)]);
        let (ws, _) = c
            .list_workspaces(&WorkspaceKind::default_selection())
            .unwrap();
        assert_eq!(ws.len(), 1, "the public workspace is filtered out");
        assert_eq!(ws[0].id, "w1");
    }

    #[test]
    fn an_unknown_workspace_visibility_is_preserved_rather_than_dropped() {
        let (c, _) = client(vec![ok(
            r#"{"workspaces":[{"id":"w1","name":"New","type":"something-new"}]}"#,
        )]);
        let (ws, _) = c.list_workspaces(&[]).unwrap();
        assert_eq!(ws[0].kind, WorkspaceKind::Other("something-new".into()));
    }

    #[test]
    fn workspace_listing_follows_the_cursor_until_it_runs_out() {
        let (c, _) = client(vec![
            ok(
                r#"{"workspaces":[{"id":"w1","name":"A","type":"team"}],"meta":{"nextCursor":"abc"}}"#,
            ),
            ok(r#"{"workspaces":[{"id":"w2","name":"B","type":"team"}]}"#),
        ]);
        let (ws, _) = c.list_workspaces(&[]).unwrap();
        assert_eq!(ws.len(), 2);
    }

    #[test]
    fn a_cursor_that_never_advances_terminates_instead_of_looping_forever() {
        // Same cursor every time and the same page — a well-behaved client must
        // still stop.
        let page =
            r#"{"workspaces":[{"id":"w1","name":"A","type":"team"}],"meta":{"nextCursor":"same"}}"#;
        let (c, _) = client(vec![ok(page), ok(page), ok(page), ok(page)]);
        let (ws, _) = c.list_workspaces(&[]).unwrap();
        assert_eq!(ws.len(), 1, "the repeated page is not counted twice");
    }

    #[test]
    fn collections_paginate_by_offset_until_meta_total_is_reached() {
        let page1 = format!(
            r#"{{"collections":[{}],"meta":{{"total":3,"limit":100,"offset":0}}}}"#,
            (0..2)
                .map(|i| format!(r#"{{"id":"c{i}","uid":"u-c{i}","name":"C{i}"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let page2 = r#"{"collections":[{"id":"c2","uid":"u-c2","name":"C2"}],"meta":{"total":3,"limit":100,"offset":100}}"#;
        let (c, rec) = client(vec![ok(&page1), ok(page2)]);
        let (items, _) = c.list_collections("ws").unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[2].fetch_id(), "u-c2");
        assert_eq!(
            rec.urls(),
            vec![
                "https://api.postman.com/collections?workspace=ws&limit=100&offset=0",
                "https://api.postman.com/collections?workspace=ws&limit=100&offset=100",
            ],
            "the offset advances by a full page each time"
        );
    }

    #[test]
    fn workspace_paging_sends_the_cursor_it_was_given() {
        let (c, rec) = client(vec![
            ok(
                r#"{"workspaces":[{"id":"w1","name":"A","type":"team"}],"meta":{"nextCursor":"c+2"}}"#,
            ),
            ok(r#"{"workspaces":[{"id":"w2","name":"B","type":"team"}]}"#),
        ]);
        c.list_workspaces(&[]).unwrap();
        assert_eq!(
            rec.urls(),
            vec![
                "https://api.postman.com/workspaces?limit=100",
                "https://api.postman.com/workspaces?limit=100&cursor=c%2B2",
            ],
            "the cursor is passed back percent-encoded"
        );
    }

    #[test]
    fn a_listing_without_meta_is_treated_as_one_complete_page() {
        let (c, _) = client(vec![ok(
            r#"{"collections":[{"id":"c0","uid":"u0","name":"Only"}]}"#,
        )]);
        let (items, _) = c.list_collections("ws").unwrap();
        assert_eq!(items.len(), 1, "no second request is made without meta");
    }

    #[test]
    fn environments_are_fetched_in_a_single_unpaginated_call() {
        let (c, rec) = client(vec![ok(
            r#"{"environments":[{"id":"e1","uid":"u-e1","name":"Prod"}]}"#,
        )]);
        let (items, _) = c.list_environments("ws-1").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Prod");
        assert_eq!(
            rec.urls(),
            vec!["https://api.postman.com/environments?workspace=ws-1"],
            "environments take exactly one call, with no paging parameters"
        );
    }

    #[test]
    fn an_item_with_no_uid_falls_back_to_its_id_for_fetching() {
        let (c, _) = client(vec![ok(r#"{"collections":[{"id":"only-id","name":"C"}]}"#)]);
        let (items, _) = c.list_collections("ws").unwrap();
        assert_eq!(items[0].fetch_id(), "only-id");
    }

    #[test]
    fn an_item_with_neither_id_nor_uid_is_skipped_rather_than_failing_the_listing() {
        let (c, _) = client(vec![ok(
            r#"{"collections":[{"name":"Broken"},{"id":"ok","name":"Fine"}]}"#,
        )]);
        let (items, _) = c.list_collections("ws").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "ok");
    }

    #[test]
    fn a_fetched_collection_is_returned_byte_for_byte() {
        let raw = r#"{"collection":{"info":{"name":"X"},"item":[],"unmodelled":42}}"#;
        let (c, _) = client(vec![ok(raw)]);
        let (body, _) = c.get_collection("u-1").unwrap();
        assert_eq!(body, raw, "nothing is reserialised or dropped");
    }

    #[test]
    fn a_non_json_body_with_a_success_status_is_a_parse_error_not_saved_to_disk() {
        let (c, _) = client(vec![ok("<html>proxy error</html>")]);
        assert!(matches!(c.get_collection("u-1"), Err(ApiError::Parse(_))));
    }

    #[test]
    fn ids_are_percent_encoded_into_the_query_so_they_cannot_alter_it() {
        let (c, rec) = client(vec![ok(r#"{"environments":[]}"#)]);
        c.list_environments("a b/c&d").unwrap();
        assert_eq!(
            rec.urls(),
            vec!["https://api.postman.com/environments?workspace=a%20b%2Fc%26d"]
        );
    }

    #[test]
    fn the_api_key_is_sent_as_the_x_api_key_header_value_on_every_call() {
        let (c, rec) = client(vec![ok(r#"{"environments":[]}"#)]);
        c.list_environments("ws").unwrap();
        assert_eq!(rec.keys.lock().unwrap().as_slice(), ["secret-key"]);
    }

    #[test]
    fn the_base_url_is_configurable_for_eu_tenants_and_its_trailing_slash_ignored() {
        let (transport, rec) = FakeTransport::new(vec![ok(r#"{"workspaces":[]}"#)]);
        let c =
            PostmanClient::with_transport("k".into(), Some(format!("{EU_BASE_URL}/")), transport);
        c.list_workspaces(&[]).unwrap();
        assert_eq!(
            rec.urls(),
            vec![format!("{EU_BASE_URL}/workspaces?limit=100")],
            "no doubled slash, and the EU host is used"
        );
    }

    #[test]
    fn http_statuses_map_onto_their_api_errors() {
        let cases: Vec<(u16, &str, ApiError)> = vec![
            (401, "{}", ApiError::Unauthorized),
            (
                403,
                r#"{"error":{"message":"This feature isn't available in your region."}}"#,
                ApiError::Forbidden("This feature isn't available in your region.".into()),
            ),
            (
                404,
                r#"{"error":{"message":"not found"}}"#,
                ApiError::NotFound("not found".into()),
            ),
        ];
        for (status, body, expected) in cases {
            let (c, _) = client(vec![fail(status, body)]);
            assert_eq!(c.list_environments("ws").unwrap_err(), expected);
        }
    }

    #[test]
    fn a_per_minute_429_is_retryable_and_carries_the_servers_retry_after() {
        let res = Ok(HttpResponse {
            status: 429,
            headers: vec![("x-ratelimit-retryafter".into(), "17".into())],
            body: r#"{"error":{"name":"rateLimited","message":"slow down"}}"#.into(),
        });
        let (c, _) = client(vec![res]);
        let err = c.list_environments("ws").unwrap_err();
        assert_eq!(
            err,
            ApiError::RateLimited {
                retry_after: Some(17),
                monthly: false
            }
        );
        assert!(err.is_retryable());
    }

    #[test]
    fn the_monthly_usage_limit_is_not_retryable_because_waiting_will_not_clear_it() {
        let (c, _) = client(vec![fail(
            429,
            r#"{"error":{"name":"serviceLimitExhausted","message":"out of calls"}}"#,
        )]);
        let err = c.list_environments("ws").unwrap_err();
        assert_eq!(
            err,
            ApiError::RateLimited {
                retry_after: None,
                monthly: true
            }
        );
        assert!(!err.is_retryable());
    }

    #[test]
    fn a_transport_failure_never_leaks_the_api_key() {
        let (transport, _) = FakeTransport::new(vec![Err(
            "failed to connect to https://api.postman.com?key=secret-key".into(),
        )]);
        let c = PostmanClient::with_transport("secret-key".into(), None, transport);
        let err = c.list_environments("ws").unwrap_err();
        let text = err.to_string();
        assert!(!text.contains("secret-key"), "got: {text}");
        assert!(text.contains("***"), "got: {text}");
    }

    #[test]
    fn an_error_body_that_is_not_postmans_envelope_is_truncated_not_dumped() {
        let long = "x".repeat(500);
        let (c, _) = client(vec![fail(500, &long)]);
        match c.list_environments("ws").unwrap_err() {
            ApiError::Http { status, message } => {
                assert_eq!(status, 500);
                assert!(
                    message.chars().count() <= 201,
                    "got {} chars",
                    message.len()
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rate_headers_are_read_in_both_spellings_and_from_the_combined_header() {
        let res = HttpResponse {
            status: 200,
            headers: vec![
                ("x-ratelimit-remaining".into(), "42".into()),
                ("ratelimit-remaining-month".into(), "812".into()),
                (
                    "ratelimit".into(),
                    "limit=300, remaining=299, reset=52".into(),
                ),
            ],
            body: String::new(),
        };
        let info = RateInfo::from_response(&res);
        assert_eq!(info.remaining, Some(42), "the explicit header wins");
        assert_eq!(info.limit, Some(300), "filled in from the combined header");
        assert_eq!(info.reset_secs, Some(52));
        assert_eq!(info.remaining_month, Some(812));
    }

    #[test]
    fn an_epoch_style_reset_is_converted_back_to_seconds_from_now() {
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 30;
        let res = HttpResponse {
            status: 200,
            headers: vec![("x-ratelimit-reset".into(), future.to_string())],
            body: String::new(),
        };
        let secs = RateInfo::from_response(&res).reset_secs.unwrap();
        assert!((25..=35).contains(&secs), "got {secs}");
    }

    #[test]
    fn a_listing_response_still_reports_its_rate_state() {
        let (c, _) = client(vec![ok_with(
            r#"{"environments":[]}"#,
            &[("ratelimit-remaining", "7")],
        )]);
        let (_, rate) = c.list_environments("ws").unwrap();
        assert_eq!(rate.remaining, Some(7));
    }

    #[test]
    fn the_two_rate_buckets_differ_because_listing_calls_are_limited_far_harder() {
        assert!(RateBucket::Strict.min_interval() > RateBucket::General.min_interval());
    }

    #[test]
    fn filenames_lose_separators_control_characters_and_leading_dots() {
        assert_eq!(sanitize_file_name("Orders/v2"), "Orders_v2");
        assert_eq!(sanitize_file_name("a\u{0}b"), "a_b");
        assert_eq!(sanitize_file_name(".hidden"), "hidden");
        assert_eq!(sanitize_file_name("Orders   API"), "Orders API");
        assert_eq!(sanitize_file_name("   "), "untitled");
        assert_eq!(sanitize_file_name("trailing. "), "trailing");
    }

    #[test]
    fn a_windows_reserved_name_is_prefixed_so_the_import_works_everywhere() {
        assert_eq!(sanitize_file_name("CON"), "_CON");
        assert_eq!(sanitize_file_name("com1"), "_com1");
    }

    #[test]
    fn a_very_long_name_is_capped_short_of_the_filesystem_limit() {
        let out = sanitize_file_name(&"a".repeat(500));
        assert_eq!(out.chars().count(), 180);
    }

    #[test]
    fn two_collections_sharing_a_name_get_distinct_files_instead_of_overwriting() {
        let mut taken = HashSet::new();
        assert_eq!(
            unique_file_name("Orders", "json", &mut taken),
            "Orders.json"
        );
        assert_eq!(
            unique_file_name("Orders", "json", &mut taken),
            "Orders (2).json"
        );
        assert_eq!(
            unique_file_name("Orders", "json", &mut taken),
            "Orders (3).json"
        );
    }

    #[test]
    fn de_duplication_is_case_insensitive_because_some_filesystems_are() {
        let mut taken = HashSet::new();
        assert_eq!(
            unique_file_name("Orders", "json", &mut taken),
            "Orders.json"
        );
        assert_eq!(
            unique_file_name("orders", "json", &mut taken),
            "orders (2).json"
        );
    }

    #[test]
    fn a_header_line_splits_into_a_lowercased_name_and_trimmed_value() {
        assert_eq!(
            parse_header_line(b"RateLimit-Remaining: 42\r\n"),
            Some(("ratelimit-remaining".to_string(), "42".to_string()))
        );
        assert_eq!(parse_header_line(b"HTTP/2 200\r\n"), None);
        assert_eq!(parse_header_line(b"\r\n"), None);
    }

    #[test]
    fn redaction_leaves_a_message_alone_when_there_is_no_key_to_remove() {
        assert_eq!(redact("plain message", ""), "plain message");
    }
}
