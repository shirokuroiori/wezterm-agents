//! Reading the state directory (`paths::status_dir()`) and resolving the git
//! branch.
//!
//! As a rule, this module doesn't write. The one exception is overwriting
//! `.read` for the "mark as read" operation (`r` / `R`), which is fine as a
//! last-write-wins overwrite with the current time, same as wezterm.lua does
//! (spec §4.3).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::model::Notification;
use crate::paths;
use crate::util::now_rfc3339;

fn state_path(pane_id: u64, ext: &str) -> PathBuf {
    paths::status_dir().join(format!("{pane_id}.{ext}"))
}

/// Only re-reads `.jsonl` when its mtime has changed (spec §4.4).
#[derive(Default)]
pub struct Store {
    cache: HashMap<u64, (SystemTime, Vec<Notification>)>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the notification history newest-first. Marks entries newer
    /// than `.read` as unread.
    pub fn notifications(&mut self, pane_id: u64) -> Vec<Notification> {
        let path = state_path(pane_id, "jsonl");
        let mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();

        let parsed: Vec<Notification> = match mtime {
            None => {
                self.cache.remove(&pane_id);
                Vec::new()
            }
            Some(mtime) => {
                let hit = matches!(self.cache.get(&pane_id), Some((cached, _)) if *cached == mtime);
                if !hit {
                    let events = parse_jsonl(&path);
                    self.cache.insert(pane_id, (mtime, events));
                }
                self.cache
                    .get(&pane_id)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default()
            }
        };

        // Redo the unread determination every time. `.read` is rewritten by
        // wezterm.lua at any moment, so the unread count can change even
        // when `.jsonl`'s mtime hasn't.
        let read_at = read_cursor(pane_id);
        let mut out: Vec<Notification> = parsed
            .into_iter()
            .map(|mut n| {
                n.unread = match &read_at {
                    Some(r) => n.at.as_str() > r.as_str(),
                    None => true,
                };
                n
            })
            .collect();
        out.reverse(); // the file is oldest-first; display is newest-first
        out.truncate(10);
        out
    }

    /// Explicitly mark as read via `r` / `R`.
    pub fn mark_read(pane_id: u64) {
        if paths::ensure_status_dir().is_err() {
            return;
        }
        let _ = paths::write_private(&state_path(pane_id, "read"), &now_rfc3339());
    }

    /// Once at startup, clean up leftovers from panes that no longer exist
    /// (spec §2.2).
    pub fn gc(live_pane_ids: &[u64]) {
        let Ok(entries) = fs::read_dir(paths::status_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // one of "<id>" / "<id>.jsonl" / "<id>.read" / "<id>.working"
            let stem = name.split('.').next().unwrap_or("");
            let Ok(id) = stem.parse::<u64>() else { continue };
            if !live_pane_ids.contains(&id) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

fn read_cursor(pane_id: u64) -> Option<String> {
    let raw = fs::read_to_string(state_path(pane_id, "read")).ok()?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// The most recent submission time, written by the `userPromptSubmitted`
/// hook (Copilot CLI). If this is newer than `.jsonl`'s latest event, that
/// turn can be assumed to still be in progress (see `model::derive_state`).
pub fn working_since(pane_id: u64) -> Option<String> {
    let raw = fs::read_to_string(state_path(pane_id, "working")).ok()?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Parses line by line with serde_json. Silently drops broken lines
/// (thanks to O_APPEND, the chance of reading a half-written line while the
/// hook side is appending is nearly nil, but if this were to fail here the
/// entire list would stop showing, so swallowing the error is the better
/// choice).
fn parse_jsonl(path: &Path) -> Vec<Notification> {
    let Ok(body) = fs::read_to_string(path) else {
        return Vec::new();
    };
    body.lines()
        .filter_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            Some(Notification {
                at: v.get("at")?.as_str()?.to_string(),
                agent: v.get("agent").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                kind: v.get("kind").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                text: v.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                task: v.get("task").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                unread: false,
            })
        })
        .collect()
}

/// Determines the git branch name from a cwd. Never spawns `git` (spec
/// §4.4).
///
/// Measured: `git branch --show-current` takes about 5,300µs versus about
/// 12µs to read `.git/HEAD` directly — a 440x difference, which makes a TTL
/// cache unnecessary in the first place. Walks up through parents since the
/// cwd may be opened at a subdirectory of the repo.
pub fn git_branch(cwd: &str) -> Option<String> {
    let mut dir = PathBuf::from(cwd);
    for _ in 0..40 {
        let dot_git = dir.join(".git");
        match fs::metadata(&dot_git) {
            Ok(meta) if meta.is_dir() => return head_to_branch(&dot_git.join("HEAD")),
            Ok(_) => {
                // in a worktree, .git is a file whose contents look like
                // "gitdir: /path/to/repo/.git/worktrees/<name>"
                let body = fs::read_to_string(&dot_git).ok()?;
                let gitdir = body.trim().strip_prefix("gitdir:")?.trim();
                return head_to_branch(&PathBuf::from(gitdir).join("HEAD"));
            }
            Err(_) => {
                if !dir.pop() {
                    return None;
                }
            }
        }
    }
    None
}

fn head_to_branch(head: &Path) -> Option<String> {
    let body = fs::read_to_string(head).ok()?;
    let body = body.trim();
    match body.strip_prefix("ref:") {
        Some(r) => Some(
            r.trim()
                .strip_prefix("refs/heads/")
                .unwrap_or(r.trim())
                .to_string(),
        ),
        // detached HEAD; return a shortened SHA
        None if !body.is_empty() => Some(body.chars().take(7).collect()),
        None => None,
    }
}
