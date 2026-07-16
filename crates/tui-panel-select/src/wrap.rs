//! Character-exact line wrapping primitives.
//!
//! These back both a panel's *rendering* (turning one raw line into the
//! wrapped rows actually shown) and its *selection geometry* (mapping
//! between logical positions and on-screen rows) — see [`crate::wrapcache`].
//! Wrapping is character-exact (not word-aware): a line is broken every
//! `width` display columns, matching how a raw HTTP response body or JSON
//! preview is displayed.

use ratatui::text::{Line, Span};

/// Wrap a (possibly multi-span, styled) [`Line`] to `width` columns,
/// breaking exactly on the character boundary and preserving each span's
/// style across the break. `width == 0` returns the line unchanged.
pub fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![line];
    }
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;
    for span in line.spans {
        let mut remaining: &str = span.content.as_ref();
        loop {
            if remaining.is_empty() {
                break;
            }
            let avail = width - cur_w;
            if avail == 0 {
                out.push(Line::from(std::mem::take(&mut cur)));
                cur_w = 0;
                continue;
            }
            let take: String = remaining.chars().take(avail).collect();
            cur_w += take.chars().count();
            remaining = &remaining[take.len()..];
            cur.push(Span::styled(take, span.style));
        }
    }
    out.push(Line::from(cur));
    out
}

/// Wrap only a bounded window of a single (unstyled) line — skip
/// `skip_rows` whole wrapped rows, then wrap at most `max_rows` more,
/// without ever touching or allocating the rest of the line. Unlike
/// [`wrap_line`], whose cost is proportional to the *entire* line, this
/// keeps cost proportional to `(skip_rows + max_rows) * width` — critical
/// for panels holding one enormous raw line (e.g. a large base64 blob or
/// minified JSON body pasted with no newlines), where re-wrapping the
/// whole line on every redraw would grind the app to a halt regardless of
/// how few rows are actually on screen.
pub fn wrap_line_window(
    text: &str,
    width: usize,
    skip_rows: usize,
    max_rows: usize,
) -> Vec<Line<'static>> {
    if max_rows == 0 {
        return Vec::new();
    }
    if width == 0 {
        return if skip_rows == 0 {
            vec![Line::raw(text.to_string())]
        } else {
            Vec::new()
        };
    }
    let skip_chars = skip_rows.saturating_mul(width);
    let take_chars = max_rows.saturating_mul(width);
    let windowed: String = text.chars().skip(skip_chars).take(take_chars).collect();
    if windowed.is_empty() {
        return Vec::new();
    }
    wrap_line(Line::raw(windowed), width)
}

/// Number of wrapped rows a line of `char_len` characters produces at
/// `width` display columns, matching [`wrap_line`]'s own boundary math
/// exactly (one row minimum, even for an empty line). Used to size the
/// total scrollable extent and locate a scroll position without ever
/// wrapping every line.
pub fn wrapped_row_count(char_len: usize, width: usize) -> usize {
    if width == 0 || char_len == 0 {
        1
    } else {
        char_len.div_ceil(width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `wrapped_row_count` must match `wrap_line`'s own boundary math
    /// exactly, since the wrap cache relies on it to size the total
    /// scrollable extent and locate a scroll position without ever wrapping
    /// every line.
    #[test]
    fn wrapped_row_count_matches_wrap_line_for_various_lengths() {
        for (len, width) in [(0, 10), (1, 10), (10, 10), (11, 10), (25, 10), (7, 0)] {
            let text = "x".repeat(len);
            let actual = wrap_line(Line::raw(text), width).len();
            assert_eq!(
                wrapped_row_count(len, width),
                actual,
                "len={len} width={width}"
            );
        }
    }
}
