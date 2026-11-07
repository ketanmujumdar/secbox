# What works today

secbox is an early build. This page says exactly which parts work, which are built but unproven on a real VM, and which are still plans.

> **Do not rely on secbox to contain anything.** Do not run untrusted code on the assumption that it is contained.

## The short version

- A microVM has run a real Compose project with its network restricted, in one test on one Mac.
- The commands that only read files (`doctor`, `check`, `resources`, `net check`) work anywhere.
- Secrets are stored in the OS keychain and work.
- Starting, stopping and inspecting a sandbox (`up`, `down`, `status`, `logs`, `shell`, `exec`) are built. Most of that was tested against a fake engine. `logs`, `shell` and `exec` have run against a real sandbox.
- The only network rule on real traffic is the engine's own **hostname allowlist**. The fuller policy engine (HTTP methods, paths) explains decisions but enforces nothing yet.
- `logs` and `net blocked` replace secret values with their names.
- On macOS, starting a sandbox, `net allow` and every push wait for your Touch ID or password, asked by the daemon itself. Built and tested with a stand-in person, **never run on a real Mac**. Linux has no prompt. See [Confirming with Touch ID](../concepts/confirming.md).
- `diff` shows what changed in a sandbox and flags risky files, tested against a fake engine only. There is still **no way to bring changes back** to your machine: `export` is not built.

## Command by command

| Command | State |
| --- | --- |
| `doctor` | Works. Checks the host, without needing Docker |
| `check [PATH]` | Works. Reads a project's Compose file as data and reports what it asks for. For a project with only a Dockerfile, reads its `EXPOSE` lines |
| `open`, `checks` | Works. Editor safety checks, and the gate before launching an editor |
| `resources [PATH]` | Works. Shows requested and resolved limits, but nothing applies them yet |
| `secrets import/set/fill/list/rm` | Works, with the OS keychain. A template such as `.env.example` adds names only, to fill in later. Assigned secrets are delivered on each `up` by environment or file. The proxy route is refused |
| `net check POLICY METHOD URL` | Works. Explains a policy decision. Nothing enforces it yet |
| `net blocked` | Works, through the daemon, on a sandbox created recently enough. Hostnames only |
| `uninstall --purge` | Works. Removes the state folder only |
| `new REMOTE` or `new PATH` | Built, tested against a fake engine and a fake fetch only. **Never run on a real VM or server.** The same requests as `up`, with one review and one question that defaults to no |
| `up`, `down`, `status` | Built, for Compose projects and projects with only a Dockerfile. Need the daemon |
| `logs` | Works on a running sandbox, through the daemon. Removes terminal control codes, and replaces assigned secret values with their names |
| `shell`, `exec` | Work on a running sandbox, on your real terminal. Their output is not masked |
| `mcp serve` | Built, never run on a real VM |
| `mcp setup claude`, `mcp setup codex` | Works |
| `diff [PATH]` | Built, through the daemon, tested against a fake engine only. **Never run on a real VM.** Lists what changed in a running sandbox, with risky files flagged first. Copies nothing out |
| Touch ID before `up`, `net allow` and each push | macOS only. Built, tested with a stand-in person, **never shown on a real Mac**. No prompt on Linux |
| `export` | Not built. Exits with code 69 |
| Desktop app | Early first cut. Run on Linux only, under a virtual display. macOS untested. The Secrets tab, which sets values through the daemon, has been seen only in a browser preview |

## What kind of project can it start

- **A Compose project.** Its own Compose files are used as they are.
- **A project with a `Dockerfile` at its root and no Compose file.** secbox writes a one-service Compose file for it. See [Running a project](../guides/running-a-project.md#projects-with-only-a-dockerfile).

Dev containers and plain package manifests (`package.json` and the like) are recognized by `check` but refused by `up`.

## Platforms

| Host | Needs |
| --- | --- |
| macOS | Apple Silicon |
| Linux | KVM, meaning `/dev/kvm` exists. A cloud VM needs nested virtualization |

Windows is not supported.

## Every protection, with its proof

[`docs/PROTECTIONS.md`](https://github.com/ketanmujumdar/secbox/blob/main/docs/PROTECTIONS.md) in the repository lists each protection, the real attack it answers, the test that proves it, and whether it has run on a real VM. Quote its status along with any claim.

## Keeping up

The repository's `README.md` holds the same table and is updated with the code. `docs/NEEDS_A_VM.md` in the repository lists everything that still has to be confirmed on real hardware.
