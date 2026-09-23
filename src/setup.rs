//! `wezterm-agents init <shell>` / `wezterm-agents install <claude|copilot>`
//!
//! Wiring that turns an agent's hook setup from "hand-write JSON and add it
//! to a config file" into "run one command." Each agent has a different
//! injection path.
//!
//! - **Claude Code**: hooks merge across scopes (`claude --settings
//!   <file-or-json>` merges with the user's own `settings.json` instead of
//!   overwriting it; `--settings` accepts a JSON string directly, not just a
//!   file path — both confirmed by hand). So wrapping every `claude`
//!   invocation in a shell function that appends `--settings '<json>'` lets
//!   us avoid ever writing to the user's own `settings.json`.
//! - **Copilot CLI**: `~/.copilot/hooks/*.json` is a drop-in format where
//!   every file in the directory is read individually, so we just need to
//!   drop one file there — no shim required.
//!
//! Either way, the binary embedded in the hook's `command` is kept as an
//! absolute path (`current_bin()`, preferring the canonical location
//! `~/.local/bin/wezterm-agents`). Wherever the user has `wezterm-agents`
//! installed (`~/.local/bin`, Homebrew, anywhere), this lets the hook point
//! at the right binary without depending on PATH at hook-run time (hooks are
//! launched as a subprocess of the agent, which sometimes has a minimal
//! PATH — same reason as `wezterm_bin()` in wezterm.rs).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::lang::Lang;

/// The "wezterm-agents binary path" embedded into config files.
///
/// `~/.local/bin/wezterm-agents` is treated as the canonical location (same
/// as the README's Install section, `bin_candidates` in plugin/init.lua, and
/// the default in shell-integration/.zshenv). If we were launched from
/// there, or if a symlink there points at us, we return that canonical path
/// instead of the real path — during development we run via a symlink to
/// `target/release/`, and if we embedded the real path (`target/release/...`)
/// it would keep pointing at a stale binary once the symlink target changes.
///
/// If we're not at the canonical location, return the absolute form of the
/// path we were launched with (without following symlinks — a safety net
/// for launches via a relative path or `./wezterm-agents`).
pub fn current_bin() -> Result<String, String> {
    let lang = Lang::from_env();
    let exe = std::env::current_exe().map_err(|e| match lang {
        Lang::En => format!("Couldn't get our own path: {e}"),
        Lang::Ja => format!("自身のパスを取得できません: {e}"),
    })?;
    if let Some(canonical) = canonical_install_path() {
        if same_file(&canonical, &exe) {
            return Ok(canonical.to_string_lossy().into_owned());
        }
    }
    let abs = std::path::absolute(&exe).map_err(|e| match lang {
        Lang::En => format!("Failed to make the path absolute ({}): {e}", exe.display()),
        Lang::Ja => format!("パスの絶対化に失敗しました ({}): {e}", exe.display()),
    })?;
    Ok(abs.to_string_lossy().into_owned())
}

/// The canonical location, `~/.local/bin/wezterm-agents`. `None` if `HOME` isn't set.
fn canonical_install_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".local/bin/wezterm-agents"))
}

/// Whether two paths point at the same file once symlinks are resolved.
/// `false` if either one doesn't exist.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Claude Code's `hooks` object (the contents of the `"hooks"` key).
fn claude_hooks(bin: &str) -> Value {
    let pretool = format!("\"{bin}\" hook --agent claude pretool");
    let waiting = format!("\"{bin}\" hook --agent claude waiting");
    let done = format!("\"{bin}\" hook --agent claude done");
    json!({
        "PreToolUse": [
            { "hooks": [{ "type": "command", "command": pretool }] }
        ],
        "Notification": [
            {
                "matcher": "permission_prompt|agent_needs_input",
                "hooks": [{ "type": "command", "command": waiting }]
            }
        ],
        "Stop": [
            { "hooks": [{ "type": "command", "command": done }] }
        ]
    })
}

