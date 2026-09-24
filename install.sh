#!/bin/sh
# wezterm-agents installer: downloads a prebuilt `wezterm-agents` binary
# from GitHub Releases, verifies it against the release's SHASUMS256.txt,
# and puts it at ~/.local/bin/wezterm-agents (the canonical location the
# plugin, the hook config, and shell-integration/.zshenv all look at).
#
# Two callers:
#   - By hand:  curl -fsSL https://raw.githubusercontent.com/shirokuroiori/wezterm-agents/main/install.sh | sh
#   - The WezTerm plugin (plugin/init.lua), on startup, with --managed-only
#     and the version the plugin checkout wants.
#
# Usage: install.sh [--version X.Y.Z] [--target <triple>] [--dest <path>]
#                   [--managed-only] [--force]
#
#   --version       Release to install (default: the latest release)
#   --target        Rust target triple (default: detected via uname)
#   --dest          Where to put the binary (default: ~/.local/bin/wezterm-agents)
#   --managed-only  Only replace a binary this script installed itself (the
#                   plugin's mode). Without it, a regular file at --dest is
#                   replaced too
#   --force         Replace whatever is at --dest, symlinks included (e.g. to
#                   go back from a `cargo build` dev symlink to a release)
#
# A symlink at --dest is never replaced without --force: that's the
# documented way to run a local `cargo build`, and silently swapping it for
# a release would be surprising.
#
# The first line of stdout is one word saying what happened, for the plugin
# to parse (wezterm's run_child_process only reports success/failure, not
# the exit code): `installed`, `updated`, `current` (already the wanted
# version), or `skipped` (left alone; the reason goes to stderr). Any
# failure exits non-zero.
#
# Ownership record: ${XDG_DATA_HOME:-~/.local/share}/wezterm-agents/installed
# holds `<sha256> <version> <dest>` of the last binary this script put in
# place. A binary at --dest whose sha256 matches is "ours" and may be
# updated; anything else is the user's.

set -eu

REPO=${WEZTERM_AGENTS_REPO:-shirokuroiori/wezterm-agents}

version=
target=
dest=
managed_only=0
force=0

die() {
  echo "wezterm-agents install: $*" >&2
  exit 1
}

note() {
  echo "wezterm-agents install: $*" >&2
}

while [ $# -gt 0 ]; do
  case $1 in
    --version) [ $# -ge 2 ] || die "--version needs a value"; version=${2#v}; shift 2 ;;
    --target) [ $# -ge 2 ] || die "--target needs a value"; target=$2; shift 2 ;;
    --dest) [ $# -ge 2 ] || die "--dest needs a value"; dest=$2; shift 2 ;;
    --managed-only) managed_only=1; shift ;;
    --force) force=1; shift ;;
    -h | --help)
      echo "usage: install.sh [--version X.Y.Z] [--target <triple>] [--dest <path>] [--managed-only] [--force]"
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

