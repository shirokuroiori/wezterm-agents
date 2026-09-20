# wezterm-agents: zsh shell integration
#
# The wezterm plugin (../init.lua's apply_to_config) points ZDOTDIR at this
# directory via `set_environment_variables`, so every zsh wezterm starts
# reads this file first. It exists to define the `claude` shell function
# (Claude Code's hooks-injection shim) without ever modifying the user's own
# rc files. kitty / Ghostty inject their shell integration the same way.
#
# There are 3 things to do here, and the order matters:
#   1. Restore ZDOTDIR to its real value. zsh re-evaluates ZDOTDIR after
#      reading each startup file, so restoring it here means the following
#      .zprofile / .zshrc / .zlogin are read normally, as the user's own.
#   2. Read the user's actual .zshenv, which would otherwise have been read
#      first.
#   3. Define the `claude` shell function.
# Step 1 comes first so that if 2 or 3 fails for any reason, the shell still
# comes up as a plain zsh (if ZDOTDIR kept pointing here, .zshrc would never
# be found and the shell would end up unconfigured).
#
# Constraint: since step 1 restores ZDOTDIR, a zsh started from inside this
# one doesn't go through this file. If you also want the shim in a nested
# zsh, run `wezterm-agents install claude` to append it to ~/.zshenv (having
# both enabled doesn't double-define it — see the check in step 3).

# 1. Restore ZDOTDIR. The plugin stashes the pre-injection value in
#    WEZTERM_AGENTS_ZDOTDIR.
if [[ -n ${WEZTERM_AGENTS_ZDOTDIR-} ]]; then
  export ZDOTDIR=$WEZTERM_AGENTS_ZDOTDIR
else
  unset ZDOTDIR
fi
unset WEZTERM_AGENTS_ZDOTDIR

# 2. The user's actual .zshenv. If it re-sets ZDOTDIR itself (the common
#    dotfiles convention of putting `export ZDOTDIR=~/.config/zsh` in
#    .zshenv), that takes effect for the files that follow too.
if [[ -r ${ZDOTDIR:-$HOME}/.zshenv ]]; then
  source "${ZDOTDIR:-$HOME}/.zshenv"
fi

# 3. The claude shim. If a claude function is already defined on the user's
#    side (from `wezterm-agents install claude`'s appended block, or their
#    own definition), respect it and do nothing.
#    The binary's location is resolved by the plugin at config-evaluation
#    time and passed via WEZTERM_AGENTS_BIN; if that isn't set, fall back to
#    the default install location. PATH is not consulted, since at .zshenv
#    time any PATH additions from .zshrc haven't taken effect yet.
if ! typeset -f claude >/dev/null 2>&1; then
  if [[ -n ${WEZTERM_AGENTS_BIN-} && -x $WEZTERM_AGENTS_BIN ]]; then
    eval "$("$WEZTERM_AGENTS_BIN" init zsh)"
  elif [[ -x $HOME/.local/bin/wezterm-agents ]]; then
    eval "$("$HOME/.local/bin/wezterm-agents" init zsh)"
  fi
fi
