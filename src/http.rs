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
    /// Why the request never left, when a `# [Gen]` row was what stopped it.
    /// `error` carries the same finding as one English sentence, because the
    /// runner is front-end agnostic and has no `Strings`; this is the same
    /// thing structurally, so a front-end that *does* know the language can
    /// say it the way the pre-flight check says it ("Generated values not
    /// set: ...") rather than prefixing the English text with "Request
    /// error:", which names the wrong subject -- there was no request.
    pub gen_errors: Vec<crate::generators::GenError>,
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
        self.gen_errors.clear();
    }

    /// The error to show the reader, in their language: a generator failure is
    /// the pre-flight report verbatim, anything else is the runner's own
    /// message under the "Request error:" heading. Empty when the request did
    /// not fail.
    pub fn error_text(&self, s: &crate::i18n::Strings) -> String {
        if !self.gen_errors.is_empty() {
            return crate::i18n::Status::GeneratorErrors(self.gen_errors.clone()).text(s);
        }
        if self.error.is_empty() {
            return String::new();
        }
        format!("{} {}", s.req_error_prefix, self.error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generators::GenError;
    use crate::i18n::{Language, Strings};

    /// A request refused before it left is not a "Request error": there was no
    /// request. The runner has no `Strings` and says so in one English
    /// sentence; the front-end has both, and must say what the pre-flight
    /// check says -- in the reader's language, under the same heading.
    #[test]
    fn a_refused_send_is_reported_the_way_the_check_reports_it() {
        let errors = vec![GenError::UndefinedReference {
            name: "broken".into(),
            reference: "nothing_defines_this".into(),
        }];
        let r = ApiResponse {
            error: "broken: nothing defines nothing_defines_this".into(),
            gen_errors: errors.clone(),
            ..Default::default()
        };
        for lang in [Language::English, Language::French, Language::Danish] {
            let s = Strings::for_language(&lang);
            assert_eq!(
                r.error_text(&s),
                crate::i18n::Status::GeneratorErrors(errors.clone()).text(&s),
                "the refusal reads exactly as the pre-flight check reads"
            );
            assert!(
                !r.error_text(&s).contains(s.req_error_prefix),
                "and not under a heading naming a request that was never made"
            );
        }
    }

    /// Everything else still is a runner error, and still says so.
    #[test]
    fn a_transport_failure_keeps_the_runner_heading() {
        let r = ApiResponse {
            error: "connection refused".into(),
            ..Default::default()
        };
        let s = Strings::for_language(&Language::English);
        assert_eq!(r.error_text(&s), "Request error: connection refused");
        assert!(
            ApiResponse::default().error_text(&s).is_empty(),
            "a request that has not failed has nothing to say"
        );
    }

    /// `begin` clears the last refusal along with the last response: without
    /// it a send that succeeds would still be described by the generator
    /// failure that stopped the previous one.
    #[test]
    fn starting_a_send_forgets_the_last_refusal() {
        let mut r = ApiResponse {
            gen_errors: vec![GenError::Cycle { name: "a".into() }],
            ..Default::default()
        };
        r.begin();
        assert!(r.gen_errors.is_empty());
    }
}
