//! The hook implementation for agents (Claude Code / GitHub Copilot CLI).
//!
//! A port of the old `.claude/hooks/wezterm-notify.sh` and
//! `.copilot/hooks/wezterm-notify.sh` (two nearly-identical bash scripts).
//! Three reasons for dropping bash:
//!
//! - The dependency on `jq` / `perl` / `base64`. `jq` in particular isn't
//!   installed by default, and on machines without it the old script would
//!   silently do nothing via `exit 0`.
//! - The same logic was duplicated across two files, and there were
//!   incidents where only one of them got fixed.
//! - Parsing the transcript is naturally this side's job anyway, since it
//!   already depends on `serde_json`.
//!
//! ## Usage
//!
//! ```text
//! wezterm-agents hook --agent <name> <pretool|waiting|done|working>
//! ```
//!
//! Called from the agent's own hook configuration; mapping that agent's
//! event names onto these four is the configuration file's job (the
//! adapter). stdin carries the JSON payload the agent sends.
//!
//! ## Where output goes
//!
//! - OSC 1337 SetUserVar → the pane's real tty (used by wezterm.lua for tab
//!   color)
//! - `<status_dir>/<pane>.jsonl` → notification history (the TUI's unread
//!   count and history)
//! - `<status_dir>/<pane>.pending` → the most recent tool summary (used for
//!   the waiting message)
//! - `<status_dir>/<pane>.working` → the send timestamp (used to infer
//!   Copilot's working state)
//! - `~/.weztermemo/tab-<tab_id>.md` → one log line appended on done
//!
//! The hook runs as a subprocess of Claude Code / Copilot CLI and can lack
//! a controlling terminal (`/dev/tty`) — confirmed in practice. So instead
//! of relying on writing to `/dev/tty` directly, it uses `wezterm cli`
//! (which talks to the mux over a Unix socket) to look up the real tty from
//! `$WEZTERM_PANE` and writes there.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::lang::Lang;
use crate::memo;
use crate::model::task_from_title;
use crate::paths;
use crate::util::{clip, epoch_secs, first_line, first_line_clipped, now_rfc3339};
use crate::wezterm;

/// Max summary length (in code points). Matches the old bash version's jq
/// `.[0:80]`.
const SUMMARY_MAX: usize = 80;

/// Freshness cutoff (seconds) for treating `.pending` as belonging to the
/// current waiting event.
///
/// Without this, a stale `.pending` would get reused for an unrelated
/// waiting event (e.g. a 60-second idle input prompt), attributing the
/// wrong tool to it.
const PENDING_MAX_AGE: u64 = 5;

/// Max line count for `.jsonl`, and how many lines to keep once it's exceeded.
const JSONL_MAX_LINES: usize = 200;
const JSONL_KEEP_LINES: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Just record which tool is about to be used (roughly PreToolUse).
    Pretool,
    /// Waiting for approval/input.
    Waiting,
    /// The response finished.
    Done,
    /// A prompt was submitted (i.e. a response is about to start).
    Working,
}

impl Event {
    pub fn parse(s: &str) -> Option<Event> {
        match s {
            "pretool" => Some(Event::Pretool),
            "waiting" => Some(Event::Waiting),
            "done" => Some(Event::Done),
            "working" => Some(Event::Working),
            _ => None,
        }
    }

    /// The string carried in the user var and in `.jsonl`'s `kind`.
    fn as_str(self) -> &'static str {
        match self {
            Event::Pretool => "pretool",
            Event::Waiting => "waiting",
            Event::Done => "done",
            Event::Working => "working",
        }
    }

    /// `.jsonl`'s `text` (a fixed message per state).
    fn text(self, lang: Lang) -> &'static str {
        match (self, lang) {
            (Event::Waiting, Lang::En) => "Waiting for approval/input",
            (Event::Waiting, Lang::Ja) => "承認/入力待ちです",
            (Event::Done, Lang::En) => "Response finished",
            (Event::Done, Lang::Ja) => "応答が完了しました",
            (_, _) => "",
        }
    }
}

