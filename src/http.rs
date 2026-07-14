//! Shared response state written by the background request runner and read by
//! the TUI. The HTTP request itself is performed by the Hurl runner (see
//! [`crate::hurl::run_hurl`]).

use std::sync::Arc;

use crate::hurl::AssertOutcome;

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
    }
}
