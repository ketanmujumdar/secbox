#!/usr/bin/env bash
# Usage: apps/desktop/scripts/run-dev.sh [--build-only]
#
# Builds the desktop app and its sidecars from this checkout (debug), quits a
# copy of the app already running from here, stops a daemon running from here so
# the app starts the one just built, and opens the app. Sandboxes are separate
# processes and keep running.
#
# Stays in the foreground: Ctrl-C quits the app, and the daemon keeps running.
#
# The engine comes from .tools/msb/bin/msb in this checkout. Set
# SECBOX_ENGINE_BIN to use another.
set -euo pipefail

build_only=false
while [ $# -gt 0 ]; do
  case "$1" in
    --build-only) build_only=true ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

root="$(cd "$(dirname "$0")/../../.." && pwd)"
tauri="$root/apps/desktop/src-tauri"
app="$tauri/target/debug/desktop"
engine="${SECBOX_ENGINE_BIN:-$root/.tools/msb/bin/msb}"

"$root/apps/desktop/scripts/prepare-sidecars.sh" --debug
cargo build --manifest-path "$tauri/Cargo.toml"

[ "$build_only" = true ] && exit 0

if [ ! -x "$engine" ]; then
  echo "no engine at $engine. Set SECBOX_ENGINE_BIN to the absolute path of msb." >&2
  exit 1
fi

# Waits for a process matching the pattern to go, so the next start finds the
# daemon's socket free. Matches only processes run from this checkout.
stop_matching() {
  pkill -TERM -f "$1" 2>/dev/null || return 0
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    pgrep -f "$1" >/dev/null || return 0
    sleep 0.5
  done
  echo "still running after 5 seconds: $1" >&2
  exit 1
}

# An app started by hand from inside src-tauri shows as target/debug/desktop, so
# the relative form is matched too.
stop_matching "(^|/src-tauri/)target/debug/desktop$"
stop_matching "^$root/(apps/desktop/src-tauri/)?target/debug/secboxd$"

cd "$tauri"
export SECBOX_ENGINE_BIN="$engine"
exec "$app"