pub fn run(agent: &str, event: Event) -> Result<(), String> {
    // Do nothing outside wezterm (a plain terminal, CI, an editor's internal
    // shell, etc). As with the old bash version, this isn't an error, so
    // exit silently with success.
    let Some(pane_id) = self_pane_id() else {
        return Ok(());
    };

    let lang = Lang::from_env();
    let payload = read_payload();
    let dir = paths::ensure_status_dir()?;

    match event {
        // Working is a lightweight event that only needs to record "a
        // prompt was submitted". Skip tty resolution (`wezterm cli list`)
        // and just update the timestamp.
        Event::Working => {
            let path = state_path(dir, pane_id, "working");
            paths::write_private(&path, &now_rfc3339()).map_err(|e| match lang {
                Lang::En => format!("Failed to write .working: {e}"),
                Lang::Ja => format!(".working の書き込みに失敗: {e}"),
            })
        }
        Event::Pretool => on_pretool(agent, dir, pane_id, &payload, lang),
        Event::Waiting | Event::Done => notify(agent, dir, pane_id, &payload, event, lang),
    }
}

fn self_pane_id() -> Option<u64> {
    std::env::var("WEZTERM_PANE").ok()?.trim().parse().ok()
}

/// The JSON payload from stdin. Doesn't read stdin when it's a tty, so
/// running this by hand from a terminal doesn't block.
fn read_payload() -> Value {
    // SAFETY: isatty(3) just takes an fd; no side effects, no interrupts.
    if unsafe { libc::isatty(0) } == 1 {
        return Value::Null;
    }
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return Value::Null;
    }
    serde_json::from_str(&buf).unwrap_or(Value::Null)
}

fn state_path(dir: &Path, pane_id: u64, ext: &str) -> PathBuf {
    dir.join(format!("{pane_id}.{ext}"))
}

// ---------------------------------------------------------------------------
// pretool
// ---------------------------------------------------------------------------

/// At the moment a waiting (awaiting-permission) notification fires, the
/// pane's transcript may not yet contain the tool that's actually asking
/// for permission right now (confirmed in practice: even while
/// AskUserQuestion is displayed, the transcript's tail still ended with the
/// previous user utterance). So on every PreToolUse we stash a summary of
/// the tool about to be used in `.pending`, and the waiting side reads that.
fn on_pretool(agent: &str, dir: &Path, pane_id: u64, payload: &Value, lang: Lang) -> Result<(), String> {
    let Some((name, input)) = tool_from_payload(payload) else {
        return Ok(());
    };
    let summary = tool_summary(&name, input, lang);

    // [Change from the old version] `.pending` now only stores the summary.
    //
    // The old bash version wrote the whole `tool_input` (untruncated), so
    // Edit's `old_string` / `new_string` — i.e. the full source code being
    // edited — ended up in a fixed path under `/tmp` (about 2KB per file,
    // confirmed in practice). Since readers only ever use the one-line
    // summary built here, the raw input is never stored in the first place.
    let doc = json!({ "name": name, "summary": summary, "at": epoch_secs() });
    let path = state_path(dir, pane_id, "pending");
    paths::replace_atomically(&path, &doc.to_string()).map_err(|e| match lang {
        Lang::En => format!("Failed to write .pending: {e}"),
        Lang::Ja => format!(".pending の書き込みに失敗: {e}"),
    })?;

    // Copilot CLI has no dedicated event for "awaiting permission" — the
    // ask_user tool call itself means it's waiting for input, so route it
    // to waiting here (same handling as the old
    // .copilot/hooks/wezterm-notify.sh). Claude Code fires a separate
    // Notification hook even for AskUserQuestion, so routing it here too
    // would double it up. This is the only place the two agents differ.
    if agent == "copilot" && name == "ask_user" {
        return notify(agent, dir, pane_id, payload, Event::Waiting, lang);
    }
    Ok(())
}