/// The shell snippet loaded via `eval "$(wezterm-agents init <shell>)"`.
///
/// This is evaluated fresh every time the process starts (`eval` runs on
/// every shell startup), so it never needs a state file. Swap the binary out
/// and the very next shell startup automatically picks up the new absolute
/// path and the new hook definitions.
pub fn shell_init(shell: &str, bin: &str) -> Result<String, String> {
    let json_str = json!({ "hooks": claude_hooks(bin) }).to_string();
    // The JSON only ever uses double quotes, so the shell side can safely
    // wrap it wholesale in single quotes (no `$` or backtick expansion to
    // worry about).
    match shell {
        "zsh" | "bash" => Ok(format!(
            "claude() {{\n  command claude --settings '{json_str}' \"$@\"\n}}\n"
        )),
        "fish" => Ok(format!(
            "function claude\n  command claude --settings '{json_str}' $argv\nend\n"
        )),
        other => Err(match Lang::from_env() {
            Lang::En => format!("Unsupported shell: {other} (specify one of zsh, bash, fish)"),
            Lang::Ja => format!(
                "未対応のシェルです: {other}（zsh, bash, fish のいずれかを指定してください）"
            ),
        }),
    }
}

/// Writes `~/.copilot/hooks/wezterm-agents.json`. An existing file is
/// overwritten outright (this file belongs entirely to wezterm-agents; it's
/// not meant to be hand-edited alongside other settings, so re-running the
/// command can just bring it fully up to date).
pub fn install_copilot(bin: &str) -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| home_unset_error())?;
    install_copilot_in(bin, &PathBuf::from(home).join(".copilot/hooks"))
}

fn home_unset_error() -> String {
    match Lang::from_env() {
        Lang::En => "HOME is not set".to_string(),
        Lang::Ja => "HOME が未設定です".to_string(),
    }
}

