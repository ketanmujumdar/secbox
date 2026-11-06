#!/usr/bin/env python3
"""Make the documentation's terminal screenshots from the real programs.

Each shot in shots.toml is run as `secbox ARGS` in a pseudo-terminal, so the
program sees a real terminal and uses its real colors and layout. The output
goes through a terminal emulator (pyte), is drawn as HTML, and is saved as a
PNG by the Chromium that Playwright drives. Nothing is drawn by hand.

    guide/screenshots/capture.py                  # every shot
    guide/screenshots/capture.py doctor up-plan   # just these
    guide/screenshots/capture.py --no-build       # use the binaries already built

Needs: cargo, Python 3.11+ with pyte (`pip install pyte`), and Node with
Playwright and Chromium (`npm i -g playwright && npx playwright install chromium`).

It uses a fresh state folder at /tmp/secbox-demo and never touches your own
~/.secbox or keychain. Commands that need the daemon get a real secboxd with no
engine, so plans and refusals are real but no VM is started.
"""

from __future__ import annotations

import argparse
import fcntl
import html
import json
import os
import pty
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
OUT = HERE.parent / "src" / "images" / "cli"
GALLERY = HERE.parent / "src" / "reference" / "screenshots.md"
DEMO = Path("/tmp/secbox-demo")

try:
    import pyte
except ImportError:
    sys.exit("capture.py needs pyte: pip install pyte")

# A dark terminal palette. Index order is the ANSI order.
PALETTE = {
    "black": "#1d1f21", "red": "#e06c75", "green": "#98c379", "brown": "#e5c07b",
    "blue": "#61afef", "magenta": "#c678dd", "cyan": "#56b6c2", "white": "#d7dae0",
    "brightblack": "#7f848e", "brightred": "#ff7b86", "brightgreen": "#b5e890",
    "brightbrown": "#ffd68a", "brightblue": "#7cc5ff", "brightmagenta": "#de9bf0",
    "brightcyan": "#7fd6e0", "brightwhite": "#ffffff",
}
FOREGROUND = "#d7dae0"
BACKGROUND = "#16181d"


def color(value: str, default: str) -> str:
    if value == "default":
        return default
    if value in PALETTE:
        return PALETTE[value]
    if len(value) == 6 and all(c in "0123456789abcdefABCDEF" for c in value):
        return "#" + value
    return default


def run_in_pty(argv: list[str], env: dict[str, str], cols: int, rows: int,
               keys: list[str], typed: str | None, interactive: bool) -> bytes:
    """Runs argv on a pseudo-terminal of the given size and returns its output."""
    pid, fd = pty.fork()
    if pid == 0:
        try:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
            os.chdir(REPO)
            os.execve(argv[0], argv, {**env, "COLUMNS": str(cols), "LINES": str(rows)})
        finally:
            os._exit(127)

    out = bytearray()

    def pump(idle: float, limit: float) -> bool:
        """Reads until nothing arrives for `idle` seconds. False on end of output."""
        start = last = time.monotonic()
        while time.monotonic() - start < limit:
            ready, _, _ = select.select([fd], [], [], 0.05)
            if ready:
                try:
                    chunk = os.read(fd, 65536)
                except OSError:
                    return False
                if not chunk:
                    return False
                out.extend(chunk)
                last = time.monotonic()
            elif time.monotonic() - last >= idle:
                return True
        return True

    if typed is not None:
        pump(0.5, 5)
        os.write(fd, typed.encode())
    if interactive:
        pump(1.5, 10)
        for key in keys:
            os.write(fd, key.encode())
            pump(1.0, 10)
        os.write(fd, b"q")
        pump(0.5, 3)
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    else:
        pump(30, 120)
    os.waitpid(pid, 0)
    os.close(fd)
    return bytes(out)


