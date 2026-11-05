# Build plan

What remains to build, in order. Read `docs/ARCHITECTURE.md` for the design and `docs/spikes/0001-engine-compose.md` for measured engine behavior.

secbox is not protecting anything yet. Do not describe a feature as working until it has run against a real VM.

## Rules for every change

- Treat everything in the VM, and every repo file and Compose file, as hostile.
- Never write the product name in Rust code outside `crates/brand`. A test enforces this, including comments.
- No `unsafe`. Keep dependencies few and commit `Cargo.lock`.
- Never pass secret values as command-line arguments, print them, or put them in errors or logs.
- Host code must never follow paths under the engine's guest state or its `.bootstrap-rootfs`, because the guest can write to both and a guest-made symlink becomes a real host symlink.
- Host code must never run Git in a `.git` written by the VM.
- Automated tests never touch the real keychain.
- Never use the engine's `allow@dns` option, because it opens a channel for sending data out.
- A probe that couldn't run reports SKIPPED, never BLOCKED.

## Where each part can be built

Most cloud environments are Linux and may have no KVM, so they can't run the VM tests.

| Work | Cloud | Needs a Mac or a KVM machine |
| --- | --- | --- |
| Daemon, CLI wiring and flows against `FakeEngine` | Yes | |
| `check`, `open`, snapshot rules, CI | Yes | |
| Milestone 0, and anything that boots a real VM | | Yes |
| Leak suite runs | | Yes |
| Touch ID and real macOS Keychain | | Mac only |

`.tools/` and `.spike/` are git-ignored, so a new machine has no engine or `demo_shop` copy. The `demo_shop` tests skip when its file is missing, and a skipped test proves nothing.

## Current state

| Crate | State | Main gaps |
| --- | --- | --- |
| `brand` | Done | |
| `projectcheck` | Done | `include` and `extends` files aren't followed; override files aren't in the model |
| `secrets` | Done | Real keychain never run |
| `egress` | Rule engine done | No proxy; no CONNECT pre-check; method-override headers and keys in query strings not handled. **Nothing calls it on real traffic** |
| `gitbroker` | Done | No real HTTPS push; no SSH remotes; no Touch ID approver; no working-tree snapshot |
| `engine` | Done on macOS | Secrets need TLS interception; Linux untested |
| `proto` | Done | The API covers `plan`, `up`, `down`, `status`, `logs`, blocked hostnames and terminal access only |
| `hoststate` | Done | Project identity still hashes the full path, so moving a project loses its secrets |
| `daemon` | Core done, against `FakeEngine` | Only partly run on a real VM; blocked hostnames come from the engine's log, with no record of raw IP, metadata or UDP attempts and no gateway log; the proxy secret route waits on milestone 0 |
| `cli` | `up`, `down`, `status`, `open`, `checks`, `logs`, `shell`, `exec` and `net blocked` done | `diff` and `export` refuse; no `sync`, `net log`, `net allow` or `rebuild` |

`docs/NEEDS_A_VM.md` is the standing list of what has not run on real hardware.

## Milestone 0: decide the TLS question (run first, on a Mac)

The allowlist works by hostname without TLS interception. Proxy-held API keys and method rules, such as read-only npm, need interception, and nested containers don't trust its CA, so a real build fails with `certificate verify failed`.

Test a full `demo_shop` `docker compose up --build` with the CA available to build steps and running containers, without editing the project's Dockerfiles. Also test whether a sandbox's network policy can change between build and run.

Record the result in the spike doc. Then pick one:

- **It works:** continue with this engine and full method rules.
- **It doesn't work:** test Docker Sandboxes next, or ship v1 with hostname-only rules and injected secrets. If v1 ships that way, say plainly that `npm publish` is not blocked on an allowed registry host.

Done when: the choice is recorded in `docs/ARCHITECTURE.md`.

## Milestone 1: run a project in a VM

- Daemon: owner-only Unix socket, peer UID check, one narrow JSON API shared by all clients, and an audit log with names only.
- `secbox up [path]`, `down` and `status`, built on the engine crate. Follow the working command sequence in the spike doc, and start `dockerd` by hand, clearing a stale `/var/run/docker.pid`.
- Snapshot from a Git commit with `.env` left out. Widen the exclusions to common secret file names, warn on likely secrets in other files, and require an explicit flag to include uncommitted changes.
- Compose first. Other project kinds wait until after v1.
- Forward only approved ports, on `127.0.0.1`, keeping the same port numbers.
- Apply resources from `projectcheck::resources`, with a user-level maximum. Memory minimum is 3G for `demo_shop`, since 2G failed every time. Budget the full VM size on the host.
- Network: deny by default, using hostname allowlists. Provide a build profile; the `demo_shop` build needed 12 allowed hosts, listed in the spike doc.
- Secret delivery by the routes in the architecture doc, to named services only. Secrets must be per project, and project identity must survive moving the folder (it hashes the full path today).