/// The body of `install_copilot`, taking the destination directory
/// explicitly instead of reading `HOME` directly.
///
/// Swapping `HOME` via an env var for a test breaks under `cargo test`'s
/// parallel execution, since tests would fight over shared process-wide
/// state (same reason as memo.rs). So tests instead call this function with
/// a tempdir.
fn install_copilot_in(bin: &str, dir: &Path) -> Result<PathBuf, String> {
    let working = format!("\"{bin}\" hook --agent copilot working");
    let pretool = format!("\"{bin}\" hook --agent copilot pretool");
    let waiting = format!("\"{bin}\" hook --agent copilot waiting");
    let done = format!("\"{bin}\" hook --agent copilot done");

    let doc = json!({
        "version": 1,
        "hooks": {
            "userPromptSubmitted": [{ "type": "command", "bash": working, "timeoutSec": 5 }],
            "preToolUse": [{ "type": "command", "bash": pretool, "timeoutSec": 5 }],
            "notification": [{
                "type": "command",
                "matcher": "permission_prompt|elicitation_dialog",
                "bash": waiting,
                "timeoutSec": 5
            }],
            "agentStop": [{ "type": "command", "bash": done.clone(), "timeoutSec": 5 }],
            "errorOccurred": [{ "type": "command", "bash": done.clone(), "timeoutSec": 5 }],
            "sessionEnd": [{ "type": "command", "bash": done, "timeoutSec": 5 }]
        }
    });

    let lang = Lang::from_env();
    std::fs::create_dir_all(dir).map_err(|e| match lang {
        Lang::En => format!("Couldn't create directory ({}): {e}", dir.display()),
        Lang::Ja => format!("ディレクトリを作成できません ({}): {e}", dir.display()),
    })?;
    let path = dir.join("wezterm-agents.json");
    let body = serde_json::to_string_pretty(&doc).expect("static shape, always serializable");
    std::fs::write(&path, body + "\n").map_err(|e| match lang {
        Lang::En => format!("Failed to write ({}): {e}", path.display()),
        Lang::Ja => format!("書き込みに失敗しました ({}): {e}", path.display()),
    })?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// install claude — appending to ~/.zshenv
// ---------------------------------------------------------------------------

/// Start/end markers for the block written into `~/.zshenv`.
///
/// pyenv's `init --install` uses a "do nothing if the target file already
/// has my name in it" approach, but that leaves a stale absolute path behind
/// once the binary moves. Here we instead treat the region wrapped in these
/// markers as ours, and replace it wholesale on every re-run (the same
/// convention as conda's `>>> conda initialize >>>`).
const ZSHENV_BEGIN: &str = "# >>> wezterm-agents >>>";
const ZSHENV_END: &str = "# <<< wezterm-agents <<<";

/// The body of the block placed into `~/.zshenv`.
///
/// **Why `.zshenv` and not `.zshrc`**: the `claude` shell function isn't
/// only needed by interactive shells. It's also launched from a
/// non-interactive, login zsh like `wezterm cli spawn -- zsh -lc 'claude'`,
/// and a pane may spawn a further nested `zsh`. Of zsh's startup files,
/// `.zshenv` is the only one guaranteed to be read no matter how the shell
/// was started.
///
/// **Why an absolute-path existence check instead of `command -v`**:
/// `.zshenv` is read before `.zshrc`, so directories `.zshrc` adds to PATH
/// (e.g. `~/.local/bin`) aren't on PATH yet. wezterm-gui is launched by
/// launchd with a minimal PATH (`/usr/bin:/bin:/usr/sbin:/sbin`), so a zsh
/// spawned from it would have `command -v wezterm-agents` fail, silently
/// leaving the shim undefined — this actually happened in practice
/// (2026-09-15).
fn zshenv_block(bin: &str) -> String {
    format!(
        "{ZSHENV_BEGIN}\n\
         # Block managed by `wezterm-agents install claude`. Don't edit by hand;\n\
         # if you move the binary, just re-run the same command (only this\n\
         # block gets replaced).\n\
         # Defines the `claude` shell function that injects Claude Code's hooks.\n\
         if [ -x \"{bin}\" ]; then\n\
         \x20 eval \"$(\"{bin}\" init zsh)\"\n\
         fi\n\
         {ZSHENV_END}\n"
    )
}

/// Determines where `install claude` should write, and appends there.
pub fn install_claude(bin: &str) -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| home_unset_error())?;
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let target = zshenv_target(
        &home,
        env("ZDOTDIR").as_deref(),
        env("WEZTERM_AGENTS_ZDOTDIR").as_deref(),
    );
    install_claude_in(&shell_bin_expr(bin, &home), &target)
}

/// The first line of the bundled shell-integration/.zshenv. Used as a marker
/// to recognize the ZDOTDIR the plugin injected — keep this in sync with
/// that file.
const PLUGIN_ZSHENV_MARKER: &str = "# wezterm-agents: zsh shell integration";

/// The `.zshenv` path to append to.
///
/// The default is `~/.zshenv`. On startup, zsh reads `$ZDOTDIR/.zshenv` if
/// `ZDOTDIR` is **already** set, and `~/.zshenv` otherwise. Under a common
/// dotfiles convention (`export ZDOTDIR=~/.config/zsh` inside `~/.zshenv`
/// itself), an interactive shell does end up with `ZDOTDIR` exported, but
/// it's unset at the moment zsh starts up, so what zsh actually reads is
/// still `~/.zshenv`. Writing to `$ZDOTDIR/.zshenv` in that case would
/// succeed but never actually get read (flagged in a 2026-09-16 review). So
/// we prefer `~/.zshenv` when it exists, and only defer to `ZDOTDIR` when it
/// doesn't (for setups where `ZDOTDIR` is set via `/etc/zshenv` or launchd).
///
/// A second trap: running this from a shell that doesn't restore ZDOTDIR
/// (such as a bash inside wezterm) can end up seeing the plugin-injected
/// bundled directory as `ZDOTDIR`. Writing there unmodified would pollute
/// the bundled `.zshenv` inside the repo, so whenever the first-line marker
/// identifies the target as the bundled file, we redirect to the fallback
/// `WEZTERM_AGENTS_ZDOTDIR` (or `HOME` if that isn't set either).
fn zshenv_target(home: &str, zdotdir: Option<&str>, saved_zdotdir: Option<&str>) -> PathBuf {
    let home_zshenv = Path::new(home).join(".zshenv");
    if home_zshenv.is_file() {
        return home_zshenv;
    }
    let dir = match zdotdir {
        Some(d) if is_plugin_shell_integration(Path::new(d)) => saved_zdotdir.unwrap_or(home),
        Some(d) => d,
        None => home,
    };
    Path::new(dir).join(".zshenv")
}

