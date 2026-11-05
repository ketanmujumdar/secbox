#!/bin/sh
# Builds the documentation site into guide/book, then adds the Markdown copies,
# llms.txt and llms-full.txt. Needs mdbook and python3.
#
#   guide/build.sh                         # build
#   guide/build.sh serve                   # build, then serve on http://localhost:3000
#   DOCS_BASE_URL=https://example.org/docs guide/build.sh
set -eu

cd "$(dirname "$0")"
base="${DOCS_BASE_URL:-https://ketanmujumdar.github.io/secbox}"

mdbook build
python3 -I tools/llms.py --base-url "$base"

if [ "${1:-}" = "serve" ]; then
    # mdbook serve would rebuild without the extra files, so serve the folder as it is.
    echo "Serving guide/book on http://localhost:3000 (Ctrl-C stops)"
    exec python3 -m http.server 3000 --bind 127.0.0.1 --directory book
fi
