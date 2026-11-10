#!/usr/bin/env bash
# Usage: scripts/render-homebrew.sh VERSION CLI_TARBALL DMG OUT_DIR
#
# Fills the templates in packaging/homebrew/ with a release's version and the
# sha256 of its two assets, and writes them in the tap's layout:
#   OUT_DIR/Formula/secbox.rb
#   OUT_DIR/Casks/secbox-desktop.rb
# The release workflow runs it with OUT_DIR set to a checkout of the tap.
set -euo pipefail

version="${1:?VERSION, such as 1.0.0}"
tarball="${2:?path to secbox-VERSION-darwin-aarch64.tar.gz}"
dmg="${3:?path to secbox-desktop-VERSION-aarch64.dmg}"
out="${4:?the tap checkout to write into}"

case "$version" in
  *[!0-9.]* | "" | .* | *. ) echo "not a plain version: $version" >&2; exit 2 ;;
esac

sha() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

root="$(cd "$(dirname "$0")/.." && pwd)"
cli_sha="$(sha "$tarball")"
dmg_sha="$(sha "$dmg")"

mkdir -p "$out/Formula" "$out/Casks"
sed -e "s/@VERSION@/$version/g" -e "s/@SHA256@/$cli_sha/g" \
  "$root/packaging/homebrew/secbox.rb" > "$out/Formula/secbox.rb"
sed -e "s/@VERSION@/$version/g" -e "s/@DMG_SHA256@/$dmg_sha/g" \
  "$root/packaging/homebrew/secbox-desktop.rb" > "$out/Casks/secbox-desktop.rb"

if grep -n '@[A-Z0-9_]*@' "$out/Formula/secbox.rb" "$out/Casks/secbox-desktop.rb"; then
  echo "a placeholder was left unfilled" >&2
  exit 1
fi
echo "wrote Formula/secbox.rb ($cli_sha) and Casks/secbox-desktop.rb ($dmg_sha) in $out"