Done when: `secbox up` runs `demo_shop` in a VM with no real secrets, on a Mac, with all services healthy, and the app opens at `127.0.0.1:3000`.

## Milestone 2: daily use

- `secbox logs`, `shell` and `exec` are built, and ran against a real VM. `logs` strips control codes. **It does not mask secret values yet.** Assigned secrets now reach a sandbox, so a service that prints one shows it in `logs`. Masking still has to be added.
- `secbox sync`.
- `secbox net blocked` is built, and ran against a real VM. It reads refused name lookups from the engine's log on the host, for sandboxes created at `--log-level debug`. Raw IP, metadata and UDP attempts are not in that log at any level, so a complete record still needs a gateway on the host.
- `net log` and `net allow`, with every decision recorded on the host, and a system approval to loosen policy. Blocked HTTP requests get a clear error.
- `secbox rebuild`, with the review steps, rollback and the five actions in the architecture doc.
- `secbox uninstall --purge` must keep refusing while a sandbox is running.

Done when: a developer can run, debug and rebuild `demo_shop` using only secbox commands.

## Milestone 3: getting code out

- `secbox diff` and `export`, connecting `gitbroker` to the CLI.
- A real Touch ID approver, and a passphrase approver for headless Linux. Approval must prove a person acted.
- Settle Git history: the snapshot has no `.git`, so VM commits must share ancestry with the remote, likely through `import_base` and prerequisite bundles.
- Real HTTPS push with credentials, tested against a real server. Then decide on SSH remotes.
- Validate bundles in a throwaway VM before the host's Git parses them.

Done when: a change made in the VM reaches a `secbox/<task>` branch on a real remote only after Touch ID, and a planted `.vscode/tasks.json` is flagged in `diff`.

## Milestone 4: VS Code safety

- Add settings checks to `secbox check`: `task.allowAutomaticTasks` and Workspace Trust, for VS Code and Cursor. Confirm from official docs whether a project's settings can override them.
- Project checks: `runOn: folderOpen` tasks, workspace settings changing these values, and hooks in `.claude/settings.json`.
- `secbox open`: runs the check, then launches the editor only if it passes. Overrides need a flag and are logged.
- `secbox check --fix`: changes user settings with consent and a backup, never project files.
- `secbox checks list`, `on` and `off`, stored at user level so a repo can't change them.

Do not build an editor, an editor plug-in, or VS Code Remote-SSH support.

Done when: `secbox open` refuses a repo with a planted folder-open task, and a hostile `secbox.yaml` can't disable a check.

## Milestone 5: leak suite (grow it alongside milestones 1 to 3)

Extend `tests/leak/`, which has probes and a host listener. Each probe must pass a positive control first. The suite must fail on any canary reaching a listener outside the VM, or appearing in logs, errors or Git exports.

- **Canaries:** fake secrets on the host, in injected variables and in proxy bindings.
- **Routes:** direct, nested containers, Compose networks, `docker build` with BuildKit and legacy, DNS, UDP, raw IPs, private ranges, metadata, and host services.
- **Purchases and writes:** calls to payment APIs and `npm publish`.
- **Host access:** host files, sockets, keychain, guest-state symlinks and malicious hooks in bundles.
- **Not yet covered:** IPv6, QUIC, DNS tunneling volume and domain fronting. Add these.

Done when: the suite runs with one command on a Mac, and a deliberately broken policy makes it fail.

## First release gate

Milestones 0 to 5 are complete, the leak suite passes on a real VM, and an independent security review of the whole codebase has been done, with its findings fixed.

## After the first release

- AI: `secbox ai login`, Claude Code and Codex in the VM, the MCP server and its tools, external MCP controls, and optional AI assessment.
- Linux support and testing, including encrypted Secret Service use.
- CI with `cargo-deny` and `cargo-vet`.
- Public repo, signed releases, automatic engine download with checksum check, the Homebrew tap, and the final name.
- Projects without Compose or a Dockerfile.
- V2 interface: retro terminal UI or the Tauri app. Evaluate the terminal UI first, since it needs no web view.
  - A first cut of the Tauri app is in `apps/desktop`, as its own Cargo workspace so this one still builds without WebKit: a menu bar icon that starts the daemon and stays running, and a window with the overview, start with plan approval, stop, destroy, logs and blocked hostnames. It has the same powers as the CLI and ships the CLI and daemon inside its bundle. It has run on Linux under Xvfb only; macOS bundling and the menu bar icon are untested. See `apps/desktop/README.md`.
