#!/usr/bin/env bash
# Usage: apps/desktop/scripts/prepare-sidecars.sh [--debug] [--target TRIPLE]
#
# Builds the command line tool and the daemon from the main workspace and
# copies them to apps/desktop/src-tauri/binaries/ with the target-triple
# suffix Tauri's externalBin expects, such as secboxd-aarch64-apple-darwin.
# Tauri strips the suffix again when it puts them beside the app's own
# executable, inside the bundle and in target/<profile>/ for a dev run.
#
# Run it before `cargo tauri build` or `cargo tauri dev`. The copies are
# git-ignored.
set -euo pipefail

profile=release
target=""
while [ $# -gt 0 ]; do
  case "$1" in
    --debug) profile=debug ;;
    --target) target="${2:?--target needs a triple}"; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

root="$(cd "$(dirname "$0")/../../.." && pwd)"
out="$root/apps/desktop/src-tauri/binaries"
host="$(rustc -vV | sed -n 's/^host: //p')"
triple="${target:-$host}"

args=(build --manifest-path "$root/Cargo.toml" -p cli -p daemon --bins)
[ "$profile" = release ] && args+=(--release)
[ -n "$target" ] && args+=(--target "$target")
cargo "${args[@]}"

if [ -n "$target" ]; then
  built="$root/target/$target/$profile"
else
  built="$root/target/$profile"
fi

mkdir -p "$out"
for bin in secbox secboxd; do
  install -m 0755 "$built/$bin" "$out/$bin-$triple"
  echo "copied $bin to binaries/$bin-$triple"
done