[ -n "${HOME-}" ] || die "HOME is not set"
[ -n "$dest" ] || dest=$HOME/.local/bin/wezterm-agents
case $dest in
  /*) ;;
  *) dest=$(pwd)/$dest ;;
esac
dest_dir=$(dirname "$dest")
record=${XDG_DATA_HOME:-$HOME/.local/share}/wezterm-agents/installed

command -v curl >/dev/null 2>&1 || die "curl is required"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    die "sha256sum or shasum is required"
  fi
}

detect_target() {
  os=$(uname -s)
  arch=$(uname -m)
  case $arch in
    arm64 | aarch64) arch=aarch64 ;;
    x86_64 | amd64) arch=x86_64 ;;
    *) die "unsupported CPU architecture: $arch" ;;
  esac
  case $os in
    Darwin)
      # A shell under Rosetta reports x86_64; prefer the native binary.
      if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
        arch=aarch64
      fi
      echo "$arch-apple-darwin"
      ;;
    Linux) echo "$arch-unknown-linux-gnu" ;;
    *) die "unsupported OS: $os" ;;
  esac
}

[ -n "$target" ] || target=$(detect_target)

# `wezterm-agents --version` prints `wezterm-agents X.Y.Z`. Releases before
# the flag existed just fail, which reads as "unknown version".
installed_version() {
  "$dest" --version 2>/dev/null | sed -n 's/^wezterm-agents //p' || true
}

# Decide whether --dest may be touched, before any network access. The
# plugin runs this on every config load, so the common "nothing to do" case
# has to stay offline and cheap. Version is compared later once known.
state=missing
if [ -L "$dest" ]; then
  state=symlink
elif [ -e "$dest" ]; then
  state=unmanaged
  if [ -r "$record" ]; then
    read -r rec_sha rec_version rec_dest <"$record" || true
    if [ "${rec_dest-}" = "$dest" ] && [ "${rec_sha-}" = "$(sha256_of "$dest")" ]; then
      state=managed
    fi
  fi
fi

# Whether --dest already holds $version. A managed binary is judged by the
# record (no need to run it); anything else by asking it.
is_current() {
  case $state in
    managed) [ "$rec_version" = "$version" ] ;;
    unmanaged) [ "$(installed_version)" = "$version" ] ;;
    *) return 1 ;;
  esac
}

if [ "$force" = 0 ]; then
  if [ -n "$version" ] && is_current; then
    echo current
    exit 0
  fi
  case $state in
    symlink)
      echo skipped
      note "$dest is a symlink (a local build?); leaving it alone. Use --force to replace it"
      exit 0
      ;;
    unmanaged)
      if [ "$managed_only" = 1 ]; then
        echo skipped
        note "$dest was not installed by this script; leaving it alone. Use --force to replace it"
        exit 0
      fi
      ;;
  esac
fi

if [ -z "$version" ]; then
  # /releases/latest redirects to /releases/tag/vX.Y.Z. Following that
  # redirect avoids the REST API and its unauthenticated rate limit.
  latest_url=$(curl -fsSLo /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") ||
    die "couldn't look up the latest release"
  version=${latest_url##*/v}
  case $version in
    [0-9]*) ;;
    *) die "couldn't parse the latest release from $latest_url" ;;
  esac
  if [ "$force" = 0 ] && is_current; then
    echo current
    exit 0
  fi
fi

asset=wezterm-agents-$target
base=https://github.com/$REPO/releases/download/v$version

mkdir -p "$dest_dir"

# Serialize concurrent runs (several wezterm windows, a config reload while
# a download is in flight). mkdir is atomic; a lock older than 10 minutes is
# treated as left behind by a killed run.
lock=$dest_dir/.wezterm-agents-install.lock
if ! mkdir "$lock" 2>/dev/null; then
  if [ -n "$(find "$lock" -maxdepth 0 -mmin +10 2>/dev/null)" ]; then
    rmdir "$lock" 2>/dev/null || true
  fi
  if ! mkdir "$lock" 2>/dev/null; then
    echo skipped
    note "another install is in progress ($lock)"
    exit 0
  fi
fi

# The temp file sits next to --dest so the final mv is an atomic rename on
# the same filesystem.
tmp=
sums=
cleanup() {
  [ -z "$tmp" ] || rm -f "$tmp"
  [ -z "$sums" ] || rm -f "$sums"
  rmdir "$lock" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
tmp=$(mktemp "$dest_dir/.wezterm-agents.XXXXXX")
sums=$(mktemp "$dest_dir/.wezterm-agents-sums.XXXXXX")

curl -fsSL -o "$sums" "$base/SHASUMS256.txt" || die "couldn't download $base/SHASUMS256.txt"
want=$(awk -v a="$asset" '$2 == a || $2 == "*" a { print $1 }' "$sums")
[ -n "$want" ] || die "no $asset in v$version's SHASUMS256.txt (unsupported target?)"
curl -fsSL -o "$tmp" "$base/$asset" || die "couldn't download $base/$asset"
got=$(sha256_of "$tmp")
[ "$got" = "$want" ] || die "checksum mismatch for $asset (expected $want, got $got)"

chmod 755 "$tmp"
mv -f "$tmp" "$dest"
tmp=

mkdir -p "$(dirname "$record")"
printf '%s %s %s\n' "$got" "$version" "$dest" >"$record"

if [ "$state" = missing ]; then
  echo installed
else
  echo updated
fi
note "installed wezterm-agents v$version ($target) to $dest"

case :${PATH-}: in
  *:"$dest_dir":*) ;;
  *) note "note: $dest_dir is not on your PATH; add it to run \`wezterm-agents\` from a shell" ;;
esac
