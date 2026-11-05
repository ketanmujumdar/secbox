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

| Command | State |
| --- | --- |
| `doctor` | Works. Checks the host, without needing Docker |
| `check [path]` | Works. Reads a project's Compose file as data and reports what it asks for |
| `open`, `checks` | Works. Editor safety checks, and the gate before launching an editor |
| `resources [path]` | Works. Shows requested and resolved limits, but nothing applies them yet |
| `secrets import/set/list/rm` | Works, with the OS keychain. Assigned secrets are delivered on each `up`, but not by the proxy route, which is refused |
| `net check <policy> <METHOD> <url>` | Works. Explains a policy decision, but nothing enforces it yet |
| `net blocked [--project PATH] [-f]` | Works, through the daemon, on a sandbox created since this was added. Blocked hostnames only. See [Blocked hostnames](#blocked-hostnames) |
| `uninstall --purge` | Works. Removes the state folder only |
| `up`, `down`, `status` | Built. They need the daemon |
| `logs [SERVICE] [-f] [--tail N] [--project PATH]` | Works on a running sandbox, through the daemon. Removes terminal control codes. Does not mask secret values |
| `shell [SERVICE]`, `exec [--service S] -- CMD...` | Work on a running sandbox, on your real terminal. Control codes can't be filtered there |
| `diff`, `export` | Not built |

`up`, `down`, `status`, `logs`, `net blocked`, `shell` and `exec` send requests to a daemon,
`secboxd`, which holds the secrets, the policy and the approvals. With no daemon
running they say so.

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

In the full-screen interface, the Logs panel follows new lines by itself. Scrolling up pauses it, and `f` or End follows again. Press `e` on Projects or Logs to open the same shell as `secbox shell`, in the service chosen on Logs if there is one. The interface steps aside while the shell runs and comes back when you leave it. The bar along the bottom lists only the keys that work on the panel showing, and `1` or `?` explains them. Starting, stopping and destroying (`2`, `3`, `x`) work only on Projects.

There is no SSH server and no open port in the VM. For `shell` and `exec` the daemon checks the sandbox is running and tells the CLI where the engine is, and the CLI runs the engine with a cleared environment. Anything you paste into that shell is inside the sandbox.

`logs` masks nothing. A service given a secret can print it, and `logs` will then show it.

## Blocked hostnames

```sh
secbox net blocked              # time, count and hostname, newest last
secbox net blocked -f           # keep checking; Ctrl-C stops
secbox status                   # includes a count
```

The Traffic panel in the full-screen interface shows the same list. Press `t` to open it, `5` to read again, and up, down, PgUp and PgDn to scroll.

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
| 69 | The command is not built yet |

## Files it keeps

State lives in `~/.secbox`, owner-only: per-project secret names, their assignments and a names-only audit log. Secret values go only to macOS Keychain or Linux Secret Service. Set `SECBOX_HOME` to use `$SECBOX_HOME/.secbox` instead, which helps when testing or when your home path is too long for Unix sockets.

## Changing the name

The name is defined once, in `crates/brand/src/lib.rs`. Rust code outside that crate must use its constants, and a test in that crate fails if it does not.

```sh
scripts/rename.sh newname   # lowercase letters and digits, starting with a letter
cargo test --workspace
```

The script rewrites project text files and renames files that carry the name, such as the Homebrew formula. Some things it does not do:

- It does not rename the project folder.
- It does not change images in `docs/images`, which show the old name.
- It leaves lines about the unrelated openSUSE package alone. Review the "Final name" row in `docs/ARCHITECTURE.md` by hand.
- It does not move existing data. The old `~/.secbox` and old keychain entries stay under the old name, so remove them first with the old binary.

Avoid a new name that is a common word or a part of other identifiers. The name is replaced as plain text wherever it appears, and the guard test matches any text containing it.
