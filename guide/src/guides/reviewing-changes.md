# Reviewing changes

`secbox diff` shows what changed inside a running sandbox since it started, and flags first the files that could act on your machine, such as a planted hook for an AI coding tool. It copies nothing out.

> **Built, and has run on a real VM.** `export`, which would bring reviewed changes back to your machine, is not built. Nothing reviewed here can leave the sandbox yet.

```sh
secbox diff                     # the current folder's project
secbox diff ~/code/myapp
secbox diff --patch             # also print the changed lines of each file
```

## Sample output

```text
Compared with the snapshot this sandbox started from (commit 1f2a12002068).

FLAG  .claude/settings.json  adds a hook for an AI coding tool, which would run on your machine
FLAG  .claude/settings.json  changes Claude Code settings, which can make it run commands on your machine

2 files changed:
  added     .claude/settings.json
  modified  api/server.js

2 flags to look at. Flags are a heuristic aid. Read the changes yourself before anything from this sandbox reaches your machine.
Nothing was copied out of the sandbox. Export is not built yet.
```

Flags come first, in red on a terminal. Then every changed file, marked `added`, `modified`, `deleted` or `type changed`.

## Reading the changed lines

`secbox diff --patch` prints the lines that changed in each file, and in the desktop app you click a file in the **Changes** tab. Git makes the comparison on the host, with `git diff-tree` in the reviewed copy. That copy has no working folder, so a `.gitattributes` from the sandbox cannot change what is shown, and nothing from the sandbox is run.

Some files are not shown, and each one says why:

| Not shown | Why |
| --- | --- |
| Looks like it holds a secret | Either version of the file has a key-shaped string or a long random value under a name like `password`, holds a value of a secret given to this project, or has a name like `.env` or `*.pem` |
| Binary | It holds a NUL byte |
| Too large | Either version is over 1 MB |
| Submodule | It points at another repository |
| No room | One reply carries about 4 MB. Files with a flag get that room first, so a flood of harmless text cannot push a planted hook out of view |

A long file is cut after 5,000 lines or 256 KB, and a long line after 2,000 characters, and each cut says so.

**A file that is not shown has not been reviewed.** A sandbox can cause any of these on purpose, for example by planting a fake key in a malicious script so that it is withheld. Treat such a file as unread, and don't take it out of the sandbox on trust.

A secret that matches no pattern and was not given to this project will appear in the lines. They go only to your own screen, but they will be in your terminal's scrollback if you use `--patch`.

Control, bidirectional and invisible characters show as escapes such as `\u{202e}`, so text cannot pretend to be something else that way. Lookalike letters from other alphabets are not escaped in file content.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Nothing was flagged |
| 5 | Something was flagged, or the review stopped at a limit |
| 1 | It could not compare, for example because the sandbox is not running |

So a script can stop on anything worth a look:

```sh
secbox diff || echo "read the flags before going further"
```

## What gets flagged

| Kind of file | Examples |
| --- | --- |
| AI coding tool settings and hooks | `.claude/`, `.cursor/`, `CLAUDE.md`, `AGENTS.md` |
| Editor settings | `.vscode/` |
| CI workflows | `.github/workflows/` |
| Git hooks | `.husky/` and similar |
| Install scripts and lockfiles | `package.json` scripts, `package-lock.json` |
| Build files | `Dockerfile`, Compose files |
| Odd files | Symlinks, binaries, likely secrets |

Names are matched the way a case-insensitive filesystem would, so `.CLAUDE/Settings.json` is flagged too, as are lookalikes that hide invisible characters in a name.

Flags are a heuristic aid. A harmful change to an ordinary source file is not flagged. Read the changes yourself.

## How it works

1. **A baseline the sandbox cannot change.** When a sandbox is created, the daemon commits the snapshot itself and keeps that commit in the project's state folder, as `baseline.git`. The sandbox gets the same commit as `/work/.git`. Whatever the sandbox does to its own history, `diff` compares with the host's copy.
2. **Git runs only inside the sandbox.** On `diff`, Git in the sandbox commits everything in `/work` onto a throwaway branch, uncommitted changes included, and sends a bundle of it back as text. The daemon stops reading at 128 MB.
3. **The bundle is treated as hostile.** The daemon checks it, imports it into a new quarantine repository and reviews it there. Nothing from it is checked out or run, and Git on your machine never runs in a `.git` folder the sandbox wrote. A malformed, oversized or tricked bundle is refused.

If the sandbox rewrites its history so it no longer starts from the snapshot, `diff` says so and exits 5.

## In the full-screen interface

Press `c` for the selected project's Changes tab. It compares by itself when the sandbox is running, and again after a Git command that can change files. `r` compares whenever you like. Flagged files come first. Pick a file to read its changed lines, beside the list on a wide terminal. See [Full-screen terminal interface](terminal-ui.md).

![The Changes tab of the full-screen interface, before comparing](../images/cli/tui-review.png)

## In the desktop app

The **Changes** tab compares by itself when you open it with the sandbox running, and again after a commit, pull or other Git command. **Compare again** runs it whenever you like. Each comparison runs Git inside the sandbox and is noted in the audit log. Changed files are listed on the left, and the file you pick shows on the right, **Side by side** or **Inline**. See [Desktop app](desktop-app.md).

## Limits

- **The sandbox must be running.** `diff` never starts one.
- **Ignored files are not shown.** Files the project's `.gitignore` leaves out are not in the comparison, and would not be in an export either.
- **Older sandboxes have no baseline.** A sandbox created before `diff` existed has nothing to compare with, and `diff` says to recreate it with `secbox down --destroy` and `secbox up`. That deletes its data.
- **The VM image needs Git.** This has not been checked on a real VM.
- **No export yet.** Changes stay in the sandbox, and `down --destroy` deletes them.
