-- wezterm-agents: a wezterm plugin that reflects the status of AI agents
-- (Claude Code / Copilot CLI / etc.) running in WezTerm as tab color and
-- bell notifications, and interoperates with the TUI (the `wezterm-agents`
-- command).
--
-- Laid out to be loaded via `wezterm.plugin.require` (`plugin/init.lua` is
-- the entry point). The actual agent-side hook logic and creating/verifying
-- the state directory are handled by a separate process (the
-- `wezterm-agents` binary, written in Rust under ../src/). This file only
-- holds the wezterm-side receiving end.
--
-- ## Usage
--
-- If you don't have any custom tab rendering, this alone gets you fully
-- working:
--
--   local agents = wezterm.plugin.require 'https://github.com/<owner>/wezterm-agents'
--   agents.apply_to_config(config, { tab_title = true })
--
-- If you already have your own `format-tab-title` handler (as this repo's
-- own .config/wezterm/wezterm.lua does), leave `tab_title` disabled and
-- call `agents.status(pane)` from inside your own handler to just compose
-- the status:
--
--   agents.apply_to_config(config, { tab_title = false, bell_toast = true })
--   wezterm.on('format-tab-title', function(tab, ...)
--     local st = agents.status(tab.active_pane)
--     if st then ... end
--   end)
--
-- Regardless of the tab_title/bell_toast settings, `apply_to_config` always
-- wires up pane-jump and read-tracking (reading/writing the state files the
-- TUI uses). These are always enabled since they never conflict with any
-- other configuration.
--
-- [format-tab-title constraint] WezTerm only runs the first handler
-- registered for `format-tab-title` (a later `wezterm.on('format-tab-title',
-- ...)` call is ignored). That's why `tab_title` defaults to off. If you
-- enable it, don't register your own `format-tab-title` elsewhere.
--
-- [hard implementation constraint] format-tab-title is called on the GUI's
-- render thread, once per tab, on every tab-bar redraw. `M.status()` only
-- does table lookups and bit operations, never file I/O or spawning a
-- subprocess (the read-tracking check only looks at the in-memory
-- `dismissed` table). Breaking this would drag the whole terminal's
-- rendering down with disk or process-launch latency, so keep any new
-- logic here to the same constraint.
local wezterm = require 'wezterm'

local M = {}

--------------------------------------------------------------------------
-- Config (defaults, until overridden by apply_to_config)
--------------------------------------------------------------------------

-- Status icons (Nerd Font). Kept in sync with the TUI (State::icon in
-- ../src/model.rs).
--   working  nf-md-dots_circle   U+F1978
--   waiting  nf-cod-stop_circle  U+EBA5
--   done     nf-fa-ok_sign       U+F058
local DEFAULT_ICONS = {
  working = '\u{f1978}',
  waiting = '\u{eba5}',
  done = '\u{f058}',
}

-- The voltwave color scheme's ansi yellow/red/green.
local DEFAULT_COLORS = {
  working = '#FFCC00',
  waiting = '#FE4450',
  done = '#50fa7b',
}

local icons = DEFAULT_ICONS
local colors = DEFAULT_COLORS
local debug_enabled = false

--------------------------------------------------------------------------
-- State directory resolution
--------------------------------------------------------------------------

-- The Rust side (../src/paths.rs) is the sole authority on the state
-- directory path. This just asks `<bin> status-dir`. That logic creates the
-- directory with the uid baked into the path, mode 0700, and verifies
-- ownership and mode before returning it — this can't be duplicated in Lua
-- (Lua has no way to get the equivalent of getuid needed for the ownership
-- check).
--
-- The binary's location can vary by environment (downloaded from GitHub
-- Releases and placed wherever, installed via a package manager, manually
-- placed at `~/.local/bin`, etc). `opts.bin` is tried first, then
-- `~/.local/bin/wezterm-agents` (where install.sh / auto_install put it), and
-- finally plain `wezterm-agents` via PATH resolution. That last candidate
-- only works in environments where wezterm-gui's PATH isn't the minimal one
-- it usually gets (i.e. it never went through a shell rc) — e.g. when
-- wezterm itself was launched from inside a terminal.
local function bin_candidates(explicit)
  local list = {}
  if explicit and explicit ~= '' then
    table.insert(list, explicit)
  end
  local home = os.getenv 'HOME'
  if home and home ~= '' then
    table.insert(list, home .. '/.local/bin/wezterm-agents')
  end
  table.insert(list, 'wezterm-agents')
  return list
end

local bins = bin_candidates(nil)

--------------------------------------------------------------------------
-- Our own location (used to resolve the shell-integration directory)
--------------------------------------------------------------------------

-- This file's absolute path. wezterm's Lua has no `debug` library
-- (confirmed in practice: `debug == nil`, 2026-09-16), so `debug.getinfo`
-- isn't available. Instead, this is picked up via two paths:
--   1. Via `require` (which `wezterm.plugin.require` also uses): Lua passes
--      the file path as the second argument. This is exact, so it's tried
--      first.
--   2. Via `dofile`: no argument is passed. The file name is pulled out of
--      the location info (`"path:line: "`) that `error()` attaches.
--      However, Lua truncates this location path to around 60 characters
--      with "...", so it can't be recovered for a long path. In that case,
--      the caller needs to pass it explicitly via `apply_to_config`'s
--      `plugin_dir`.
local SELF_PATH = (function(modpath)
  if type(modpath) == 'string' and modpath:sub(1, 1) == '/' then
    return modpath
  end
  local where = select(2, pcall(error, '', 2))
  if type(where) ~= 'string' then
    return nil
  end
  return where:match '^(/.*):%d+: $'
end)(select(2, ...))