/// Whether `dir/.zshenv` is the plugin's bundled shell integration file
/// (determined by the first-line marker).
fn is_plugin_shell_integration(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join(".zshenv"))
        .map(|s| s.lines().next() == Some(PLUGIN_ZSHENV_MARKER))
        .unwrap_or(false)
}

/// The binary-path form embedded into `.zshenv`. If it's under the home
/// directory, the leading portion is replaced with `$HOME` (expanded inside
/// the double quotes), so that sharing `.zshenv` as dotfiles across multiple
/// machines doesn't break just because the username differs. Don't use this
/// for paths embedded anywhere other than shell scripts (e.g. JSON).
fn shell_bin_expr(bin: &str, home: &str) -> String {
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return bin.to_string();
    }
    match bin.strip_prefix(home) {
        // Only substitute when the next character is `/`, so we don't
        // mistake `/home/alice2/...` for being under the home dir `/home/alice`.
        Some(rest) if rest.starts_with('/') => format!("$HOME{rest}"),
        _ => bin.to_string(),
    }
}

/// The body of `install_claude`. Like `install_copilot_in`, takes the
/// destination path explicitly for testability.
fn install_claude_in(bin: &str, path: &Path) -> Result<PathBuf, String> {
    let lang = Lang::from_env();
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(match lang {
                Lang::En => format!("Failed to read ({}): {e}", path.display()),
                Lang::Ja => format!("読み込みに失敗しました ({}): {e}", path.display()),
            })
        }
    };
    let body = upsert_block(&existing, &zshenv_block(bin))?;
    std::fs::write(path, body).map_err(|e| match lang {
        Lang::En => format!("Failed to write ({}): {e}", path.display()),
        Lang::Ja => format!("書き込みに失敗しました ({}): {e}", path.display()),
    })?;
    Ok(path.to_path_buf())
}

/// Replaces the marker-delimited region inside `body` with `block`. If the
/// region doesn't exist, appends it at the end (adding a trailing newline
/// first if missing, plus one blank line of separation from existing
/// content). If only the begin or only the end marker is present — a
/// corrupted state — this returns an error rather than silently trying to
/// fix it, so a human can look at it.
fn upsert_block(body: &str, block: &str) -> Result<String, String> {
    let begin = body.find(ZSHENV_BEGIN);
    let end = body.find(ZSHENV_END);
    match (begin, end) {
        (Some(b), Some(e)) if b < e => {
            // Drop everything through the end of the end marker's line
            // (including its newline, if present).
            let after = &body[e + ZSHENV_END.len()..];
            let after = after.strip_prefix('\n').unwrap_or(after);
            Ok(format!("{}{block}{after}", &body[..b]))
        }
        (None, None) => {
            let mut out = body.to_string();
            if !out.is_empty() {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
            }
            out.push_str(block);
            Ok(out)
        }
        _ => Err(match Lang::from_env() {
            Lang::En => format!(
                "The existing `{ZSHENV_BEGIN}` / `{ZSHENV_END}` pairing is broken. \
                 Remove the block by hand and re-run"
            ),
            Lang::Ja => format!(
                "既存の `{ZSHENV_BEGIN}` / `{ZSHENV_END}` の対応が壊れています。\
                 手で区画を削除してから再実行してください"
            ),
        }),
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