def screen_from(output: bytes, cols: int, rows: int, interactive: bool) -> list[list]:
    """Plays the output through a terminal emulator and returns its rows."""
    screen = pyte.Screen(cols, rows)
    stream = pyte.ByteStream(screen)
    if interactive:
        # Keep the last full frame the program drew: drop what it wrote on its
        # way out (leaving the alternate screen, restoring the cursor).
        marker = output.rfind(b"\x1b[?1049l")
        if marker != -1:
            output = output[:marker]
    stream.feed(output)
    lines = [[screen.buffer[y][x] for x in range(cols)] for y in range(rows)]
    if not interactive:
        while lines and all(c.data == " " for c in lines[-1]):
            lines.pop()
    return lines


def to_html(lines: list[list], title: str, cols: int) -> str:
    rows_html = []
    for line in lines:
        spans = []
        run, style = "", None
        for char in line:
            fg = color(char.fg, FOREGROUND)
            bg = color(char.bg, "transparent")
            if char.reverse:
                fg, bg = (bg if bg != "transparent" else BACKGROUND), fg
            css = f"color:{fg};background:{bg}"
            if char.bold:
                css += ";font-weight:700"
            if char.underscore:
                css += ";text-decoration:underline"
            if css != style:
                if run:
                    spans.append(f'<span style="{style}">{html.escape(run)}</span>')
                run, style = "", css
            run += char.data or " "
        if run:
            spans.append(f'<span style="{style}">{html.escape(run)}</span>')
        rows_html.append('<div class="row">' + "".join(spans) + "</div>")
    body = "".join(rows_html)
    return f"""<!doctype html><meta charset="utf-8">
<style>
  html, body {{ margin: 0; background: transparent; }}
  .window {{ display: inline-block; margin: 24px; border-radius: 10px; overflow: hidden;
    background: {BACKGROUND}; box-shadow: 0 10px 30px rgba(0,0,0,.35), 0 0 0 1px rgba(255,255,255,.06); }}
  .bar {{ height: 34px; display: flex; align-items: center; padding: 0 14px; gap: 8px;
    background: #23262d; color: #9aa0aa; font: 13px -apple-system, "Segoe UI", sans-serif; }}
  .dot {{ width: 12px; height: 12px; border-radius: 50%; }}
  .title {{ flex: 1; text-align: center; margin-right: 52px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }}
  pre {{ margin: 0; padding: 16px 18px 18px; width: {cols}ch; color: {FOREGROUND};
    font: 14px "JetBrains Mono", "DejaVu Sans Mono", Menlo, Consolas, monospace; }}
  .row {{ height: 1.4em; white-space: pre; }}
  .row span {{ display: inline-block; height: 100%; vertical-align: top; line-height: 1.4em; }}
</style>
<div class="window">
  <div class="bar"><span class="dot" style="background:#ff5f57"></span><span class="dot" style="background:#febc2e"></span><span class="dot" style="background:#28c840"></span><span class="title">{html.escape(title)}</span></div>
  <pre>{body}</pre>
</div>"""


