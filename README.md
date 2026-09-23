<div align="center">

# wezterm-agents

**Multi-agent status for [WezTerm](https://wezterm.org)**

Color tabs and send toast notifications when an AI coding agent (Claude Code,
GitHub Copilot CLI) is waiting for input or has finished responding, plus a
TUI to list, monitor, and jump to every agent pane across all your windows
and workspaces.

[![License: Apache 2.0](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](rust-toolchain.toml)
![Status](https://img.shields.io/badge/status-early%20%2F%20WIP-yellow.svg)

</div>

**Status: early / work in progress.** Built for and dogfooded on a single
macOS + WezTerm setup. Expect rough edges on other platforms; issues and PRs
welcome.

## Table of contents

- [✨ What it does](#-what-it-does)
- [⚙️ How it works](#-how-it-works)
- [📦 Install](#-install)
  - [WezTerm](#wezterm)
  - [Claude Code](#claude-code)
  - [GitHub Copilot CLI](#github-copilot-cli)
- [⌨️ Usage](#-usage)
- [📄 License](#-license)

## ✨ What it does

- **🎨 Tab color**: a pane's tab turns red while its agent is waiting for
  approval/input, green when it just finished responding (until you look at
  the tab), and yellow while it's actively generating a response.
- **🍞 Toast notification**: the same waiting/done transitions can pop a native
  notification.
- **📺 TUI dashboard** (`wezterm-agents` / `wezterm-agents --watch`): lists every
  agent pane, grouped by project, with unread counts, last-known task, and a
  jump-to-pane action. Also doubles as a lightweight per-project notepad
  (`e` to edit). *Note: this feature is under active development and may
  change.*

## ⚙️ How it works

An agent's hook configuration (Claude Code's `settings.json`, Copilot CLI's
`hooks/*.json`) calls `wezterm-agents hook --agent <name> <event>` on
lifecycle events (about to run a tool, waiting for input, done, prompt
submitted). The hook writes an OSC 1337 `SetUserVar` to the pane's tty and a
small per-pane state file under a private, per-uid runtime directory. A
WezTerm plugin (`plugin/init.lua`) reads those user vars to drive tab color
and notifications, and the TUI reads the state files to build its list.

## 📦 Install

Requires a Rust toolchain (`cargo build`) for now; prebuilt releases are
planned. If you don't have `cargo`, install it via
[rustup](https://rustup.rs).

```sh
git clone https://github.com/shirokuroiori/wezterm-agents
cd wezterm-agents
cargo build --release
ln -s "$PWD/target/release/wezterm-agents" ~/.local/bin/wezterm-agents
```

`~/.local/bin/wezterm-agents` is the canonical location: the WezTerm plugin
looks there by default, and `wezterm-agents init` / `install` embed that
path (not the symlink target) into the hook configuration they generate, so
re-pointing the symlink at a new build or a downloaded release binary needs
no other change. Put the binary elsewhere and you'll need `bin = '...'` in
`apply_to_config` and a re-run of `install` after every move.

### WezTerm

Until this is published as a proper `wezterm.plugin.require`-able package,
load it by path. If you already have your own `format-tab-title` handler,
call `agents.status(pane)` from inside it instead of enabling `tab_title`:

```lua
local wezterm = require 'wezterm'
local agents = dofile '/path/to/wezterm-agents/plugin/init.lua'
local config = wezterm.config_builder()

agents.apply_to_config(config, {
  tab_title = true,  -- set false if you have your own format-tab-title
  bell_toast = true,
})
```

See the doc comment at the top of `plugin/init.lua` for the full option list
(`icons`, `colors`, `bin`, `debug`, `shell_integration`, `plugin_dir`, `lang`,
`dashboard_key`)
and the composable-API example. The dashboard's display language defaults to
English; pass `lang = 'ja'` for Japanese.

### Claude Code

Claude Code picks up the hooks through a `claude` shell function that adds
`--settings '<json>'` — a JSON string, not a file — on every invocation.
Claude Code merges hooks passed this way with your own `settings.json`
instead of replacing them, so nothing in `~/.claude/` is ever touched. There
are two ways to get that function defined; they can be combined.

**Automatic (zsh, default on).** The WezTerm plugin points `ZDOTDIR` at
`plugin/shell-integration/` for every shell it spawns. The bundled `.zshenv`
there restores `ZDOTDIR`, sources your real `~/.zshenv`, then defines the
function — your rc files are never modified, and there is nothing to run
after installing the binary. This covers every zsh WezTerm starts (tabs,
splits, `wezterm cli spawn`), but not a zsh you start *inside* a pane, and
not other shells. Disable with `shell_integration = false` in
`apply_to_config`; if the plugin cannot work out its own location (a very
long `dofile` path — see the option docs), pass `plugin_dir`.

**`wezterm-agents install claude`.** Appends the same function to
`~/.zshenv`, inside a `# >>> wezterm-agents >>>` … `# <<< wezterm-agents <<<`
block that re-running the command replaces (do that after moving the
binary). This covers nested shells and terminals other than WezTerm too
(the hook is a no-op outside WezTerm), at the cost of one line in a file
you own. It goes in `.zshenv`, not `.zshrc`, because the function is also
needed by non-interactive shells such as `zsh -lc 'claude'`.

Either way, the function re-evaluates on every new shell, so it always
reflects the `wezterm-agents` binary currently on disk. What neither
covers: a `claude` launched without a shell (an editor plugin, a script
with its own PATH). For that, run `wezterm-agents init zsh` and copy the
`--settings '...'` JSON into your own `~/.claude/settings.json` hooks
section (hooks from multiple sources merge, so this is safe to add
alongside your own). bash and fish support is planned.

### GitHub Copilot CLI

```sh
wezterm-agents install copilot
```

Writes `~/.copilot/hooks/wezterm-agents.json` (Copilot CLI reads every file
in that directory, so no shim is needed here — re-run this after upgrading
`wezterm-agents` to refresh the embedded binary path). Safe to re-run; it
only ever touches that one file.

## ⌨️ Usage

```
wezterm-agents                 launcher (exits after jumping to a pane)
wezterm-agents --watch         persistent dashboard
wezterm-agents --resident      persistent dashboard used by the dashboard key
wezterm-agents --print         print the list once, no interaction
wezterm-agents --interval <s>  explicit tick interval
```

Keys: `↑/↓`/`j/k` move · `Enter` jump · `e` edit notes · `r`/`R` mark
read · `/` filter · `g` refresh · `Tab` switch list/detail layout ·
`q`/`Esc` quit.

**Dashboard key.** `apply_to_config` binds `Cmd+Shift+A` to toggle a
dashboard (`--resident`) that lives in its own `wezterm-agents-dashboard`
workspace. It's started once and reused, so opening it repeatedly doesn't
burn through tab ids. Each time it's shown, the cursor starts on the tab you
opened it from; `Enter` jumps (switching workspace as needed), `q`/`Esc`
return to where you were, and `Ctrl+C` actually quits it. Change the key
with `dashboard_key = { key = 'd', mods = 'CTRL|SHIFT' }`, or pass
`dashboard_key = false` and bind `agents.toggle_dashboard` yourself. The key
is appended to `config.keys`, so set your own `config.keys` before calling
`apply_to_config` (or append to it).

## 📄 License

Apache License 2.0
