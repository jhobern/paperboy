//! Shared response state written by the background request runner and read by
//! the TUI. The HTTP request itself is performed by the Hurl runner (see
//! [`crate::hurl::run_hurl`]).

use std::sync::Arc;

use crate::hurl::AssertOutcome;

/// Common HTTP header names offered as autocomplete suggestions for the Key
/// field of the New Request headers table. Kept in a sensible display order.
pub const COMMON_HEADERS: &[&str] = &[
    "Accept",
    "Accept-Charset",
    "Accept-Encoding",
    "Accept-Language",
    "Authorization",
    "Cache-Control",
    "Connection",
    "Content-Length",
    "Content-Type",
    "Cookie",
    "Date",
    "ETag",
    "Expect",
    "Host",
    "If-Match",
    "If-Modified-Since",
    "If-None-Match",
    "Origin",
    "Pragma",
    "Range",
    "Referer",
    "User-Agent",
    "X-Api-Key",
    "X-Content-Type-Options",
    "X-Correlation-ID",
    "X-CSRF-Token",
    "X-Forwarded-For",
    "X-Forwarded-Host",
    "X-Forwarded-Proto",
    "X-Frame-Options",
    "X-Request-ID",
    "X-Requested-With",
];

/// Common header names matching `query` (case-insensitive substring). An empty
/// query returns the full list.
pub fn filter_headers(query: &str) -> Vec<&'static str> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return COMMON_HEADERS.to_vec();
    }
    COMMON_HEADERS
        .iter()
        .copied()
        .filter(|h| h.to_ascii_lowercase().contains(&q))
        .collect()
}

/// Response state shared between the UI thread and background request threads.
#[derive(Debug, Default, Clone)]
pub struct ApiResponse {
    pub status: u16,
    pub status_text: String,
    /// The response body. `Arc<str>` (not `String`) so the TUI can clone it
    /// for a fresh draw in O(1) — a cheap refcount bump — instead of a full
    /// memcpy every frame, and so its wrap/line cache
    /// (`tui::wrapcache::PanelWrap`) can detect "unchanged since last frame"
    /// via a pointer comparison rather than a byte-for-byte compare. `Arc`
    /// (not `Rc`) because responses are produced on a background thread and
    /// sent to the UI thread across an `mpsc` channel / shared `Mutex`.
    pub body: Arc<str>,
    pub loading: bool,
    pub error: String,
    /// Response headers (name, value).
    pub headers: Vec<(String, String)>,
    /// Results of evaluating the run entry's `[Asserts]` against this response.
    pub assert_results: Vec<AssertOutcome>,
    /// Wall-clock duration of the HTTP transfer for this request, in
    /// milliseconds, as reported by the Hurl runner (the same figure reports
    /// surface as the per-request "Time" column). `None` when unknown — e.g. a
    /// response constructed before a run completed, or a transport error with
    /// no timing.
    pub duration_ms: Option<u64>,
}

impl ApiResponse {
    /// Reset to the "in-flight" state before dispatching a new request.
    pub fn begin(&mut self) {
        self.loading = true;
        self.status = 0;
        self.status_text.clear();
        self.body = Arc::from("");
        self.error.clear();
        self.headers.clear();
        self.assert_results.clear();
        self.duration_ms = None;
    }
}
