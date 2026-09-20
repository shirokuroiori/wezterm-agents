//! Memos (saving work state). Spec §5.
//!
//! There are two writers. The TUI (this module) is only responsible for
//! displaying the "# メモ" (Memo) section and preparing the file for a human
//! to edit. Appending to the "## ログ" (Log) section is the hook side's job
//! (.claude/hooks/wezterm-notify.sh etc.); the TUI never writes there
//! (division of responsibility per spec §5.1).
//!
//! All internal logic is written as `_in` functions that take `dir: &Path`
//! explicitly, with the public functions being thin wrappers that just pass
//! `memo_dir()` in. Testing by overriding `HOME` via an environment variable
//! would break under `cargo test`'s parallel execution (tests would fight
//! over process-wide state), so tests instead pass a tempdir to the `_in`
//! functions.

use std::fs;
use std::path::{Path, PathBuf};

use crate::util::now_rfc3339;

pub fn memo_dir() -> PathBuf {
    // An environment without HOME is essentially never seen, but avoid crashing even so.
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => PathBuf::from(h).join(".weztermemo"),
        _ => PathBuf::from("/tmp/.weztermemo"),
    }
}

fn archive_dir_in(dir: &Path) -> PathBuf {
    dir.join("archive")
}

fn path_in(dir: &Path, tab_id: u64) -> PathBuf {
    dir.join(format!("tab-{tab_id}.md"))
}

/// A minimal parser that reads just one frontmatter field. Looks for a
/// `<key>: <value>` line inside the region delimited by `---`. Full YAML
/// support isn't needed for this, so this is hand-rolled here rather than
/// pulling in another crate.
fn frontmatter_field(path: &Path, key: &str) -> Option<String> {
    let body = fs::read_to_string(path).ok()?;
    let mut in_fm = false;
    let prefix = format!("{key}:");
    for line in body.lines() {
        if line == "---" {
            if in_fm {
                break;
            }
            in_fm = true;
            continue;
        }
        if in_fm {
            if let Some(v) = line.strip_prefix(&prefix) {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// Extracts only the "# メモ" (Memo) section (the human's free-edit area).
/// The TUI displays only this and never touches "## ログ" (Log).
fn read_preview_in(dir: &Path, tab_id: u64) -> Option<String> {
    let body = fs::read_to_string(path_in(dir, tab_id)).ok()?;
    let start = body.find("# メモ")? + "# メモ".len();
    let rest = &body[start..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    let section = rest[..end].trim();
    if section.is_empty() {
        None
    } else {
        Some(section.to_string())
    }
}

pub fn read_preview(tab_id: u64) -> Option<String> {
    read_preview_in(&memo_dir(), tab_id)
}

/// Creates the tab's memo file as an empty memo with frontmatter if it
/// doesn't exist yet, then returns its path. Does nothing if it already
/// exists.
fn ensure_file_in(dir: &Path, tab_id: u64, cwd: &str) -> std::io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = path_in(dir, tab_id);
    if !path.exists() {
        let created_at = now_rfc3339();
        let content = format!(
            "---\ntab_id: {tab_id}\ncwd: {cwd}\ncreated_at: {created_at}\n---\n\n# メモ\n\n## ログ\n\n"
        );
        fs::write(&path, content)?;
    }
    Ok(path)
}

pub fn ensure_file(tab_id: u64, cwd: &str) -> std::io::Result<PathBuf> {
    ensure_file_in(&memo_dir(), tab_id, cwd)
}

/// Editor resolution order (spec §5.3): `$VISUAL` → `$EDITOR` → `nvim` → `vi`.
pub fn resolve_editor() -> String {
    for var in ["VISUAL", "EDITOR"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                return v;
            }
        }
    }
    for candidate in ["nvim", "vi"] {
        if which(candidate) {
            return candidate.to_string();
        }
    }
    "vi".to_string()
}

fn which(bin: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    path.split(':').any(|dir| Path::new(dir).join(bin).is_file())
}

/// Stale GC run exactly once at startup (spec §5.2).
///
/// `tab_id` gets renumbered from 0 on every WezTerm restart, but
/// `~/.weztermemo` is persistent, so a memo from an "old tab 3" could
/// wrongly get attached to the "current tab 3". This is decided by matching
/// the frontmatter's `cwd` against the current tab's cwd. Memos that don't
/// match, or whose tab_id no longer exists, are moved to `archive/`.
fn gc_stale_in(dir: &Path, live: &[(u64, String)]) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(id_str) = name.strip_prefix("tab-").and_then(|s| s.strip_suffix(".md")) else {
            continue;
        };
        let Ok(tab_id) = id_str.parse::<u64>() else {
            continue;
        };

        let current_cwd = live.iter().find(|(id, _)| *id == tab_id).map(|(_, c)| c.as_str());
        let stale = match current_cwd {
            None => true,
            Some(cwd) => frontmatter_field(&path, "cwd").as_deref() != Some(cwd),
        };
        if !stale {
            continue;
        }

        let archive = archive_dir_in(dir);
        if fs::create_dir_all(&archive).is_err() {
            continue;
        }
        let created_at = frontmatter_field(&path, "created_at").unwrap_or_else(|| "unknown".into());
        // This is used in a file name, so collapse colons and the like.
        let safe_created_at: String = created_at
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
            .collect();
        let dest = archive.join(format!("tab-{tab_id}-{safe_created_at}.md"));
        let _ = fs::rename(&path, dest);
    }
}

pub fn gc_stale(live: &[(u64, String)]) {
    gc_stale_in(&memo_dir(), live)
}

#[cfg(test)]
#[path = "memo_tests.rs"]
mod tests;
