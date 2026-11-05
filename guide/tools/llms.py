#!/usr/bin/env python3
"""Make the built docs easy for language models to read.

Run after `mdbook build`. Reads the page order from src/SUMMARY.md and writes,
into the build folder:

- every page's Markdown source, next to its HTML (`foo/bar.html` -> `foo/bar.md`)
- `llms.txt`, an index in the https://llmstxt.org format
- `llms-full.txt`, every page in reading order in one file, with links made
  absolute so it still makes sense when pasted somewhere else

Standard library only, so CI needs nothing but Python 3.
"""

from __future__ import annotations

import argparse
import posixpath
import re
import shutil
import sys
from pathlib import Path

LINK = re.compile(r"(!?\[[^\]]*\])\(([^)\s]+)\)")
SUMMARY_PART = re.compile(r"^#\s+(.+?)\s*$")
SUMMARY_ITEM = re.compile(r"^\s*(?:-\s+)?\[([^\]]+)\]\(([^)]+)\)\s*$")


def parse_summary(src: Path) -> list[tuple[str, list[tuple[str, str]]]]:
    """Returns [(part title, [(page title, path)])] in reading order."""
    parts: list[tuple[str, list[tuple[str, str]]]] = [("Introduction", [])]
    for line in (src / "SUMMARY.md").read_text(encoding="utf-8").splitlines():
        part = SUMMARY_PART.match(line)
        if part and part.group(1) != "Summary":
            parts.append((part.group(1), []))
            continue
        item = SUMMARY_ITEM.match(line)
        if item:
            parts[-1][1].append((item.group(1), item.group(2)))
    return [p for p in parts if p[1]]


def description(text: str) -> str:
    """The first ordinary paragraph after the page title."""
    paragraph: list[str] = []
    seen_title = False
    in_code = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("```"):
            in_code = not in_code
            continue
        if in_code:
            continue
        if not seen_title:
            seen_title = stripped.startswith("# ")
            continue
        if not stripped:
            if paragraph:
                break
            continue
        if stripped[0] in "#>|!-*" or stripped.startswith("<"):
            if paragraph:
                break
            continue
        paragraph.append(stripped)
    plain = " ".join(paragraph)
    return LINK.sub(lambda m: m.group(1).lstrip("!")[1:-1], plain)


def absolute_links(text: str, page: str, base: str) -> str:
    """Rewrites relative links so they work outside the site."""

    def fix(match: re.Match[str]) -> str:
        label, target = match.group(1), match.group(2)
        if re.match(r"^[a-z][a-z0-9+.-]*:", target) or target.startswith("#"):
            return match.group(0)
        path, _, anchor = target.partition("#")
        joined = posixpath.normpath(posixpath.join(posixpath.dirname(page), path))
        url = f"{base}/{joined}" + (f"#{anchor}" if anchor else "")
        return f"{label}({url})"

    return LINK.sub(fix, text)


def book_field(root: Path, key: str, default: str) -> str:
    match = re.search(rf'^{key}\s*=\s*"([^"]*)"', (root / "book.toml").read_text(encoding="utf-8"), re.M)
    return match.group(1) if match else default


def main() -> int:
    here = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--src", type=Path, default=here / "src")
    parser.add_argument("--out", type=Path, default=here / "book")
    parser.add_argument("--base-url", default="https://ketanmujumdar.github.io/secbox")
    args = parser.parse_args()
    base = args.base_url.rstrip("/")

    if not (args.out / "index.html").is_file():
        print(f"{args.out} has no index.html: run `mdbook build` first", file=sys.stderr)
        return 1

    title = book_field(here, "title", "Documentation")
    summary = book_field(here, "description", "")
    parts = parse_summary(args.src)

    index = [
        f"# {title}",
        "",
        f"> {summary}",
        "",
        "secbox is an early build. Do not rely on it to contain untrusted code yet.",
        "Every page below is plain Markdown. The whole documentation is also in one file:",
        f"{base}/llms-full.txt",
        "",
    ]
    full = [
        f"# {title}",
        "",
        f"> {summary}",
        "",
        f"This file holds every page of {base}/ in reading order.",
        "Each page starts with a `Source:` line giving its address.",
        "",
    ]

    count = 0
    for part, pages in parts:
        index += [f"## {part}", ""]
        for page_title, path in pages:
            source = args.src / path
            text = source.read_text(encoding="utf-8")
            dest = args.out / path
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, dest)
            desc = description(text)
            index.append(f"- [{page_title}]({base}/{path})" + (f": {desc}" if desc else ""))
            full += ["", "---", "", f"Source: {base}/{path}", "", absolute_links(text, path, base).rstrip(), ""]
            count += 1
        index.append("")

    (args.out / "llms.txt").write_text("\n".join(index).rstrip() + "\n", encoding="utf-8")
    (args.out / "llms-full.txt").write_text("\n".join(full).rstrip() + "\n", encoding="utf-8")
    print(f"wrote llms.txt, llms-full.txt and {count} Markdown pages to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
