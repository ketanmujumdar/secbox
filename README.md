# secbox

secbox is being built to run a project and its AI agent inside a disposable microVM, so that a compromised dependency or a tricked agent stays away from your machine. `docs/ARCHITECTURE.md` has the design.

`secbox` is a working name. See [Changing the name](#changing-the-name).

## Status: early, and not protecting anything yet

**Do not rely on this tool to contain anything.** A microVM has been shown to
run a Compose project with its network constrained, in one spike on one Mac
(`docs/spikes/0001-engine-compose.md`). Most commands below were built against
a fake engine, and only some have run on a real VM. On real traffic, the only
network rule is the engine's own hostname allowlist: the rule engine in
`crates/egress` is not connected to anything. Secrets assigned to a service are
delivered into its sandbox on each start, by environment or file. That route
exists in the code and has not been confirmed on a real guest.

`docs/NEEDS_A_VM.md` lists everything that has to run on real hardware before
any of it can be called working.

`docs/PROTECTIONS.md` lists each protection against the real attack it answers, the test that proves it, and whether it has run on a real VM.

| Command | State |
| --- | --- |
| `doctor` | Works. Checks the host, without needing Docker |
| `check [path]` | Works. Reads a project's Compose file as data and reports what it asks for. For a project with only a Dockerfile, reads its `EXPOSE` lines |
| `open`, `checks` | Works. Editor safety checks, and the gate before launching an editor |
| `resources [path]` | Works. Shows requested and resolved limits, but nothing applies them yet |
| `secrets import/set/list/rm` | Works, with the OS keychain. Assigned secrets are delivered on each `up`, but not by the proxy route, which is refused |
| `net check <policy> <METHOD> <url>` | Works. Explains a policy decision, but nothing enforces it yet |
| `net blocked [--project PATH] [-f]` | Works, through the daemon, on a sandbox created since this was added. Blocked hostnames only. See [Blocked hostnames](#blocked-hostnames) |
| `uninstall --purge` | Works. Removes the state folder only |
| `up`, `down`, `status` | Built, for Compose projects and projects with only a Dockerfile. They need the daemon |
| `logs [SERVICE] [-f] [--tail N] [--project PATH]` | Works on a running sandbox, through the daemon. Removes terminal control codes, and replaces assigned secret values with their names |
| `shell [SERVICE]`, `exec [--service S] -- CMD...` | Work on a running sandbox, on your real terminal. Control codes can't be filtered there |
| `diff [PATH]` | Built, through the daemon, and tested against a fake engine only: **never run on a real VM**. Lists what changed in a running sandbox since its snapshot, with files that could act on your machine flagged first. Copies nothing out. See [Reviewing changes](#reviewing-changes-in-a-sandbox) |
| `export` | Not built |
| Desktop app (`apps/desktop`) | Early first cut. A menu bar icon that keeps the daemon running, and a window for status, start with plan approval, stop, destroy, logs and blocked hostnames. Same daemon, same powers as the CLI. Run on Linux under Xvfb only; macOS untested. See [its README](apps/desktop/README.md) |

`up`, `down`, `status`, `logs`, `net blocked`, `shell`, `exec` and `diff` send requests to a daemon,
`secboxd`, which holds the secrets, the policy and the approvals. With no daemon
running they say so.

## What can be started

Two kinds of project, and nothing else yet:

- **A Compose project.** Its own Compose files are used as they are.
- **A project with a `Dockerfile` at its root and no Compose file.** The daemon writes a one-service Compose file for it: a service named `app`, built from the project with that Dockerfile, with no volumes and no extra privileges. Ports come from the Dockerfile's `EXPOSE` lines in its last build stage, TCP only; variables, ranges, UDP and anything odd are listed in the plan and left out. Like any port, each is forwarded to the same number on `127.0.0.1` only if you approve it. The generated file is shown in the plan and covered by its approval. It lands at `/work/compose.yaml` in the sandbox, so a project that brings its own Compose file uses that instead.

Dev containers and package manifests are recognized by `check` but refused by `up`. This has not yet run on a real VM: see `docs/NEEDS_A_VM.md`.

## Looking inside a sandbox

```sh
secbox logs                     # the last 200 lines from each service
secbox logs api -f              # follow one service; Ctrl-C stops following only
secbox shell                    # a shell in the VM, in /work
secbox shell postgres           # docker compose exec postgres sh
secbox exec -- ls -la           # one command, with its exit code passed back
secbox exec --service api -- env
```

Add `--project PATH` to name a project from another folder. None of these starts a stopped sandbox: they refuse and tell you how to start it.

In the full-screen interface, the Logs panel follows new lines by itself. Scrolling up pauses it, and `f` or End follows again. Press `e` on Projects or Logs to open the same shell as `secbox shell`, in the service chosen on Logs if there is one. The interface steps aside while the shell runs and comes back when you leave it. The bar along the bottom lists only the keys that work on the panel showing, and `1` or `?` explains them. Starting, stopping and destroying (`2`, `3`, `x`) work only on Projects. The red tag at the right of the menu bar, saying this is an early build, never goes away. The Secrets panel (`s`) lists the selected project's secret names, how each was stored and which services it goes to, read from the same metadata as `secbox secrets list`. It never shows a value or opens the keychain, and it changes nothing: use the `secrets` commands for that.

There is no SSH server and no open port in the VM. For `shell` and `exec` the daemon checks the sandbox is running and tells the CLI where the engine is, and the CLI runs the engine with a cleared environment. Anything you paste into that shell is inside the sandbox.

`logs` and `net blocked` replace each assigned secret's value with its name, such as `[DEMO_TOKEN]`, before you see it. That covers the value as it is, in base64, URL encoding and hex, and pieces of it inside a hostname. A value that has been encrypted or reworked can't be recognised, so this stops accidental leaks into logs, not a thief. `shell` and `exec` run on your own terminal, so their output is not masked.

## Reviewing changes in a sandbox

```sh
secbox diff                     # the current folder's project
secbox diff path/to/project
```

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

It exits 0 when nothing is flagged, 5 when something is, or when the review stopped at a limit, and 1 when it could not compare. Flags cover AI tool settings and hooks (`.claude/`, `.cursor/`), `CLAUDE.md` and `AGENTS.md`, `.vscode/`, CI workflows, Git hooks such as `.husky`, package install scripts, lockfiles, Dockerfiles and Compose files, symlinks, binaries and likely secrets. Names are matched the way a case-insensitive filesystem would, so `.CLAUDE/Settings.json` and lookalikes with invisible characters flag too.

How it works:

- When a sandbox is created, the daemon commits the snapshot itself, keeps that commit in the project's state folder, and gives the sandbox the same commit as `/work/.git`. That commit is what `diff` compares with, whatever the sandbox does to its own history.
- On `diff`, Git runs inside the sandbox only. It commits everything in `/work` onto a throwaway branch, uncommitted changes included, and sends a bundle of it back as text. The daemon stops reading at 128 MB.
- The daemon checks the bundle, imports it into a new quarantine repository and reviews it there. Nothing from it is checked out or run, and Git on your machine never runs in a `.git` the sandbox wrote.

Limits: files the project's `.gitignore` leaves out are not shown, and would not be in an export either. A sandbox created before this was added has nothing to compare with, and `diff` says to recreate it. The sandbox must be running. `export` is not built, so nothing reviewed here can leave the sandbox yet.

The Review panel in the full-screen interface (`r`) shows the same lines, flags first in red. Press `5` to compare: it runs inside the sandbox, so it never runs by itself. Up, down, PgUp and PgDn scroll. The desktop app's Git review page shows the same, with a Compare button.

## Blocked hostnames

```sh
secbox net blocked              # time, count and hostname, newest last
secbox net blocked -f           # keep checking; Ctrl-C stops
secbox status                   # includes a count
```

The Traffic panel in the full-screen interface shows the same list. Press `t` to open it, `5` to read again, and up, down, PgUp and PgDn to scroll. The Projects panel shows the count in a Blocked box beside the selected project's services, ports and resources, in red when anything was refused.

Read this before relying on the list:

- **It shows blocked hostnames only.** Raw IP, metadata and UDP attempts are blocked but not recorded, so an empty list doesn't mean nothing was tried.
- **Only a sandbox created since this was added records anything.** A sandbox's log level can't be changed later. An older one says nothing is being recorded. To record, recreate it with `secbox down --destroy`, then `secbox up`, which deletes its data.
- **Events can be lost.** The engine keeps about 50 MB of this log. On an idle sandbox that is about an hour at best. A busy or hostile sandbox can push events out within seconds, and the output says so when older events are cut off. `-f` sees events as they arrive, so it loses far fewer.
- **The count is of refused queries.** One lookup is usually two, for IPv4 and IPv6. The guest chooses each name, so a name shows what was asked for and nothing more.

The record comes from the engine's log on the host, read by the daemon. As measured on engine 0.7.13, the guest could not write to the folder holding it. If a check on that folder fails, the output says the record is not trustworthy.

## Quick start

You need a Rust toolchain, and macOS on Apple Silicon or Linux. There is no Homebrew tap or release yet, so [install from source](docs/INSTALL.md):

```sh
cargo install --path crates/cli --locked
secbox doctor
secbox check tests/fixtures/risky
secbox net check tests/fixtures/policy.yaml PUT https://registry.npmjs.org/left-pad
```

`docs/MANUAL_TEST.md` is a step-by-step script for trying everything that is built.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | A check failed, or something was refused or aborted |
| 2 | Bad usage |
| 3 | `check --strict` found Critical or High findings |
| 4 | `net check` explained a denied request |
| 5 | `diff` flagged something, or its review stopped at a limit |
| 69 | The command is not built yet |

## Files it keeps

State lives in `~/.secbox`, owner-only: per-project secret names, their assignments, a names-only audit log, and for each sandbox a small Git repository holding the commit of the snapshot it was given (`baseline.git`, removed by `down --destroy`). Secret values go only to macOS Keychain or Linux Secret Service. Set `SECBOX_HOME` to use `$SECBOX_HOME/.secbox` instead, which helps when testing or when your home path is too long for Unix sockets.

## Changing the name

The name is defined once, in `crates/brand/src/lib.rs`. Rust code outside that crate must use its constants, and a test in that crate fails if it does not.

```sh
scripts/rename.sh newname   # lowercase letters and digits, starting with a letter
cargo test --workspace
```

The script rewrites project text files and renames files that carry the name, such as the Homebrew formula. Some things it does not do:

- It does not rename the project folder.
- It does not change images in `docs/images`, which show the old name.
- It rewrites the desktop app's `tauri.conf.json`, but no other JSON, HTML or JavaScript. The app's page asks the app for the name at run time, and the app takes it from the brand crate, so the page has none to change.
- It leaves lines about the unrelated openSUSE package alone. Review the "Final name" row in `docs/ARCHITECTURE.md` by hand.
- It does not move existing data. The old `~/.secbox` and old keychain entries stay under the old name, so remove them first with the old binary.

Avoid a new name that is a common word or a part of other identifiers. The name is replaced as plain text wherever it appears, and the guard test matches any text containing it.