def write_gallery(shots: list[dict]) -> None:
    groups: dict[str, list[dict]] = {}
    for shot in shots:
        groups.setdefault(shot.get("group", "Other"), []).append(shot)
    lines = [
        "# Screenshots",
        "",
        "Real output from the secbox programs, captured from a terminal by "
        "`guide/screenshots/capture.py`. Nothing here is drawn by hand.",
        "",
        "> These were captured on a Linux machine without KVM, with a real daemon "
        "but no VM engine. Commands that need a running sandbox (`up` without "
        "`--plan-only`, `logs`, `shell`, `exec`) are not shown yet.",
        "",
        "<!-- Generated by guide/screenshots/capture.py from shots.toml. Do not edit by hand. -->",
    ]
    for group, items in groups.items():
        lines += ["", f"## {group}"]
        for shot in items:
            command = " ".join(["secbox", *shot["args"]]).strip()
            lines += ["", f"### {shot['title']}", "", f"`{command}`", "",
                      f"![{shot['title']}](../images/cli/{shot['name']}.png)"]
    GALLERY.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("names", nargs="*", help="only these shots")
    parser.add_argument("--no-build", action="store_true", help="do not run cargo build first")
    args = parser.parse_args()

    shots = tomllib.loads((HERE / "shots.toml").read_text(encoding="utf-8"))["shot"]
    wanted = [s for s in shots if not args.names or s["name"] in args.names]
    unknown = set(args.names) - {s["name"] for s in shots}
    if unknown:
        sys.exit(f"no such shot: {', '.join(sorted(unknown))}")

    if not args.no_build:
        subprocess.run(["cargo", "build", "-q", "-p", "cli", "-p", "daemon"], cwd=REPO, check=True)
    bins = REPO / "target" / "debug"
    secbox, secboxd = str(bins / "secbox"), str(bins / "secboxd")

    shutil.rmtree(DEMO, ignore_errors=True)
    (DEMO / "home").mkdir(parents=True)
    env = {
        "PATH": "/usr/local/bin:/usr/bin:/bin",
        "HOME": str(DEMO / "home"),
        "SECBOX_HOME": str(DEMO),
        "TERM": "xterm-256color",
        "LANG": "C.UTF-8",
    }

    daemon = None
    if any(s.get("daemon") for s in wanted):
        # A real daemon with no engine: plans and refusals are real, no VM starts.
        daemon = subprocess.Popen([secboxd], env={**env, "SECBOX_ENGINE_BIN": "/bin/false"}, cwd=REPO,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        socket = DEMO / ".secbox" / "secboxd.sock"
        for _ in range(100):
            if socket.exists():
                break
            time.sleep(0.05)
        # Make the daemon know two projects, so the full-screen interface has
        # something to show.
        for fixture in ("tests/fixtures/clean", "tests/fixtures/dockerfile-only"):
            subprocess.run([secbox, "up", "--plan-only", fixture], env=env, cwd=REPO,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    OUT.mkdir(parents=True, exist_ok=True)
    pages = Path(tempfile.mkdtemp(prefix="secbox-shots-"))
    jobs = []
    try:
        for shot in wanted:
            interactive = "keys" in shot
            cols = shot.get("cols", 100 if interactive else 130)
            rows = shot.get("rows", 30 if interactive else 200)
            output = run_in_pty([secbox, *shot["args"]], env, cols, rows,
                                shot.get("keys", []), shot.get("input"), interactive)
            lines = screen_from(output, cols, rows, interactive)
            title = "$ " + " ".join(["secbox", *shot["args"]]).strip()
            page = pages / f"{shot['name']}.html"
            page.write_text(to_html(lines, title, cols), encoding="utf-8")
            jobs.append({"page": page.as_uri(), "png": str(OUT / f"{shot['name']}.png")})
            print(f"captured {shot['name']}")
    finally:
        if daemon:
            daemon.terminate()
            daemon.wait()

    script = pages / "shoot.js"
    script.write_text("""
const { chromium } = require('playwright');
(async () => {
  const jobs = JSON.parse(process.argv[2]);
  const browser = await chromium.launch();
  const page = await browser.newPage({ deviceScaleFactor: 2, viewport: { width: 1600, height: 900 } });
  for (const job of jobs) {
    await page.goto(job.page);
    await page.evaluate(() => document.fonts.ready);
    await page.locator('.window').screenshot({ path: job.png, omitBackground: true });
  }
  await browser.close();
})().catch(e => { console.error(e); process.exit(1); });
""", encoding="utf-8")
    node_path = subprocess.run(["npm", "root", "-g"], capture_output=True, text=True).stdout.strip()
    subprocess.run(["node", str(script), json.dumps(jobs)], check=True,
                   env={**os.environ, "NODE_PATH": node_path})
    shutil.rmtree(pages, ignore_errors=True)

    write_gallery(shots)
    print(f"wrote {len(jobs)} screenshots to {OUT.relative_to(REPO)} and {GALLERY.relative_to(REPO)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
