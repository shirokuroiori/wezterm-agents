//! Small shared helpers.
//!
//! The timestamp format is compared as a string across three parties (the
//! hook, the TUI, and wezterm.lua — `.jsonl`'s `at` field and `.read`'s
//! contents), so the implementation is consolidated in one place. Store.rs
//! and memo.rs used to each have their own copy of the same function.

use std::time::{SystemTime, UNIX_EPOCH};

/// UNIX time (seconds). Used for `.pending` freshness checks and the
/// `agent_bell` nonce. Both only ever look at differences within the same
/// group of processes, so a timezone isn't needed.
pub fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// RFC3339 with a local offset (e.g. `2026-09-13T07:52:31+09:00`).
///
/// The local timezone offset can't be obtained with std alone. This isn't
/// worth pulling in chrono/time for (it's called once per notification), so
/// `date` is invoked instead, exactly once. Doesn't rely on PATH resolution:
/// when launched from a keybinding, PATH can be a minimal set (same reason
/// as `wezterm_bin()` in wezterm.rs).
///
/// BSD date has `-Iseconds`, but for environments without it, falls back to
/// `+%Y-%m-%dT%H:%M:%S%z` (this comes out as `+0900`, without a colon in the
/// offset, so that's patched up afterward).
pub fn now_rfc3339() -> String {
    for bin in ["/bin/date", "date"] {
        if let Some(s) = run_date(bin, &["-Iseconds"]) {
            return s;
        }
    }
    for bin in ["/bin/date", "date"] {
        if let Some(s) = run_date(bin, &["+%Y-%m-%dT%H:%M:%S%z"]) {
            return with_offset_colon(&s);
        }
    }
    // Reaching this point breaks the read/unread determination (everything
    // becomes unread), but that's better than crashing.
    "1970-01-01T00:00:00+00:00".to_string()
}

fn run_date(bin: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(bin).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// "2026-09-13T07:52:31+0900" -> "2026-09-13T07:52:31+09:00"
/// A string that already has the colon (i.e. equivalent to `-Iseconds`'s
/// output) is returned unchanged.
fn with_offset_colon(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() < 5 {
        return s.to_string();
    }
    let tail = &s[s.len() - 5..];
    let sign_ok = tail.starts_with('+') || tail.starts_with('-');
    if sign_ok && tail[1..].chars().all(|c| c.is_ascii_digit()) {
        return format!("{}{}:{}", &s[..s.len() - 5], &tail[..3], &tail[3..]);
    }
    s.to_string()
}

/// Returns the first "non-empty line". Matches the behavior of jq's
/// `split("\n") | map(select(length > 0)) | .[0]`.
pub fn first_line(s: &str) -> &str {
    s.lines().find(|l| !l.is_empty()).unwrap_or("")
}

/// Truncates by codepoint count, appending `…` at the end if truncated
/// (matches jq's `if (length > 80) then .[0:80] + "…" else . end`).
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

/// Takes the first line and truncates it. Always used as this pair wherever
/// a summary is built.
pub fn first_line_clipped(s: &str, max: usize) -> String {
    clip(first_line(s).trim_end(), max)
}

#[cfg(test)]
#[path = "util_tests.rs"]
mod tests;