-- The shell-integration directory (bundles a `.zshenv`). Sibling of init.lua.
local SHELL_INTEGRATION_DIR = SELF_PATH and (SELF_PATH:match '^(.*)/[^/]*$' .. '/shell-integration')

-- The repo root (the parent of plugin/). install.sh and Cargo.toml live
-- there.
local REPO_ROOT = SELF_PATH and SELF_PATH:match '^(.*)/plugin/[^/]*$'

local function file_exists(path)
  local f = io.open(path, 'r')
  if not f then
    return false
  end
  f:close()
  return true
end

-- Whether `dir/.zshenv` is (this plugin's, or another instance's) bundled
-- shell integration. Determined by a first-line marker — the same string as
-- src/setup.rs's PLUGIN_ZSHENV_MARKER.
local PLUGIN_ZSHENV_MARKER = '# wezterm-agents: zsh shell integration'
local function is_plugin_shell_integration(dir)
  local f = io.open(dir .. '/.zshenv', 'r')
  if not f then
    return false
  end
  local first = f:read 'l'
  f:close()
  return first == PLUGIN_ZSHENV_MARKER
end

-- nil = not yet resolved / false = resolution failed (don't retry) / string = resolved
local status_dir_cache = nil
-- The path of the binary that resolution succeeded with. Used to spawn the
-- resident dashboard.
local resolved_bin = nil

local function log(fmt, ...)
  if not debug_enabled then
    return
  end
  local ok, msg = pcall(string.format, fmt, ...)
  if not ok then
    msg = tostring(fmt)
  end
  -- Only looks at the status_dir_cache value (calling status_dir() here
  -- would cause mutual recursion). Falls to /tmp while unresolved, which is
  -- harmless since this is only for debugging.
  local dir = type(status_dir_cache) == 'string' and status_dir_cache or '/tmp'
  pcall(function()
    local f = io.open(dir .. '/wezterm-agents-debug.log', 'a')
    if not f then
      return
    end
    f:write(os.date('%H:%M:%S') .. ' ' .. msg .. '\n')
    f:close()
  end)
end

-- [can't resolve during config evaluation] Calling
-- `wezterm.run_child_process` at the top level of the config file (i.e.
-- during config evaluation) fails (confirmed in practice: pcall returns
-- false, 2026-09-13). So resolution must always happen from inside an
-- event handler, and the result is cached the first time it succeeds.
local function status_dir()
  if status_dir_cache == false then
    return nil
  end
  if status_dir_cache then
    return status_dir_cache
  end
  for _, bin in ipairs(bins) do
    -- run_child_process returns (success, stdout, stderr). Since this goes
    -- through pcall, the first return value is whether the call itself
    -- succeeded.
    local ok, success, stdout = pcall(wezterm.run_child_process, { bin, 'status-dir' })
    if ok and success and type(stdout) == 'string' then
      local trimmed = stdout:gsub('%s+$', '')
      if trimmed ~= '' then
        status_dir_cache = trimmed
        resolved_bin = bin
        pcall(function()
          local f = io.open(trimmed .. '/wezterm-agents-debug.log', 'a')
          if f then
            f:write(os.date('%H:%M:%S') .. ' status_dir: ' .. trimmed .. ' (bin=' .. bin .. ')\n')
            f:close()
          end
        end)
        return trimmed
      end
    end
  end
  status_dir_cache = false
  return nil
end

-- Runs fn(dir) with the state directory, but never spawns a child process
-- from inside the calling handler. While unresolved, resolution (and fn) is
-- deferred to a timer callback.
--
-- [why] wezterm runs at most one `update-status` per GUI window at a time
-- and drops new ones while the previous is still InProgress (see
-- emit_window_event in wezterm-gui/src/termwindow/mod.rs). If a handler's
-- completion notice is ever lost, that window never gets `update-status`
-- again. That happened in practice on 2026-09-23: a window's first
-- update-status yielded inside run_child_process during a config reload,
-- and read-tracking in that window stayed dead for hours. A timer callback
-- isn't tied to any window's event state, so yielding there is harmless.
local function with_status_dir(fn)
  if status_dir_cache == false then
    return
  end
  if status_dir_cache then
    fn(status_dir_cache)
    return
  end
  local ok = pcall(wezterm.time.call_after, 0, function()
    local dir = status_dir()
    if dir then
      fn(dir)
    end
  end)
  if not ok then
    local dir = status_dir()
    if dir then
      fn(dir)
    end
  end
end

local function read_file(path)
  local ok, body = pcall(function()
    local f = io.open(path, 'r')
    if not f then
      return nil
    end
    local content = f:read 'a'
    f:close()
    return content
  end)
  return ok and body or nil
end

local function write_file(path, body)
  pcall(function()
    local f = io.open(path, 'w')
    if not f then
      return
    end
    f:write(body)
    f:close()
  end)
end

--------------------------------------------------------------------------
-- Binary auto-install
--------------------------------------------------------------------------

-- The repo root to run install.sh from, set by apply_to_config when
-- auto_install is on. nil = off.
local auto_install_root = nil
-- Display language for this plugin's own toasts ('en' / 'ja').
local ui_lang = 'en'

-- The version this plugin checkout wants: `version` in the repo's
-- Cargo.toml, the same number release.yml checks each tag against. Pinning
-- to it (rather than "latest") keeps the plugin and the binary it installs
-- from the same release, and makes a plugin update pull the matching binary.
local function required_version(root)
  local body = read_file(root .. '/Cargo.toml')
  if not body then
    return nil
  end
  for line in body:gmatch '[^\n]+' do
    local v = line:match '^version%s*=%s*"([^"]+)"'
    if v then
      return v
    end
  end
  return nil
end

local INSTALLED_TOAST = {
  en = 'Installed v%s. Claude Code hooks take effect in new tabs.',
  ja = 'v%s をインストールしました。Claude Code の hooks は新しいタブから有効になります。',
}

-- Puts the release binary matching required_version() at
-- ~/.local/bin/wezterm-agents via install.sh. --managed-only means a
-- symlink (a local `cargo build`) or a binary install.sh didn't put there
-- itself is left alone; see install.sh's header for the rules. The common
-- case ("already current") doesn't touch the network.
--
-- Spawns a child process, so only call this from a timer callback (see
-- with_status_dir for why not from an event handler directly).
local function ensure_binary(window)
  local root = auto_install_root
  if not root then
    return
  end
  local version = required_version(root)
  if not version then
    wezterm.log_warn('wezterm-agents: auto_install: ' .. root .. '/Cargo.toml からバージョンを読めませんでした')
    return
  end
  -- The target triple is left to install.sh's own detection rather than
  -- wezterm.target_triple: an x86_64 wezterm under Rosetta should still get
  -- the native arm64 binary.
  local ok, success, stdout, stderr =
    pcall(wezterm.run_child_process, { 'sh', root .. '/install.sh', '--version', version, '--managed-only' })
  local result = ok and success and type(stdout) == 'string' and stdout:match '^(%a+)' or nil
  log('auto_install: v%s -> %s (%s)', version, tostring(result), tostring(stderr))
  if not result then
    wezterm.log_warn('wezterm-agents: auto_install failed: ' .. tostring(ok and stderr or success))
    return
  end
  if result == 'installed' or result == 'updated' then
    -- A status_dir() that ran (and failed) while the download was in
    -- flight cached `false`. Forget it so the next call resolves again,
    -- now against the new binary.
    status_dir_cache = nil
    resolved_bin = nil
  end
  if result == 'installed' and window then
    pcall(function()
      window:toast_notification(
        'wezterm-agents',
        string.format(INSTALLED_TOAST[ui_lang] or INSTALLED_TOAST.en, version),
        nil,
        8000
      )
    end)
  end
end

-- Matches the format used on the hook side (now_rfc3339 in ../src/util.rs).
-- os.date('%z') returns "+0900" without a colon, so one is inserted.
-- .read and .jsonl timestamps are compared as strings, so the format needs
-- to match exactly.
local function rfc3339(t)
  local z = os.date('%z', t)
  return os.date('%Y-%m-%dT%H:%M:%S', t) .. z:sub(1, 3) .. ':' .. z:sub(4, 5)
end

--------------------------------------------------------------------------
-- Read tracking
--------------------------------------------------------------------------

-- dismissed[pane_id] = true means "that pane's done state has been read".
-- Held only in the GUI process's memory.
local dismissed = {}

-- window_id -> most recently observed active pane_id.
-- update-status fires roughly every second, so this is used as a diff to
-- only write when it actually changes.
local last_active = {}

-- Flag so restore_dismissed() runs exactly once, on the first update-status.
local restored_once = false

-- Called from format-tab-title / M.status. Table lookup only.
function M.is_dismissed(pane_id)
  return dismissed[pane_id] == true
end

-- The user vars that read-tracking applies to. agent_status is the current
-- one; the other two are compatibility for hooks from before things were
-- unified onto `agent_status`/`agent_name`.
local STATUS_VARS = {
  agent_status = true,
  claude_status = true,
  copilot_status = true,
}

-- Returns the single status value a pane is currently showing. 'working'
-- never renders green, so it's excluded from read-tracking (the caller's
-- user-var-changed handler early-returns on it for the same reason).
local function current_status_value(mux_pane)
  local ok, vars = pcall(function() return mux_pane:get_user_vars() end)
  if not ok or not vars then
    return nil
  end
  local v = vars.agent_status or vars.claude_status or vars.copilot_status
  if v == nil or v == '' or v == 'working' then
    return nil
  end
  return v
end

-- When given a value, persists it as dismissed_value, meaning "this
-- pane_id has been read up through this value". restore_dismissed compares
-- what's stored here against the value currently showing, so it's
-- unaffected by `.jsonl` rotation or a missing `.jsonl` (more below). When
-- there's no value (i.e. nothing is currently showing done/waiting), just
-- updating the in-memory read flag is enough, so nothing is written.
local function mark_read(pane_id, value)
  dismissed[pane_id] = true
  -- If the state directory isn't available (e.g. the binary is missing),
  -- fall back to the in-memory read flag for this session only. The time is
  -- taken now, so a deferred write still records when it was actually read.
  local read_at = rfc3339(os.time())
  with_status_dir(function(dir)
    write_file(dir .. '/' .. pane_id .. '.read', read_at)
    if value then
      write_file(dir .. '/' .. pane_id .. '.dismissed_value', value)
    end
  end)
end

-- Reloading the config wipes `dismissed` along with the rest of Lua state,
-- but the panes and their user_vars (state on the wezterm cli side) survive.
--
-- [why this was fixed] The old approach compared .read against .jsonl's
-- last-event timestamp as strings to restore state, and that misbehaved in
-- practice: .jsonl trims its oldest lines once it exceeds 200, and some
-- panes have never had a notification at all (no .jsonl to begin with). In
-- both cases "the last-read time" and "the current user_vars value" fell
-- out of correspondence, so unrelated tabs would "come back" green right
-- after a reload even though no new notification had actually arrived
-- (found through hands-on investigation on 2026-08-16).
--
-- The current approach only compares "the value currently showing" against
-- "the value it was marked read at", so it doesn't depend on .jsonl's
-- contents at all. If the values match exactly (i.e. still the same
-- done/waiting as before the reload = nothing new), it stays read. If the
-- value changed (i.e. a new notification arrived during the reload), it's
-- treated as unread.
--
-- (A full WezTerm restart makes panes disappear entirely and reassigns
-- pane_ids, so in practice this only matters for a config reload.)
local function restore_dismissed(dir)
  local restored = 0
  for _, mux_win in ipairs(wezterm.mux.all_windows()) do
    for _, mux_tab in ipairs(mux_win:tabs()) do
      for _, mux_pane in ipairs(mux_tab:panes()) do
        local pane_id = mux_pane:pane_id()
        local current = current_status_value(mux_pane)
        if current then
          local saved = read_file(dir .. '/' .. pane_id .. '.dismissed_value')
          if saved and saved:gsub('%s+$', '') == current then
            dismissed[pane_id] = true
            restored = restored + 1
          end
        end
      end
    end
  end
  log('restore_dismissed: %d panes', restored)
end

--------------------------------------------------------------------------
-- Pane jump
--------------------------------------------------------------------------

-- Scans the whole mux to look up {pane, tab, window} from a pane_id.
local function find_pane(target_id)
  for _, mux_win in ipairs(wezterm.mux.all_windows()) do
    for _, mux_tab in ipairs(mux_win:tabs()) do
      for _, mux_pane in ipairs(mux_tab:panes()) do
        if mux_pane:pane_id() == target_id then
          return mux_pane, mux_tab, mux_win
        end
      end
    end
  end
  return nil, nil, nil
end

-- Each step is pcall'd individually, to pin down exactly which one failed.
local function step(name, fn)
  local ok, err = pcall(fn)
  log('  %-18s %s', name, ok and 'ok' or ('FAILED: ' .. tostring(err)))
  return ok
end

-- When the workspace isn't active, the target pane's mux window has no
-- native GUI window assigned, so gui_window() comes back nil (confirmed in
-- practice: "mux window id N is not currently associated with a gui
-- window"). Right after a workspace switch, the GUI side can lag behind by
-- a tick, so on failure this waits briefly and retries exactly once.
local function activate_target(target_id, mux_pane, mux_tab, mux_win)
  step('tab:activate', function() mux_tab:activate() end)
  step('pane:activate', function() mux_pane:activate() end)

  local gui_win
  step('mux:gui_window', function() gui_win = mux_win:gui_window() end)
  if gui_win then
    step('gui_window:focus', function() gui_win:focus() end)
    return true
  end
  log '  gui_window is nil (そのウィンドウに GUI が無い)'
  return false
end

-- This has been gotten wrong twice before, so the history is recorded
-- here. `wezterm cli activate-pane` has a known bug where it can't bring a
-- different native window to the front
-- (https://github.com/wezterm/wezterm/issues/5536); only
-- gui_window:focus() inside the GUI process can avoid that (confirmed in
-- practice: it successfully brought a different window to front, while a
-- side-by-side test of the cli path didn't even generate a focus event).
-- That's why the TUI side (jump() in ../src/wezterm.rs) delegates here via
-- an OSC 1337 SetUserVar rather than falling back to the cli path.
local function handle_jump(value)
  log('jump: value=%q', tostring(value))

  -- The value is a pane_id. Confirmed in practice that user-var-changed
  -- fires even when re-setting the same value, so no nonce is needed for
  -- repeated jumps.
  local target_id = tonumber(tostring(value):match '^(%d+)')
  if not target_id then
    log '  parse failed'
    return
  end

  local mux_pane, mux_tab, mux_win = find_pane(target_id)
  if not mux_pane then
    log('  pane %d not found', target_id)
    return
  end
  log('  target pane=%d tab=%d window=%d', target_id, mux_tab:tab_id(), mux_win:window_id())

  -- When the target pane is in a different workspace, tab:activate /
  -- pane:activate are mux-level state operations that succeed regardless
  -- of workspace, but an inactive workspace's mux window has no real
  -- native GUI window backing it, so gui_window()/focus() pass through
  -- silently and nothing happens (confirmed via real logs). So switch the
  -- workspace itself first, to bring the target window to the front.
  local wok, target_ws = pcall(function() return mux_win:get_workspace() end)
  local aok, active_ws = pcall(wezterm.mux.get_active_workspace)
  if wok and aok and target_ws ~= active_ws then
    log('  workspace switch: %s -> %s', tostring(active_ws), tostring(target_ws))
    step('set_active_workspace', function() wezterm.mux.set_active_workspace(target_ws) end)
  end

  if not activate_target(target_id, mux_pane, mux_tab, mux_win) then
    pcall(wezterm.time.call_after, 0.1, function()
      activate_target(target_id, mux_pane, mux_tab, mux_win)
    end)
  end

  -- A jump means the user is now looking at the target, so mark it read
  -- here rather than waiting for update-status to notice the active pane
  -- changed. That event can stop firing for a window for good (see
  -- with_status_dir), and read-tracking shouldn't hinge on it.
  log('  mark_read pane=%d', target_id)
  mark_read(target_id, current_status_value(mux_pane))
end

--------------------------------------------------------------------------
-- Resident dashboard
--------------------------------------------------------------------------

-- The dashboard key keeps one `wezterm-agents --resident` in a workspace of
-- its own and switches to it, instead of spawning a launcher tab on every
-- press. Spawning each time burns a tab id per press (mux ids are never
-- reused), so tab ids shown in the tab bar kept climbing.
local DASHBOARD_WORKSPACE = 'wezterm-agents-dashboard'
local DEFAULT_DASHBOARD_KEY = { key = 'a', mods = 'CMD|SHIFT' }

-- Kept in sync with DASHBOARD_ORIGIN_FILE in ../src/wezterm.rs. The
-- resident process was spawned long ago, so an env var can't tell it which
-- pane the key was pressed from; this file does (read once and removed on
-- the TUI side).
local DASHBOARD_ORIGIN_FILE = 'dashboard-origin'

local function workspace_exists(name)
  local ok, names = pcall(wezterm.mux.get_workspace_names)
  if not ok then
    return false
  end
  for _, n in ipairs(names) do
    if n == name then
      return true
    end
  end
  return false
end

-- Back to where the dashboard was opened from. Switching the workspace
-- alone isn't enough: SwitchToWorkspace re-lays out every mux window of
-- that workspace into GUI windows, and which of them ends up in front is up
-- to wezterm — with several windows, often not the one the user came from.
-- So go back to the origin pane the same way a jump does (handle_jump
-- switches the workspace and then focuses the pane's own window). Only if
-- that pane has since closed, fall back to switching the workspace: the
-- previous one, else any other; with none left, stay put rather than
-- creating a fresh workspace.
local function dashboard_back(window, pane)
  local origin = wezterm.GLOBAL.wezterm_agents_origin_pane
  if origin and find_pane(origin) then
    log('dashboard back: -> pane %d', origin)
    handle_jump(tostring(origin))
    return
  end

  local prev = wezterm.GLOBAL.wezterm_agents_prev_workspace
  if not prev or prev == DASHBOARD_WORKSPACE or not workspace_exists(prev) then
    prev = nil
    local ok, names = pcall(wezterm.mux.get_workspace_names)
    for _, n in ipairs(ok and names or {}) do
      if n ~= DASHBOARD_WORKSPACE then
        prev = n
        break
      end
    end
  end
  log('dashboard back: -> %s', tostring(prev))
  if prev then
    window:perform_action(wezterm.action.SwitchToWorkspace { name = prev }, pane)
  end
end

local function toggle_dashboard(window, pane)
  local current = window:active_workspace()
  if current == DASHBOARD_WORKSPACE then
    dashboard_back(window, pane)
    return
  end

  -- wezterm.GLOBAL rather than a local so it survives config reloads.
  wezterm.GLOBAL.wezterm_agents_prev_workspace = current
  wezterm.GLOBAL.wezterm_agents_origin_pane = pane:pane_id()
  local dir = status_dir()
  if dir then
    write_file(dir .. '/' .. DASHBOARD_ORIGIN_FILE, tostring(pane:pane_id()))
  end
  -- SwitchToWorkspace only uses `spawn` when the workspace doesn't exist
  -- yet, so this both starts the dashboard the first time and just
  -- switches to it afterwards.
  log('dashboard open: from=%s pane=%d', current, pane:pane_id())
  window:perform_action(
    wezterm.action.SwitchToWorkspace {
      name = DASHBOARD_WORKSPACE,
      spawn = { args = { resolved_bin or bins[1], '--resident' } },
    },
    pane
  )
end

-- For binding the dashboard to a key of your own (with `dashboard_key =
-- false`), e.g. `{ key = 'd', mods = 'LEADER', action = agents.toggle_dashboard }`.
M.toggle_dashboard = wezterm.action_callback(toggle_dashboard)

--------------------------------------------------------------------------
-- Status composition (composable primitive)
--------------------------------------------------------------------------

-- Detects "actively generating a response" (Claude Code) from a spinner
-- character at the start of the pane title. This watches a signal the
-- agent itself puts on screen rather than a hook, so it still tracks
-- correctly even in cases where no terminating hook fires, like an Esc
-- interrupt.
--
-- There are two variants; which one is used depends on the Claude Code
-- version.
--   - Braille spinner ⠀-⣿ (U+2800-U+28FF) (older Claude Code)
--   - Circular spinner ◐◓◑◒ (U+25D0-U+25D3) (current Claude Code, confirmed
--     on 2.1.233)
local function is_working_title(title)
  local b1, b2, b3 = title:byte(1, 3)
  if b1 == 0xE2 and b2 and b2 >= 0xA0 and b2 <= 0xA3 then
    return true
  end
  if b1 == 0xE2 and b2 == 0x97 and b3 and b3 >= 0x90 and b3 <= 0x93 then
    return true
  end
  return false
end

-- Copilot CLI doesn't put a spinner in the title; instead it renders a
-- status line like "◉ Working ..." on the screen's last row.
-- get_lines_as_text(1) returns just that bottom line (confirmed in
-- practice), so calling it on every redraw is cheap. The spinner cycles
-- through 4 frames — ○ ◎ ◉ ● — a circle filling in.
local COPILOT_SPINNER_FRAMES = { '○', '◎', '◉', '●' }
local function copilot_is_working(pane_id)
  local ok, mp = pcall(wezterm.mux.get_pane, pane_id)
  if not ok or not mp then
    return false
  end
  local ok2, last_line = pcall(function() return mp:get_lines_as_text(1) end)
  if not ok2 or not last_line then
    return false
  end
  for _, frame in ipairs(COPILOT_SPINNER_FRAMES) do
    if last_line:find(frame, 1, true) then
      return true
    end
  end
  return false
end

-- `pane_info` is expected to have the same shape as `tab.active_pane` (the
-- tab info table passed to format-tab-title): any table with `.pane_id`
-- `.title` `.user_vars` `.foreground_process_name` works, it doesn't need
-- to be an actual Pane object.
--
-- Returns nil (nothing to display) or
-- `{ state = 'working'|'waiting'|'done', agent = 'claude'|'copilot'|<string>,
--    icon = <string>, color = <hex string> }`.
--
-- [rendering-cost constraint] This function can be called on every tab-bar
-- redraw. It only does table lookups and byte comparisons, never file I/O
-- or spawning a subprocess (the one exception is `copilot_is_working`'s
-- `get_lines_as_text`, a lightweight mux call that just reads the pane's
-- screen buffer — it never touches disk or an external process).
function M.status(pane_info)
  local user_vars = pane_info.user_vars or {}
  local title = pane_info.title or ''

  -- The agent kind. The hook sends agent_name, so that's preferred; if
  -- it's absent, infer it from the foreground process name / pane title
  -- (so `working` detection still works even on a pane that hasn't sent a
  -- single notification yet).
  local agent_name = user_vars.agent_name
  local process = (pane_info.foreground_process_name or ''):match '[^/]+$' or ''
  local title_lower = title:lower()
  if not agent_name then
    if process == 'claude' then
      agent_name = 'claude'
    elseif process == 'copilot' or process == 'copilot-cli'
        or (process == 'node' and title_lower:find('copilot', 1, true)) then
      agent_name = 'copilot'
    end
  end

  local status = user_vars.agent_status or user_vars.claude_status or user_vars.copilot_status
  if not agent_name and not status then
    return nil
  end

  local is_thinking = is_working_title(title)
  if not is_thinking and agent_name == 'copilot' then
    is_thinking = copilot_is_working(pane_info.pane_id)
  end
  if is_thinking then
    return { state = 'working', agent = agent_name, icon = icons.working, color = colors.working }
  end

  -- A done that's been read is treated as idle, reverting to the normal
  -- color. waiting stays displayed even when read, since it means input is
  -- actually being requested right now.
  if status == 'done' and M.is_dismissed(pane_info.pane_id) then
    status = nil
  end
  if status ~= 'waiting' and status ~= 'done' then
    return nil
  end
  return { state = status, agent = agent_name, icon = icons[status], color = colors[status] }
end

--------------------------------------------------------------------------
-- Bell notification
--------------------------------------------------------------------------

local AGENT_LABELS = { claude = 'Claude Code', copilot = 'Copilot CLI' }
local BELL_MESSAGES = { waiting = '承認/入力待ちです', done = '応答が完了しました' }

-- The wezterm-agents hook only sends a BEL for waiting/done, and writes the
-- current time (epoch seconds) to agent_bell right before it. This only
-- toasts a bell when its agent_bell is "within the last few seconds" and
-- "different from the last one rung for this pane". Since a SetUserVar
-- can't be cleared, this means something like a zsh tab-completion bell
-- after returning to the prompt sees a stale/repeated agent_bell and
-- doesn't get mistakenly replayed as a toast.
local toasted_bell = {}

local function on_bell(window, pane)
  local user_vars = pane:get_user_vars()

  local nonce = user_vars.agent_bell
  if not nonce or nonce == '' then
    return
  end
  local pane_id = pane:pane_id()
  if toasted_bell[pane_id] == nonce then
    return
  end
  -- Also check freshness, as a safety net for when a config reload wipes
  -- the memory above.
  local ts = tonumber(nonce)
  local now = os.time()
  if ts and (now - ts) > 10 then
    return
  end
  toasted_bell[pane_id] = nonce

  local status, label
  if user_vars.agent_status then
    status = user_vars.agent_status
    local name = user_vars.agent_name
    label = (name and AGENT_LABELS[name]) or name or 'AI エージェント'
  elseif user_vars.claude_status then
    label, status = AGENT_LABELS.claude, user_vars.claude_status
  elseif user_vars.copilot_status then
    label, status = AGENT_LABELS.copilot, user_vars.copilot_status
  else
    return
  end

  local message = BELL_MESSAGES[status]
  if not message then
    return
  end

  local cwd_uri = pane:get_current_working_dir()
  local cwd = cwd_uri and (cwd_uri.file_path:match '[^/]+/?$' or '') or ''

  window:toast_notification(label .. ': ' .. cwd, message, nil, 5000)
end

--------------------------------------------------------------------------
-- Default tab rendering (only used when tab_title = true)
--------------------------------------------------------------------------

-- A minimal setup for users without their own tab rendering. Shows only
-- the cwd and an icon. For a fancier look, set `tab_title = false` and call
-- `M.status()` from your own `format-tab-title` (see this repo's own
-- .config/wezterm/wezterm.lua for an example).
local function default_format_tab_title(tab)
  local pane = tab.active_pane
  local cwd = ''
  if pane.current_working_dir then
    local path = pane.current_working_dir.file_path
    cwd = (path:match '[^/]+/?$' or path):gsub('/$', '')
  end
  local title = string.format(' %d. %s ', tab.tab_id, cwd)

  local st = M.status(pane)
  if not st then
    return { { Text = title } }
  end
  local status_title = string.format(' %d. %s %s ', tab.tab_id, st.icon, cwd)
  return {
    { Foreground = { Color = st.color } },
    { Text = status_title },
  }
end

--------------------------------------------------------------------------
-- Shell integration (ZDOTDIR injection)
--------------------------------------------------------------------------

-- Installs the `claude` shell function (the Claude Code hooks-injection
-- shim) into every zsh wezterm spawns, without touching the user's own rc
-- files.
--
-- How it works: zsh reads `$ZDOTDIR/.zshenv` first, before anything else.
-- Pointing ZDOTDIR at the bundled shell-integration/ via
-- `set_environment_variables` means that directory's `.zshenv` runs first,
-- restores the real ZDOTDIR, sources the user's own .zshenv in its place,
-- and only then defines the shim (see shell-integration/.zshenv's comments
-- for the details).
--
-- This runs exactly once, during config evaluation. It checks whether
-- files exist (io.open) but never spawns a child process
-- (run_child_process isn't usable during config evaluation). For the same
-- reason, the binary's location is also decided purely by checking whether
-- candidate paths exist.
local function setup_shell_integration(config, opts)
  local dir = opts.plugin_dir and (opts.plugin_dir .. '/plugin/shell-integration')
    or SHELL_INTEGRATION_DIR
  -- A feature that's on by default shouldn't silently fail to work, so
  -- these two cases always warn to wezterm's own log
  -- (`wezterm-gui-log-*.txt`), regardless of `debug`. It only runs once
  -- during config evaluation, so it isn't noisy.
  if not dir then
    wezterm.log_warn(
      'wezterm-agents: shell_integration を有効にできません: プラグインの場所を'
        .. '判別できませんでした。dofile のパスが長い場合は apply_to_config に'
        .. ' plugin_dir = "<リポジトリのルート>" を渡すか、shell_integration = false'
        .. ' にしてください'
    )
    return
  end
  -- Pointing ZDOTDIR there without the bundled file present would produce
  -- a bare zsh that never reads the user's .zshrc. Doing nothing is the
  -- safe fallback when it's missing.
  if not file_exists(dir .. '/.zshenv') then
    wezterm.log_warn(
      'wezterm-agents: shell_integration を有効にできません: ' .. dir .. '/.zshenv がありません'
    )
    return
  end

  -- Merge in, preserving any variables another plugin or the user's own
  -- config already set.
  local env = config.set_environment_variables or {}
  -- Stash the pre-injection ZDOTDIR (whatever the config already set it
  -- to, or wezterm-gui's own environment otherwise), to be restored on the
  -- .zshenv side.
  local orig = env.ZDOTDIR
  if not orig then
    orig = os.getenv 'ZDOTDIR'
    -- If wezterm-gui itself was launched from a "shell that doesn't
    -- restore ZDOTDIR" — e.g. a bash pane inside a previous wezterm — the
    -- inherited ZDOTDIR is actually the bundled directory a previous
    -- instance injected. Stashing that as "the real value" would make
    -- .zshenv restore itself and read itself recursively, losing the
    -- user's actual ZDOTDIR. So this checks the bundled file's marker
    -- (first line) and uses the already-stashed value instead when it
    -- matches.
    if orig and orig ~= '' and is_plugin_shell_integration(orig) then
      orig = os.getenv 'WEZTERM_AGENTS_ZDOTDIR'
    end
  end
  if orig and orig ~= '' then
    env.WEZTERM_AGENTS_ZDOTDIR = orig
  end
  env.ZDOTDIR = dir
  for _, bin in ipairs(bins) do
    -- A bare name resolved via PATH can't be existence-checked, so only
    -- absolute-path candidates are considered.
    if bin:sub(1, 1) == '/' and file_exists(bin) then
      env.WEZTERM_AGENTS_BIN = bin
      break
    end
  end
  config.set_environment_variables = env
  log('shell_integration: ZDOTDIR=%s bin=%s', dir, tostring(env.WEZTERM_AGENTS_BIN))
end

--------------------------------------------------------------------------
-- Entry point
--------------------------------------------------------------------------

-- @param config The config object built by `wezterm.config_builder()`.
-- @param opts (optional)
--   icons        {working=,waiting=,done=} icons overriding the defaults
--   colors       {working=,waiting=,done=} colors (hex strings) overriding
--                the defaults
--   bin          Explicit path to the `wezterm-agents` binary. When
--                omitted, tries `~/.local/bin/wezterm-agents` then PATH.
--                Setting this also turns auto_install off.
--   auto_install true downloads the release binary matching this plugin's
--                version (from GitHub Releases, checksum-verified) to
--                `~/.local/bin/wezterm-agents` on startup, and keeps it in
--                step when the plugin is updated (default true). Never
--                replaces a symlink or a binary it didn't install itself
--                (e.g. a local `cargo build`); see install.sh. Runs after
--                the first window appears, so on a very first launch the
--                Claude Code hooks only take effect in tabs opened after
--                the "installed" toast. false disables it.
--   debug        true leaves a diagnostic log at
--                `<status_dir>/wezterm-agents-debug.log` (default false)
--   tab_title    true has this plugin register `format-tab-title` itself,
--                rendering just the cwd and status (default false).
--                [important] WezTerm only runs the first registered
--                format-tab-title handler. If you set this true, don't
--                register your own. If you already have your own
--                rendering, leave this false and call `M.status(pane)`
--                from inside it.
--   bell_toast   true sets `config.audible_bell = 'Disabled'` and converts
--                waiting/done bells into toast notifications (default
--                true). Multiple 'bell' handlers can coexist, so this
--                doesn't conflict with any existing bell handling.
--   shell_integration
--                true automatically installs a `claude` shell function
--                (which injects Claude Code's hooks) into every zsh
--                wezterm spawns (default true). Works by pointing ZDOTDIR
--                at the bundled shell-integration/, never touching the
--                user's own rc files. false disables it. Shells other than
--                zsh aren't covered yet. Doesn't reach a zsh started
--                nested inside a pane — combine with
--                `wezterm-agents install claude` if you need that too.
--   plugin_dir   This repo's root (the parent of `plugin/`, holding
--                install.sh and shell-integration/). Normally
--                auto-detected (via `wezterm.plugin.require`, or `dofile`
--                with a short enough path). Only needed when detection
--                fails, which shows up in the debug log.
--   lang         Dashboard (`wezterm-agents` / `--watch`) display language:
--                'en' (default) or 'ja'. Passed to the `wezterm-agents`
--                binary via WEZTERM_AGENTS_LANG, so it also applies to
--                notification text written by the hook subcommand.
--   dashboard_key
--                {key=, mods=} that toggles the resident dashboard: opens
--                `wezterm-agents --resident` in its own workspace
--                ('wezterm-agents-dashboard'), and switches back when
--                pressed there (default { key = 'a', mods = 'CMD|SHIFT' }).
--                Appended to `config.keys`, so assign your own
--                `config.keys` before calling apply_to_config, or append
--                to it afterwards. false binds nothing; bind
--                `M.toggle_dashboard` yourself if you want it elsewhere.
--
-- Pane-jump and read-tracking (reading/writing the TUI's state files) are
-- always wired up regardless of the settings above.
function M.apply_to_config(config, opts)
  opts = opts or {}

  if opts.icons then
    local merged = {}
    for k, v in pairs(DEFAULT_ICONS) do
      merged[k] = opts.icons[k] or v
    end
    icons = merged
  end
  if opts.colors then
    local merged = {}
    for k, v in pairs(DEFAULT_COLORS) do
      merged[k] = opts.colors[k] or v
    end
    colors = merged
  end
  if opts.debug then
    debug_enabled = true
  end
  if opts.lang then
    local env = config.set_environment_variables or {}
    env.WEZTERM_AGENTS_LANG = opts.lang
    config.set_environment_variables = env
  end
  ui_lang = opts.lang or ui_lang
  bins = bin_candidates(opts.bin)

  -- An explicit `bin` means the user manages the binary themselves.
  if opts.auto_install ~= false and not (opts.bin and opts.bin ~= '') then
    local root = opts.plugin_dir or REPO_ROOT
    if not root then
      wezterm.log_warn(
        'wezterm-agents: auto_install を有効にできません: プラグインの場所を'
          .. '判別できませんでした。apply_to_config に plugin_dir = "<リポジトリのルート>"'
          .. ' を渡すか、auto_install = false にしてください'
      )
    elseif not file_exists(root .. '/install.sh') then
      wezterm.log_warn('wezterm-agents: auto_install を有効にできません: ' .. root .. '/install.sh がありません')
    else
      auto_install_root = root
    end
  end

  if opts.shell_integration ~= false then
    setup_shell_integration(config, opts)
  end

  if opts.dashboard_key ~= false then
    local k = opts.dashboard_key or DEFAULT_DASHBOARD_KEY
    config.keys = config.keys or {}
    table.insert(config.keys, { key = k.key, mods = k.mods, action = M.toggle_dashboard })
  end

  wezterm.on('user-var-changed', function(window, pane, name, value)
    if name == 'wezterm_agents_jump' then
      handle_jump(value)
      return
    end
    if name == 'wezterm_agents_back' then
      dashboard_back(window, pane)
      return
    end
    if not STATUS_VARS[name] then
      return
    end
    if value == 'working' then
      return
    end

    local pane_id = pane:pane_id()

    -- Mark a notification as read immediately if it arrives while it's
    -- being watched. Otherwise the tab in front of you would keep
    -- pointlessly lighting up for up to a second, until update-status's
    -- next tick.
    local watching = false
    pcall(function()
      watching = window:is_focused() and window:active_pane():pane_id() == pane_id
    end)

    if watching then
      mark_read(pane_id, value)
    else
      dismissed[pane_id] = false
    end
    log('status: pane=%d value=%s watching=%s', pane_id, tostring(value), tostring(watching))
  end)

  -- Marks read based on changes to the active pane. update-status fires
  -- roughly once a second, so this returns immediately when the value is
  -- unchanged, to avoid a file write every second.
  wezterm.on('update-status', function(window, pane)
    if not window or not pane then
      return
    end
    -- Restoring read state happens exactly once, here. The state directory
    -- can't be resolved during config evaluation (see status_dir()'s
    -- comment), so this is deferred until the first event. update-status
    -- fires within about a second, so there's no perceptible delay.
    -- with_status_dir keeps the child-process spawn out of this handler (see
    -- its comment for why that matters).
    if not restored_once then
      restored_once = true
      -- The install check goes first so the state directory is resolved
      -- against the freshly installed binary. It's also deferred to a timer
      -- for the same reason as with_status_dir.
      local ok = auto_install_root
        and pcall(wezterm.time.call_after, 0, function()
          ensure_binary(window)
          with_status_dir(restore_dismissed)
        end)
      if not ok then
        with_status_dir(restore_dismissed)
      end
    end
    local ok, window_id = pcall(function() return window:window_id() end)
    if not ok then
      return
    end
    local pane_id = pane:pane_id()
    if last_active[window_id] == pane_id then
      return
    end
    last_active[window_id] = pane_id
    log('update-status: window=%s active pane=%d -> mark_read', tostring(window_id), pane_id)
    mark_read(pane_id, current_status_value(pane))
  end)

  if opts.tab_title then
    wezterm.on('format-tab-title', function(tab) return default_format_tab_title(tab) end)
  end

  if opts.bell_toast ~= false then
    config.audible_bell = 'Disabled'
    wezterm.on('bell', on_bell)
  end

  -- Creating the state directory is already handled inside `wezterm-agents
  -- status-dir` (created mode 0700, with owner and mode validated — see
  -- ../src/paths.rs). We don't mkdir it here: doing so would create it
  -- with whatever mode the GUI process's umask happens to produce,
  -- defeating the point of that validation.
end

return M
