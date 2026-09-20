//! The boundary with `wezterm cli`, plus pane-jump handling.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::model::{
    agent_from_title, derive_state, is_working_title, task_from_title, Group, Pane, Snapshot, State,
};
use crate::store::{git_branch, working_since, Store};

#[derive(Debug, Deserialize)]
struct CliPane {
    pane_id: u64,
    window_id: u64,
    tab_id: u64,
    workspace: String,
    cwd: String,
    title: String,
    /// The pane's real tty (e.g. `/dev/ttys012`). The hook writes its OSC
    /// sequences here. Older wezterm builds may omit this, so `default`.
    #[serde(default)]
    tty_name: Option<String>,
}

/// Resolve the location of the `wezterm` executable.
///
/// This has been gotten wrong twice before, so the history is recorded here.
///
/// 1. Don't rely on PATH. When launched from a keybinding
///    (SpawnCommandInNewTab) there's no shell in between, so `.zshrc`'s PATH
///    additions don't apply, and it fails with "No such file or directory".
/// 2. Don't use WEZTERM_EXECUTABLE as-is either. Its contents vary by launch
///    path — a pane spawned via `wezterm cli spawn` gets `.../wezterm`, but
///    one spawned from a keybinding (the GUI process) gets `.../wezterm-gui`.
///    wezterm-gui has no `cli` subcommand, so that fails with
///    "unrecognized subcommand 'cli'".
///
/// So instead, look for "`wezterm` inside the directory the env var points
/// at". `wezterm` and `wezterm-gui` live side by side in the same directory.
fn wezterm_bin() -> String {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("WEZTERM_EXECUTABLE_DIR") {
        if !dir.is_empty() {
            dirs.push(PathBuf::from(dir));
        }
    }
    if let Ok(exe) = std::env::var("WEZTERM_EXECUTABLE") {
        if let Some(parent) = Path::new(&exe).parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/Applications/WezTerm.app/Contents/MacOS"));

    for dir in dirs {
        let candidate = dir.join("wezterm");
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    // Last resort: let PATH resolution handle it
    "wezterm".to_string()
}

/// This is the only subprocess spawned per tick (design spec §4.4).
pub fn list_panes() -> Result<Vec<CliPaneInfo>, String> {
    let out = Command::new(wezterm_bin())
        .args(["cli", "list", "--format", "json"])
        .output()
        .map_err(|e| format!("wezterm cli の起動に失敗: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "wezterm cli list が失敗: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let panes: Vec<CliPane> =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("JSONの解析に失敗: {e}"))?;
    Ok(panes
        .into_iter()
        .map(|p| CliPaneInfo {
            pane_id: p.pane_id,
            window_id: p.window_id,
            tab_id: p.tab_id,
            workspace: p.workspace,
            cwd: decode_cwd(&p.cwd),
            title: p.title,
            tty_name: p.tty_name,
        })
        .collect())
}

/// Look up a single pane. Used by the hook to fetch its own pane's
/// (`$WEZTERM_PANE`) tty, title, tab_id, and cwd all at once. Still only
/// one `wezterm cli` invocation.
pub fn find_pane(pane_id: u64) -> Option<CliPaneInfo> {
    list_panes()
        .ok()?
        .into_iter()
        .find(|p| p.pane_id == pane_id)
}

pub struct CliPaneInfo {
    pub pane_id: u64,
    pub window_id: u64,
    pub tab_id: u64,
    pub workspace: String,
    pub cwd: String,
    pub title: String,
    pub tty_name: Option<String>,
}

/// "file:///Users/example/my%20dir/" -> "/Users/example/my dir"
fn decode_cwd(uri: &str) -> String {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    let mut out = String::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&path[i + 1..i + 3], 16) {
                out.push(b as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    // Drop a trailing slash (but keep the root "/")
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// Build a grouped snapshot from the pane list.
///
/// - Grouping is per cwd (project)
/// - Within a group, ascending order by window_id -> tab_id -> pane_id
/// - Groups are ordered "has unread first" -> then lexicographic by cwd
/// - Hides our own pane ($WEZTERM_PANE), so the launcher instance doesn't
///   list itself (design spec §4.2)
pub fn build_snapshot(panes: Vec<CliPaneInfo>, store: &mut Store, self_pane: Option<u64>) -> Snapshot {
    // Multiple panes can share a cwd, so memoize the git lookup per tick.
    let mut branches: HashMap<String, Option<String>> = HashMap::new();
    let mut by_cwd: HashMap<String, Vec<Pane>> = HashMap::new();

    for info in panes {
        if Some(info.pane_id) == self_pane {
            continue;
        }
        let notifications = store.notifications(info.pane_id);
        let working = is_working_title(&info.title);
        let working_since = working_since(info.pane_id);
        let state = derive_state(working, working_since.as_deref(), &notifications);
        let unread = notifications.iter().filter(|n| n.unread).count();
        let branch = branches
            .entry(info.cwd.clone())
            .or_insert_with(|| git_branch(&info.cwd))
            .clone();
        // The agent kind is recorded in the notification log. If there's
        // none, infer it from where `working` came from (a title spinner =
        // Claude Code, a `.working` cursor file = Copilot CLI — each is
        // only ever written by that agent's own hook/detection, so there's
        // no ambiguity). If we still can't tell, it's not an AI agent at
        // all (nvim, a plain shell, etc.), so just display the title's
        // foreground process name as-is (agent_from_title).
        let agent = notifications
            .first()
            .map(|n| n.agent.clone())
            .filter(|a| !a.is_empty())
            .or_else(|| {
                if working {
                    Some("claude".into())
                } else if state == State::Working && working_since.is_some() {
                    Some("copilot".into())
                } else {
                    None
                }
            })
            .or_else(|| agent_from_title(&info.title));

        by_cwd.entry(info.cwd.clone()).or_default().push(Pane {
            pane_id: info.pane_id,
            window_id: info.window_id,
            tab_id: info.tab_id,
            workspace: info.workspace,
            cwd: info.cwd,
            branch,
            agent,
            state,
            unread,
            notifications,
            task: task_from_title(&info.title),
        });
    }

    let mut groups: Vec<Group> = by_cwd
        .into_iter()
        .map(|(cwd, mut panes)| {
            panes.sort_by_key(|p| (p.window_id, p.tab_id, p.pane_id));
            Group { cwd, panes }
        })
        .collect();

    groups.sort_by(|a, b| {
        let a_unread = a.unread() > 0;
        let b_unread = b.unread() > 0;
        b_unread.cmp(&a_unread).then_with(|| a.cwd.cmp(&b.cwd))
    });

    Snapshot { groups }
}

/// Move focus to the chosen pane.
///
/// The primary path goes through an OSC 1337 SetUserVar, handled by
/// wezterm.lua's user-var-changed (design spec §4.5). `wezterm cli
/// activate-pane` has a known bug where it can't bring a different native
/// window to the front; only `gui_window:focus()` inside the GUI process
/// avoids that, confirmed by hands-on testing (§9.1).
///
/// Falls back to the cli path when launched outside WezTerm, or when
/// /dev/tty can't be opened.
pub fn jump(pane_id: u64) -> Result<(), String> {
    if std::env::var("WEZTERM_PANE").is_ok() {
        if let Ok(mut tty) = OpenOptions::new().write(true).open("/dev/tty") {
            // Write directly to /dev/tty rather than through ratatui's
            // render buffer — mixing it into the buffer risks it being
            // dropped by diff rendering.
            let payload = b64(pane_id.to_string().as_bytes());
            let seq = format!("\x1b]1337;SetUserVar=wezterm_agents_jump={payload}\x07");
            if tty.write_all(seq.as_bytes()).is_ok() && tty.flush().is_ok() {
                return Ok(());
            }
        }
    }
    activate_pane_fallback(pane_id)
}

fn activate_pane_fallback(pane_id: u64) -> Result<(), String> {
    let out = Command::new(wezterm_bin())
        .args(["cli", "activate-pane", "--pane-id", &pane_id.to_string()])
        .output()
        .map_err(|e| format!("activate-pane の起動に失敗: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "activate-pane が失敗: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// SetUserVar values are base64. Not worth pulling in a dependency just for
/// this, so a standard encoder is written out directly here.
/// `pub(crate)` because the hook side (hook.rs's emit_osc) uses this too.
pub(crate) fn b64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18 & 63) as usize] as char);
        out.push(TABLE[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
#[path = "wezterm_tests.rs"]
mod tests;
