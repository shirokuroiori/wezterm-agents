//! Determining the state directory's path, and creating/verifying/writing to it safely.
//!
//! The old implementation used a fixed `/tmp/wezterm-agent-status`, which had three holes
//! on shared hosts.
//!
//! 1. **Preemption**: because the path is fixed and public, another user can create it
//!    first as 0777 and hijack where we write. `mkdir -p` doesn't error on an existing
//!    directory and doesn't check its owner, so this goes unnoticed (CWE-377).
//! 2. **Default permissions**: the hook side's `mkdir -p` yields 0755 under umask 022,
//!    and the `.jsonl` files created with `>>` come out 0644. Their contents are
//!    transcript-derived command lines, file paths, and response bodies, so anyone who
//!    can read them leaks your work.
//! 3. **Symlink planting**: if `<pane_id>.read` is planted as a symlink to a file we
//!    own, `fs::write` follows it and truncates that file (CWE-59).
//!
//! Here, (1) and (2) are closed by "a deterministic path + creating it as 0700 +
//! verifying owner and mode", and (3) by `O_NOFOLLOW`, which rejects a symlink as the
//! final path component.
//!
//! ## Why we don't use `$XDG_RUNTIME_DIR` / `$TMPDIR` to decide the path
//!
//! The state directory is touched by two kinds of processes: the wezterm GUI process
//! (agents.lua), and the hook, which is spawned as a subprocess of the agent. These two
//! have completely different startup paths, so there's no guarantee their environment
//! variables agree; if they diverge, you get the hardest-to-notice kind of breakage —
//! both sides silently look at a different directory without either one raising an
//! error (e.g. the tab lights up but nothing shows in the TUI).
//!
//! Since security is guaranteed by the verification below rather than by where the
//! directory lives, the default is a deterministic path that doesn't depend on
//! environment variables. Only override it explicitly with `$WEZTERM_AGENTS_STATUS_DIR`
//! if you specifically want tmpfs or automatic cleanup on logout (in that case you need
//! to set the same value in both your shell rc and your wezterm config).

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Env var for an explicit override. If you set it, set it in both the shell and wezterm.
pub const STATUS_DIR_ENV: &str = "WEZTERM_AGENTS_STATUS_DIR";

fn uid() -> u32 {
    // SAFETY: getuid(2) takes no arguments, always succeeds, and is thread-safe.
    unsafe { libc::getuid() }
}

/// The state directory's path. Does not create or verify it (use `ensure_status_dir`
/// below for that).
///
/// The TUI reads this multiple times per tick, so it's cached in a `OnceLock`. We don't
/// expect the env var to change mid-process.
pub fn status_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        if let Ok(v) = std::env::var(STATUS_DIR_ENV) {
            let v = v.trim();
            if !v.is_empty() {
                return PathBuf::from(v);
            }
        }
        // The uid is appended so the path isn't shared with other users. Without
        // sharing, preemption can only mean "I created it before myself" — i.e. it
        // can't actually happen.
        PathBuf::from(format!("/tmp/wezterm-agents-{}", uid()))
    })
}

/// Prepares the state directory as 0700 and verifies it's safe to use before returning it.
///
/// On verification failure, gives up on writing (callers may swallow the error). We
/// choose "don't write if in doubt" because missing a notification is better than
/// writing into a hijacked directory.
pub fn ensure_status_dir() -> Result<&'static Path, String> {
    let dir = status_dir();
    if !dir.exists() {
        // Doing `create_dir_all` + `set_permissions` as two separate steps would let
        // another user slip in between them. `DirBuilder::mode` passes the mode
        // straight to mkdir(2) instead (umask can only clear bits, never add them, so
        // 0700 or stricter is guaranteed).
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("状態ディレクトリを作成できません ({}): {e}", dir.display()))?;
    }
    verify(dir)?;
    Ok(dir)
}

fn verify(dir: &Path) -> Result<(), String> {
    // Look without following symlinks. Following it would end up verifying that "the
    // symlink target is safe" while missing the real issue: "the path itself was
    // swapped out."
    let meta = fs::symlink_metadata(dir)
        .map_err(|e| format!("状態ディレクトリを stat できません ({}): {e}", dir.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "状態ディレクトリが symlink です ({})。すり替えの可能性があるため使いません",
            dir.display()
        ));
    }
    if !meta.is_dir() {
        return Err(format!(
            "状態ディレクトリがディレクトリではありません ({})",
            dir.display()
        ));
    }
    if meta.uid() != uid() {
        return Err(format!(
            "状態ディレクトリの所有者が別ユーザーです ({}, uid={})。使いません",
            dir.display(),
            meta.uid()
        ));
    }
    // If any group/other bits are set, another user could read or write it.
    if meta.mode() & 0o077 != 0 {
        return Err(format!(
            "状態ディレクトリが他ユーザーからアクセス可能です ({}, mode={:o})。使いません",
            dir.display(),
            meta.mode() & 0o7777
        ));
    }
    Ok(())
}

/// Overwrites with 0600. Fails if the final path component is a symlink (`O_NOFOLLOW`).
pub fn write_private(path: &Path, contents: &str) -> io::Result<()> {
    let mut f = private_open(path).truncate(true).open(path)?;
    f.write_all(contents.as_bytes())
}

/// Appends with 0600. Fails if the final path component is a symlink.
///
/// As long as there's one writer process at a time and each line is under 4KB,
/// `O_APPEND` appends are atomic at the kernel level, so no lock is needed (spec §2.2).
pub fn append_private(path: &Path, line: &str) -> io::Result<()> {
    let mut f = private_open(path).append(true).open(path)?;
    f.write_all(line.as_bytes())
}

fn private_open(_path: &Path) -> OpenOptions {
    let mut opts = OpenOptions::new();
    opts.write(true)
        .create(true)
        .mode(0o600)
        // Fail with ELOOP if the final component is a symlink. This prevents a
        // planted symlink from letting someone truncate a file we own (see (3) above).
        .custom_flags(libc::O_NOFOLLOW);
    opts
}

/// Creates a 0600 temp file in the same directory, then renames it into place.
///
/// rename(2) is atomic within the same filesystem, so readers (the TUI or
/// wezterm.lua) never see partially-written content. Use this for "replace the whole
/// thing" writes, like rotation or updating `.pending`.
pub fn replace_atomically(path: &Path, contents: &str) -> io::Result<()> {
    let tmp = tmp_sibling(path);
    // The temp file must always be a fresh create (create_new, so we never grab an
    // existing one).
    let mut opts = OpenOptions::new();
    opts.write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW);
    let mut f = opts.open(&tmp)?;
    let write_result = f.write_all(contents.as_bytes());
    drop(f);
    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let pid = std::process::id();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tmp".to_string());
    path.with_file_name(format!(".{name}.tmp.{pid}"))
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod tests;
