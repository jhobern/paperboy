//! Copy-on-mouse-up for panel-scoped text selection.
//!
//! Primary path: pipe the text into a locally installed clipboard utility
//! (`wl-copy`/`xclip`/`xsel`/`pbcopy`/`clip.exe`, whichever fits the active
//! display server) — this works unconditionally on a local desktop session
//! regardless of whether the terminal emulator itself honours clipboard
//! escape sequences (several common terminals/multiplexer configs simply
//! don't, which is what this fallback chain exists to work around).
//!
//! Fallback: the OSC 52 "set clipboard" escape sequence, written straight to
//! the terminal (bypassing ratatui's own buffer, so it works regardless of
//! the alternate screen / raw mode). This is what actually reaches the
//! clipboard over SSH/remote sessions where no local clipboard tool is
//! reachable, provided the terminal supports OSC 52 — needs no platform
//! clipboard crate, since `base64` is already a dependency for other
//! features.

use std::io::{self, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// Build the OSC 52 escape sequence that asks the terminal to set the
/// system clipboard ("c" selection) to `text`. When running inside tmux the
/// raw sequence needs wrapping in a DCS passthrough (`\x1bPtmux;...\x1b\\`)
/// with any embedded ESC bytes doubled, or tmux swallows it instead of
/// relaying it to the outer terminal.
pub fn osc52_sequence(text: &str, in_tmux: bool) -> String {
    let encoded = STANDARD.encode(text.as_bytes());
    let inner = format!("\x1b]52;c;{encoded}\x07");
    if in_tmux {
        let escaped = inner.replace('\x1b', "\x1b\x1b");
        format!("\x1bPtmux;{escaped}\x1b\\")
    } else {
        inner
    }
}

/// Candidate local clipboard tools to try, in order, given which display
/// server (if any) is active. A pure function (no I/O) so the *selection*
/// logic stays unit-testable without ever actually spawning a process — the
/// real spawning happens only in `try_external_clipboard`, which is
/// compiled out of test builds entirely (see its own doc comment).
fn clipboard_candidates(
    has_wayland: bool,
    has_x11: bool,
    is_macos: bool,
) -> Vec<(&'static str, &'static [&'static str])> {
    let mut out: Vec<(&'static str, &'static [&'static str])> = Vec::new();
    if has_wayland {
        out.push(("wl-copy", &[]));
    }
    if has_x11 {
        out.push(("xclip", &["-selection", "clipboard"]));
        out.push(("xsel", &["--clipboard", "--input"]));
    }
    if is_macos {
        out.push(("pbcopy", &[]));
    }
    // WSL's bridge to the Windows clipboard; harmless to always offer last —
    // spawning it simply fails (and falls through) everywhere else.
    out.push(("clip.exe", &[]));
    out
}

/// Try each candidate local clipboard tool (see `clipboard_candidates`) in
/// turn, piping `text` into its stdin. Returns `true` on the first one that
/// starts and accepts the write. Deliberately doesn't wait for the child to
/// exit: `wl-copy`/`xclip` both fork themselves into the background to keep
/// serving the selection once they own it, so waiting would either block
/// pointlessly or race their own detach.
///
/// Compiled only outside test builds: spawning a real clipboard tool from
/// an automated test would mutate whatever desktop clipboard happens to be
/// reachable from the machine running the tests, which a test run must
/// never do.
#[cfg(not(test))]
fn try_external_clipboard(text: &str) -> bool {
    use std::process::{Command, Stdio};

    let has_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let has_x11 = std::env::var_os("DISPLAY").is_some();
    let is_macos = cfg!(target_os = "macos");
    for (cmd, args) in clipboard_candidates(has_wayland, has_x11, is_macos) {
        let Ok(mut child) = Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let Some(mut stdin) = child.stdin.take() else {
            continue;
        };
        if stdin.write_all(text.as_bytes()).is_ok() {
            return true;
        }
    }
    false
}

/// Copy `text` to the system clipboard: try a local clipboard tool first
/// (see `try_external_clipboard`), falling back to an OSC 52 escape
/// sequence written directly to stdout when none is available (or in test
/// builds, where the external-tool path never runs at all). Best-effort:
/// failures are silently ignored since this is a convenience, not core
/// functionality.
pub fn copy_to_clipboard(text: &str) {
    if text.is_empty() {
        return;
    }
    #[cfg(not(test))]
    {
        if try_external_clipboard(text) {
            return;
        }
    }
    let in_tmux = std::env::var_os("TMUX").is_some();
    let seq = osc52_sequence(text, in_tmux);
    let _ = io::stdout().write_all(seq.as_bytes());
    let _ = io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_plain_osc52_sequence_outside_tmux() {
        let seq = osc52_sequence("hi", false);
        assert_eq!(seq, format!("\x1b]52;c;{}\x07", STANDARD.encode(b"hi")));
    }

    #[test]
    fn wraps_the_sequence_in_a_tmux_dcs_passthrough_and_doubles_escapes() {
        let seq = osc52_sequence("hi", true);
        let inner = format!("\x1b]52;c;{}\x07", STANDARD.encode(b"hi"));
        let expected = format!("\x1bPtmux;{}\x1b\\", inner.replace('\x1b', "\x1b\x1b"));
        assert_eq!(seq, expected);
        assert!(seq.starts_with("\x1bPtmux;"));
        assert!(seq.ends_with("\x1b\\"));
    }

    #[test]
    fn empty_text_still_produces_a_valid_sequence() {
        // copy_to_clipboard() itself short-circuits on empty text, but the
        // pure sequence builder should still behave sanely if ever called
        // directly with an empty string.
        let seq = osc52_sequence("", false);
        assert_eq!(seq, "\x1b]52;c;\x07");
    }

    #[test]
    fn wayland_is_tried_before_x11_tools_when_both_are_present() {
        let c = clipboard_candidates(true, true, false);
        assert_eq!(c[0].0, "wl-copy");
        assert!(c.iter().any(|(name, _)| *name == "xclip"));
        assert!(c.iter().any(|(name, _)| *name == "xsel"));
    }

    #[test]
    fn x11_tools_are_offered_without_wayland() {
        let c = clipboard_candidates(false, true, false);
        assert_eq!(c[0].0, "xclip");
        assert!(!c.iter().any(|(name, _)| *name == "wl-copy"));
    }

    #[test]
    fn macos_offers_pbcopy() {
        let c = clipboard_candidates(false, false, true);
        assert!(c.iter().any(|(name, _)| *name == "pbcopy"));
    }

    #[test]
    fn clip_exe_is_always_offered_as_a_last_resort_for_wsl() {
        let c = clipboard_candidates(false, false, false);
        assert_eq!(c, vec![("clip.exe", &[][..])]);
    }
}