/// Pulls the tool name and input out of the payload. Field names differ per
/// agent (Claude Code uses `tool_name`/`tool_input`, Copilot CLI uses
/// `toolName`/`toolInput`), so both are checked.
fn tool_from_payload(payload: &Value) -> Option<(String, &Value)> {
    let name = payload
        .get("tool_name")
        .or_else(|| payload.get("toolName"))
        .and_then(Value::as_str)?;
    if name.is_empty() {
        return None;
    }
    let input = payload
        .get("tool_input")
        .or_else(|| payload.get("toolInput"))
        .or_else(|| payload.get("input"))
        .unwrap_or(&Value::Null);
    Some((name.to_string(), input))
}

/// Builds a one-line "what is it asking" summary from a single tool call.
///
/// Returns an empty string when nothing matches (the caller then falls
/// through to the next candidate).
fn tool_summary(name: &str, input: &Value, lang: Lang) -> String {
    let s = match name {
        "AskUserQuestion" => input
            .get("questions")
            .and_then(Value::as_array)
            .map(|qs| {
                qs.iter()
                    .filter_map(|q| q.get("question").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
            .unwrap_or_default(),
        "ExitPlanMode" => match str_field(input, &["plan"]) {
            Some(plan) => match lang {
                Lang::En => format!("Plan approval pending: {}", first_line(plan)),
                Lang::Ja => format!("プラン承認待ち: {}", first_line(plan)),
            },
            None => String::new(),
        },
        "Bash" | "bash" => match str_field(input, &["command"]) {
            Some(cmd) => format!("Bash: {}", first_line(cmd)),
            None => String::new(),
        },
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            match str_field(input, &["file_path", "notebook_path"]) {
                Some(p) => format!("{name}: {}", first_line(p)),
                None => String::new(),
            }
        }
        // Copilot CLI's input prompt. Its schema isn't published, so use
        // whichever plausible field is present, falling back to the
        // default message below if none is.
        "ask_user" => str_field(input, &["question", "prompt", "message"])
            .map(|q| first_line(q).to_string())
            .unwrap_or_default(),
        _ => String::new(),
    };
    let s = if s.is_empty() {
        match lang {
            Lang::En => format!("{name}: awaiting approval"),
            Lang::Ja => format!("{name} の許可待ち"),
        }
    } else {
        s
    };
    clip(&s, SUMMARY_MAX)
}

fn str_field<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// waiting / done
// ---------------------------------------------------------------------------

fn notify(
    agent: &str,
    dir: &Path,
    pane_id: u64,
    payload: &Value,
    event: Event,
    lang: Lang,
) -> Result<(), String> {
    // Fetch tty_name, pane title, tab_id, and cwd together in a single
    // `wezterm cli` call. Do nothing if the pane can't be found (e.g. it
    // was already closed).
    let Some(info) = wezterm::find_pane(pane_id) else {
        return Ok(());
    };
    let Some(tty) = info.tty_name.as_deref().filter(|t| !t.is_empty()) else {
        return Ok(());
    };

    let summary = summarize(dir, pane_id, payload, event, &info.title, lang);

    emit_osc(tty, agent, event);
    append_event(dir, pane_id, agent, event, &summary, lang);
    if event == Event::Done {
        append_memo_log(info.tab_id, &info.cwd, agent, &summary);
    }
    Ok(())
}

/// Decides the one-line "what it was doing / what it's asking" to attach to
/// the notification.
///
/// Priority order (same as the old bash version):
///   waiting … 1) `.pending` (fresh within 5s = the tool asking for
///                permission right now)
///             2) the transcript's pending tool_use (a fallback for before
///                `.pending` arrives)
///             3) the transcript's most recent assistant text
///             4) the payload's `message` (from the Notification hook)
///             5) the pane title
///   done    … 3) → 4) → 5)
///
/// The fallback to the pane title works because both Claude Code and
/// Copilot CLI write their own conversation summary into the pane title, so
/// even when the transcript can't be read, some sense of "what it was
/// doing" survives.
fn summarize(
    dir: &Path,
    pane_id: u64,
    payload: &Value,
    event: Event,
    pane_title: &str,
    lang: Lang,
) -> String {
    if event == Event::Waiting {
        if let Some(s) = pending_summary(dir, pane_id) {
            return s;
        }
    }

    let transcript = payload
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_file());

    if let Some(path) = transcript {
        let extract = extract_from_transcript(&path);
        if event == Event::Waiting {
            if let Some((name, input)) = &extract.tool {
                let s = tool_summary(name, input, lang);
                if !s.is_empty() {
                    return s;
                }
            }
        }
        if let Some(text) = &extract.text {
            // For response body text: strip a leading Markdown heading
            // marker from the first line, then clip it.
            let line = first_line(text).trim_start_matches('#').trim_start();
            let s = clip(line, SUMMARY_MAX);
            if !s.is_empty() {
                return s;
            }
        }
    }

    if let Some(msg) = payload.get("message").and_then(Value::as_str) {
        let s = first_line_clipped(msg, SUMMARY_MAX);
        if !s.is_empty() {
            return s;
        }
    }

    pane_title.to_string()
}

fn pending_summary(dir: &Path, pane_id: u64) -> Option<String> {
    let raw = std::fs::read_to_string(state_path(dir, pane_id, "pending")).ok()?;
    let doc: Value = serde_json::from_str(&raw).ok()?;
    let at = doc.get("at").and_then(Value::as_u64)?;
    // Discard both future timestamps (clock skew) and ones that are too old.
    let age = epoch_secs().checked_sub(at)?;
    if age > PENDING_MAX_AGE {
        return None;
    }
    doc.get("summary")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[derive(Default)]
struct Extract {
    /// The most recent assistant text block (the last one).
    text: Option<String>,
    /// The most recent tool_use (the last one).
    tool: Option<(String, Value)>,
}

/// Pulls the most recent assistant text and tool_use from the tail of the
/// transcript (JSON Lines).
///
/// Doesn't read the whole file: a transcript can run to several MB, and
/// only the tail is needed, so only a fixed number of trailing bytes are
/// read and split into lines (equivalent to the old bash version's
/// `tail -n 300`).
fn extract_from_transcript(path: &Path) -> Extract {
    const TAIL_BYTES: u64 = 512 * 1024;
    const TAIL_LINES: usize = 300;

    let Some(body) = read_tail(path, TAIL_BYTES) else {
        return Extract::default();
    };
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.len().saturating_sub(TAIL_LINES);

    let mut out = Extract::default();
    for line in &lines[start..] {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(blocks) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(t) = b.get("text").and_then(Value::as_str) {
                        out.text = Some(t.to_string());
                    }
                }
                Some("tool_use") => {
                    if let Some(n) = b.get("name").and_then(Value::as_str) {
                        let input = b.get("input").cloned().unwrap_or(Value::Null);
                        out.tool = Some((n.to_string(), input));
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Reads up to the last `max_bytes` of a file. Discards a first line that
/// starts mid-way through.
fn read_tail(path: &Path, max_bytes: u64) -> Option<String> {
    use std::io::{Seek, SeekFrom};

    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(max_bytes);
    if start > 0 {
        f.seek(SeekFrom::Start(start)).ok()?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let s = String::from_utf8_lossy(&buf).into_owned();
    if start == 0 {
        return Some(s);
    }
    // The start is mid-line, so discard everything up to the first newline.
    match s.find('\n') {
        Some(i) => Some(s[i + 1..].to_string()),
        None => Some(String::new()),
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// Writes OSC 1337 SetUserVar and a bell to the pane's real tty.
///
/// The user var names were generalized to `agent_status` / `agent_name`, to
/// get away from the old structure where adding an agent meant adding its
/// name in three places (Lua, Rust, and configuration). The old names
/// (`claude_status` / `copilot_status`) are still set alongside these, for
/// compatibility while configs that still read them remain around.
fn emit_osc(tty: &str, agent: &str, event: Event) {
    use std::io::Write;

    let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(tty) else {
        return;
    };
    let mut out = String::new();
    let mut set = |name: &str, value: &str| {
        out.push_str(&format!(
            "\x1b]1337;SetUserVar={name}={}\x07",
            wezterm::b64(value.as_bytes())
        ));
    };
    set("agent_name", agent);
    set("agent_status", event.as_str());
    // Compatibility only. Remove once readers have fully moved to
    // `agent_status`.
    if let "claude" | "copilot" = agent {
        set(&format!("{agent}_status"), event.as_str());
    }

    // Write the current time (epoch seconds) to agent_bell right before the
    // BEL; wezterm.lua only treats a bell as toast-worthy when agent_bell
    // is within the last few seconds and differs from the last one seen.
    // Without this, things like a zsh tab-completion bell after returning
    // to the prompt would pick up the stale state and replay the toast.
    set("agent_bell", &epoch_secs().to_string());
    out.push('\x07');

    let _ = f.write_all(out.as_bytes());
}

/// The append-only notification log (spec §2.2). The TUI's unread count
/// and history read from this.
fn append_event(dir: &Path, pane_id: u64, agent: &str, event: Event, summary: &str, lang: Lang) {
    let text = event.text(lang);
    if text.is_empty() {
        return;
    }
    let path = state_path(dir, pane_id, "jsonl");
    let line = json!({
        "at": now_rfc3339(),
        "agent": agent,
        "kind": event.as_str(),
        "text": text,
        "task": summary,
    });
    if paths::append_private(&path, &format!("{line}\n")).is_err() {
        return;
    }
    rotate(&path);
}

fn rotate(path: &Path) {
    let Ok(body) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = body.lines().collect();
    if lines.len() <= JSONL_MAX_LINES {
        return;
    }
    let kept = lines[lines.len() - JSONL_KEEP_LINES..].join("\n");
    let _ = paths::replace_atomically(path, &format!("{kept}\n"));
}

/// On done, inserts one line right after `## ログ` in the tab's memo file
/// (spec §5.3).
///
/// Only writes when there's a "meaningful string" (not a bare process name
/// like zsh) once decorative characters like spinners are stripped. The
/// judgment call is delegated to `task_from_title`, the same one the TUI's
/// display uses.
fn append_memo_log(tab_id: u64, cwd: &str, agent: &str, summary: &str) {
    let task = task_from_title(summary);
    if task == "-" {
        return;
    }
    let Ok(path) = memo::ensure_file(tab_id, cwd) else {
        return;
    };
    let Ok(body) = std::fs::read_to_string(&path) else {
        return;
    };
    let when = now_rfc3339();
    // The memo is for humans, so drop the seconds and offset: "2026-09-13T07:52:31+09:00"
    let when = when.get(..16).unwrap_or(&when).replace('T', " ");
    let line = format!("- {when} {agent}: {task}");
    let _ = std::fs::write(&path, insert_log_line(&body, &line));
}

/// Inserts a new line right after `## ログ` (the new line goes first).
///
/// There's originally exactly one blank line right after the heading, so
/// when inserting we skip that original blank line to avoid doubling it up
/// with the one we add ourselves. If the file has no `## ログ` (e.g. a
/// human deleted it), the whole section is created at the end instead.
fn insert_log_line(body: &str, line: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut done = false;
    let mut skip_blank = false;
    for l in body.lines() {
        if skip_blank && l.is_empty() {
            skip_blank = false;
            continue;
        }
        skip_blank = false;
        out.push(l.to_string());
        if !done && l == "## ログ" {
            out.push(String::new());
            out.push(line.to_string());
            done = true;
            skip_blank = true;
        }
    }
    if !done {
        out.push(String::new());
        out.push("## ログ".to_string());
        out.push(String::new());
        out.push(line.to_string());
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

#[cfg(test)]
#[path = "hook_tests.rs"]
mod tests;
