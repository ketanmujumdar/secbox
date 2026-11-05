#!/usr/bin/env bash
# Usage: scripts/rename.sh <newname>   (lowercase letters and digits, starting with a letter)
# Rewrites the current product name everywhere it appears in project text files.
set -euo pipefail
new="${1:?usage: rename.sh <newname>}"
[[ "$new" =~ ^[a-z][a-z0-9]*$ ]] || { echo "name must be lowercase letters and digits" >&2; exit 1; }
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
old="$(sed -n 's/^pub const PRODUCT_NAME: &str = "\(.*\)";/\1/p' crates/brand/src/lib.rs)"
[ -n "$old" ] || { echo "could not read the current name" >&2; exit 1; }
[ "$old" != "$new" ] || { echo "already named $new"; exit 0; }
oldU="$(printf '%s' "$old" | tr '[:lower:]' '[:upper:]')"; newU="$(printf '%s' "$new" | tr '[:lower:]' '[:upper:]')"
oldT="$(printf '%s' "${old:0:1}" | tr '[:lower:]' '[:upper:]')${old:1}"; newT="$(printf '%s' "${new:0:1}" | tr '[:lower:]' '[:upper:]')${new:1}"
excluded=(-not -path './target/*' -not -path './.git/*' -not -path './.tools/*' -not -path './.spike/*')
# Lines naming the unrelated openSUSE package keep the old name, since that
# package does not change. Review the "Final name" row in the docs by hand.
find . -type f \( -name '*.rs' -o -name '*.toml' -o -name '*.md' -o -name '*.sh' -o -name '*.yaml' -o -name '*.yml' -o -name '*.rb' -o -name '*.py' -o -name 'Cargo.lock' \) \
  "${excluded[@]}" -print0 |
  xargs -0 perl -pi -e "unless (/openSUSE/i) { s/$old/$new/g; s/$oldU/$newU/g; s/$oldT/$newT/g }"
# File names carry the name too, such as the formula and config fixtures. The
# deepest paths go first, so that a parent is renamed after its children.
find . -depth -name "*$old*" "${excluded[@]}" -print0 |
  while IFS= read -r -d '' path; do
    base="$(basename "$path")"
    mv "$path" "$(dirname "$path")/${base//$old/$new}"
  done
echo "renamed $old to $new; the folder name is unchanged"
