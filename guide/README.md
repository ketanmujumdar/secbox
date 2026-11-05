# secbox user documentation

This folder is the public documentation site, published to GitHub Pages at
<https://ketanmujumdar.github.io/secbox/>. To read it without building anything,
start at [`src/index.md`](src/index.md). Every page is plain Markdown and reads
fine on GitHub.

`docs/` at the repository root is different: design notes, spikes and test
checklists for people working on secbox itself.

## Layout

```text
guide/
  book.toml          site settings (mdBook)
  build.sh           builds the site into guide/book
  src/SUMMARY.md     the table of contents: a page is published only if listed here
  src/**/*.md        the pages
  src/images/        images used by the pages
  theme/llm.js, .css the "Copy page" and "Markdown" buttons
  tools/llms.py      writes llms.txt, llms-full.txt and a .md copy of each page
```

## Build and preview

You need [mdBook](https://rust-lang.github.io/mdBook/) 0.5 and Python 3:

```sh
cargo install mdbook --version 0.5.4 --locked
guide/build.sh serve        # then open http://localhost:3000
```

`mdbook serve guide` also works for quick edits, with live reload, but it
leaves out the Markdown copies and the `llms` files, so the Copy page button
fails there.

## Publishing

`.github/workflows/docs.yml` builds on every pull request that touches
`guide/`, and publishes on every push to `main`. One-time setup: in the
repository's **Settings > Pages**, set **Source** to **GitHub Actions**.

## Screenshots

Every terminal screenshot in `src/images/cli/` is real output, captured by
`screenshots/capture.py` from the programs built from this repository. Nothing
is drawn by hand. When a command is added or its output changes, add or keep a
`[[shot]]` in `screenshots/shots.toml` and run:

```sh
pip install pyte                        # once
guide/screenshots/capture.py            # rebuilds secbox, captures every shot
guide/screenshots/capture.py doctor     # or just one
```

It also rewrites `src/reference/screenshots.md`, the gallery page, from
`shots.toml`. It needs Node with Playwright and Chromium to save the PNGs. It
uses a fresh state folder at `/tmp/secbox-demo` and a real daemon with no VM
engine, so commands that need a running sandbox cannot be captured yet.

## Made for people and for AI

Every page is also served as Markdown at the same address with `.md` in place
of `.html`. The site root has `llms.txt` (an index in the
[llms.txt](https://llmstxt.org) format) and `llms-full.txt` (every page in one
file). To keep these useful:

- **Start every page with an `# H1`, then one plain paragraph** that says what
  the page covers. `llms.txt` uses that paragraph as the page's summary.
- **Use plain Markdown only.** No mdBook `{{#include}}` or other preprocessor
  syntax, because the raw `.md` files are served as they are.
- **Use relative links to `.md` files**, such as `../guides/secrets.md`. They
  work on GitHub, on the site, and are made absolute in `llms-full.txt`.
- **Prefer tables and code blocks** over long prose for options and output.

## Writing style

- Say plainly what is **not built** or **not tested**. secbox is a security
  tool, and an overstated claim is a bug.
- Check every command and flag against `secbox COMMAND --help`. Paste real
  output where you can.
- Short sentences. Second person ("you"). No marketing words.
- When the code changes, update the page in the same pull request. The status
  table in `src/start/status.md` mirrors the one in the root `README.md`.
