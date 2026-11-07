# What still needs a real VM

As of Nov 5, 2026. Everything below was built and tested in a Linux cloud
container with **no KVM**, so no sandbox was ever booted. A passing test here
shows the logic holds against `FakeEngine`. It is not evidence that anything is
contained.

`docs/spikes/0001-engine-compose.md` is the only place in this repository where
a real VM ran. Those results came from driving the engine's command line by
hand on a Mac. Nothing in `crates/daemon` or `crates/cli` was part of that run.

## The two gaps in this machine

| Gap | Effect |
| --- | --- |
| No `/dev/kvm`, no engine binary | Nothing can boot. Every VM path is unexercised |
| git 2.43, and `gitbroker` requires 2.50 | Worked around on Nov 5, 2026 by building git 2.56.0 from the kernel.org tarball into `/usr/local` and linking `/usr/bin/git` to it. With that, every `gitbroker` test ran and passed, and so did the `diff` tests in `crates/daemon/tests/diff.rs`. A fresh container is back to 2.43. The SSH fetch test added later in `crates/gitbroker/src/remote.rs` has not run on that build; it passed once on 2.43 with the version check lowered for the run |

## Logs and terminals: what ran on a real VM

On Nov 5, 2026, on a Mac, against a running `demo_shop` sandbox, using a second daemon with its own state folder. Its engine path was a test wrapper that pinned the engine's state folder, refused lifecycle commands and set `HISTFILE=/dev/null`.

- Ran: `logs` for all services and for one, `-f` with Ctrl-C on a pty, `shell`, `shell SERVICE`, `exec` on a terminal and on pipes, and the TUI Logs panel. After Ctrl-C the sandbox was still running, no `compose logs` process was left in the guest, and the terminal settings were unchanged.
- [ ] Refusing a sandbox that is really stopped. This ran only with a stub engine that reports `Stopped`, because the real sandbox was in use.
- [ ] Hostile output from inside a VM. Hostile bytes ran only from a stub engine through the real binaries and socket, and in tests.
- [ ] Commands typed into `shell SERVICE`. Only opening it and leaving with Ctrl-D ran, so as not to write a history file in the container.

## Git: import, pull and push through the relay

Built on Nov 5, 2026 (`docs/GIT.md`). Tested end to end with real Git on both sides, but with a local folder as the VM and a local repository as the server. **Never run in a VM, against a real server, or with a real agent.**

- [ ] **Git in the guest.** The relay runs `git`, `mkfifo`, `mktemp` and `cat` inside the VM. Check that `docker:dind` has Git. If it doesn't, the clone fails with "git is not installed in this sandbox", and the image needs it added.
- [ ] **Two-way streaming through `msb exec --stream --no-tty`,** with stdin and stdout both busy at once, for a whole clone. Exec on pipes ran on a real VM, but only one way at a time.
- [ ] **End of stream:** when Git in the VM closes its side, the host sees end of input, and the next step follows. In tests, a copy of the pipe held open made both sides wait for ever. The relay script now holds each end in one process only. Check that `msb` closes the guest's stdout when the guest closes it.
- [ ] `secbox up git@github.com:ORG/REPO.git` against a private repository: the clone has the full history at the approved commit, and `git push` typed in the VM fails with the message.
- [ ] `secbox git pull`, `commit` and `push` on a branch, the push landing on GitHub, and a push to `main` refused with nothing sent.
- [ ] 1Password: its prompt appears for the import fetch, the clone, each pull and each push; approving works; denying fails quickly with the hint. Once with the daemon started by the desktop app at login, relying on `IdentityAgent` in `~/.ssh/config`.
- [ ] A host missing from `known_hosts` failing with the hint, and nothing written to `known_hosts`.
- [ ] A large repository cloning inside the 15-minute limit.
- [ ] The MCP tools from Claude Code: `git_pull`, `git_commit` and `git_push` on a branch, and refused on `main`.
- [ ] The desktop app: Import, then the plan, then the Git tab's Status, Pull, Commit and Push.
- [ ] The desktop app's Secrets tab on a Mac: set an empty name's value with only `web` ticked, start the project, and check inside `api` that the variable is unset and `/run/secrets` holds none of it. macOS must not ask for keychain access when the daemon later reads a value it stored itself. Typing in the field must not offer to save a password.

## Confirming with Touch ID

Built on Nov 7, 2026 (`crates/personcheck`). Compiled and linted for `aarch64-apple-darwin` from Linux; every rule around it is tested with a stand-in person. **The prompt itself has never been shown on a Mac.**

- [ ] `secboxd` started from a terminal: `secbox up` shows macOS's prompt with the reason, Touch ID confirms it, and Cancel ends `up` with "not confirmed" and no sandbox.
- [ ] `secboxd` started by the desktop app at login (no terminal): the prompt still appears.
- [ ] A Mac with no Touch ID, or with the lid closed: the password prompt appears instead.
- [ ] Over SSH, with no screen: refused at once with "there is no screen to ask on", not left waiting.
- [ ] No answer for two minutes: the prompt goes away and the request is refused.
- [ ] Two requests at once (desktop Start and `secbox git push`): the prompts come one after the other, each with its own reason.
- [ ] A push from the `git_push` MCP tool: the prompt names the branch and commit, and Cancel sends GitHub nothing.

## AI agents through MCP

Built on Nov 5, 2026 (`docs/MCP.md`). Tested against the real daemon logic with `FakeEngine` and a stub engine binary. The real binary was run on pipes against a real daemon that had no engine, and Claude Code 2.1.318 reported the server as connected. **Nothing ran in a VM.**

- [ ] `exec`, `read_file` and `write_file` through a real `msb exec --stream --no-tty`, in the sandbox and in a service with `docker compose exec -T`.
- [ ] **Standard input reaching the command** through `msb exec --stream --no-tty`, then ending. `write_file` depends on it entirely: if the engine does not pass stdin on, every write produces an empty file. Check the size and content of a written file.
- [ ] A timed-out `exec`: stopping the engine's process on the host also stops the command inside the VM, and leaves no process behind. Check with `ps` in the sandbox.
- [ ] Output over 256 KiB being cut without the engine or the command hanging.
- [ ] `dirname`, `mkdir -p` and `cat` behaving as expected in the guest's shell, including paths with spaces.
- [ ] A model in Claude Code using the tools on a real project, with `Bash` and `Read` denied, and never reaching the host's copy of the project.
- [ ] Codex with `codex mcp add`, `--sandbox read-only` and `--ask-for-approval untrusted`. Codex was not installed where this was built.
- [ ] The audit log holding one `TerminalOpened` entry per `exec`, `read_file` and `write_file` call.

## Must run on a real VM before any of this is described as working

### The start sequence

The daemon's order of operations comes from the spike, but the daemon has never
performed it. Each line is a separate thing to confirm.

- [ ] `create` with the resources, ports and network rules the daemon builds, not hand-written flags.
- [ ] Removing a stale `/var/run/docker.pid` and starting `dockerd` as a detached job, then the poll loop finding it ready inside the 90-second limit.
- [ ] `copy` of a real snapshot to `/work`, and Compose finding its files there.
- [ ] `docker compose up --build -d --wait` driven through `exec` with a 25-minute limit.
- [ ] `docker compose ps -a --format json` output. **The parser has only ever seen JSON written by hand.** Compose's real shape, one object per line or an array, decides whether services are reported at all.
- [ ] Restart: `down` then `up` on an existing sandbox, through `start` rather than `create`, with the stack and its data coming back.
- [ ] `down` asking Compose to stop before the sandbox stops, and whether that is enough to avoid the crash recovery the spike saw.

### Dockerfile-only projects

Built on Nov 5, 2026 and tested only against `FakeEngine`, with `tests/fixtures/dockerfile-only`. **Never run on a real VM.** The tests show the plan, the approval and the generated file reaching the engine's copy; they say nothing about Compose or Docker accepting it.

- [ ] Compose v5.7.0 in the guest accepting the generated `compose.yaml` and building `app` from `/work` with `/work/Dockerfile`.
- [ ] The long-syntax ports (`target`, `published` as a string, `protocol: tcp`) publishing as expected, and an approved port reaching the app on `127.0.0.1` with its number unchanged. `tests/fixtures/dockerfile-only` answers on 3000 and 8080.
- [ ] `ps`, `logs`, `shell app`, `exec --service app` and `down` finding the generated file by name in `/work`.
- [ ] Restart: `down` then `up` reusing the file already in the guest.
- [ ] The heredoc in the fixture building, which needs BuildKit's Dockerfile 1.4 syntax in the guest's Docker.

### Ports, resources, rules

- [ ] Ports the daemon approved appearing on `127.0.0.1` only, with their numbers unchanged, and gone after `down`.
- [ ] A port in the plan but not approved **not** appearing.
- [ ] The resources the daemon asks for holding, and the 3 GiB build floor being the right floor for a project other than `demo_shop`.
- [ ] The allowlist the daemon builds, as opposed to the hand-written one the spike used.

### The question Milestone 0 has to answer

**Unresolved, and it blocks two features.** The spike found that terminating
TLS at the gateway breaks HTTPS inside nested containers and build steps. So:

- [ ] Whether a sandbox's network policy can change between build and run. **Never tested.** Because it is unknown, the daemon merges the build and run host lists for the sandbox's whole life, and says so in the plan. If policy can change, that merge should go away.
- [ ] Whether a build can be made to trust the gateway's certificate authority without editing the project's Dockerfiles.

Until one of those is answered, the daemon refuses a proxy-held secret rather
than guessing, and the plan carries the reason.

### Comparing a sandbox with its snapshot (`diff`)

Built on Nov 5, 2026 and tested only against `FakeEngine`, which plays the guest by streaming a bundle that ordinary git made on the host. **Never run on a real VM.** The tests show what the host does with whatever comes back, including bundles that are hostile, cut short, too big or not base64. They say nothing about the guest side.

- [ ] The snapshot's `.git`, made on the host, arriving in the guest at `/work/.git` through `msb copy`, with the index and refs intact.
- [ ] `git`, `base64` and `mktemp` existing in `docker:dind`. **Unknown, and it decides whether `diff` works at all.** The leak-probe notes below guess the image has no git. Without it, `diff` fails with the guest's `git: not found` and nothing else is affected; the fix would be a git binary the host provides, or a different image.
- [ ] The guest script itself: a temporary index, `add --all`, `commit-tree`, and `bundle create` excluding the baseline, as root in `/work`, where the copied files may belong to another user (hence `safe.directory`).
- [ ] **The bundle leaving the guest as base64 on `msb exec` output**, through the engine adapter's streamed exec. It is line-buffered and passed through the secret redactor: a delivered secret's value appearing in the base64 text would be masked and the bundle refused, and how the real engine handles a stream of this size is unmeasured. The 128 MB cap is enforced on the bytes as they arrive.
- [ ] Time to bundle and review a project the size of `demo_shop`, against the 10-minute limit in the daemon and the 3-minute limit in the full-screen interface and the desktop app. `secbox diff` itself waits as long as the daemon does.
- [ ] The hostile demo's planted `.claude/settings.json` appearing in `/work` through the `api` container's mount, and `diff` flagging it. See `tests/fixtures/hostile-demo/README.md`.

There is deliberately no copy-out step: the engine has none, and one would have the engine write guest-shaped files on the host.

## Built, but never run against anything real

| Thing | State |
| --- | --- |
| Snapshot through the git broker | Wired. Needs git 2.50, so it has never run, here or on a VM |
| `gitref` HEAD resolution | 9 tests pass against handmade `.git` directories. Never run against a real repository's packed refs at scale |
| The daemon's socket on macOS | The peer check uses a different system call there (`LocalPeerCred`). That branch has never been compiled, let alone run |
| Editor checks | Verified against planted files. The real settings paths on macOS, and actually launching an editor, are unexercised |
| Leak probes | Self-test passes, 167 checks. **No probe has run in a guest.** See below |
| Secret delivery, environment and file routes | Built and tested against a fake engine. Never run in a guest. The three things to confirm are below |

### Secret delivery, specifically

Three assumptions hold the design up, and none has been seen working:

- **`msb exec` accepts standard input.** The adapter stops passing `--no-stdin` and writes the value to the child. If the engine does not read it, no secret arrives. This is the whole delivery channel.
- **A `tmpfs` can be mounted inside the guest.** Delivery refuses rather than falling back to the sandbox's own disk, so if the mount fails on a real guest, every project with a secret stops starting. That is the right failure, but it has to be seen.
- **Compose merges the generated override as expected.** Verified here against Compose v5.6.1 by parsing only: a relative base file plus an absolute override keeps the project directory, resolves build contexts correctly, puts the value on the named service alone, and leaves `${VAR}` in another service empty. Never run against the guest's own Compose v5.7.0 with a daemon behind it.

Seen outside a VM on Nov 7, 2026: the daemon's own override and environment files, used with real Docker 29.9 and Compose 5.6 on Linux, gave a secret ticked for `web` only to `web`; `api` had neither the variable nor the file, and neither container could see `/run/secbox`. A Dockerfile-only project's `RUN env` saw no secret, and the built image held none. This shows the files are right, not that the guest delivers them.

Also unverified: that a value written over standard input does not transit the engine's guest-state share on the host. The argument-list route was avoided for exactly this reason, but `copy_in` was never measured either.

### Leak probes, specifically

Every probe added in this round is unproven. The ones with a known risk of
being uninformative rather than wrong:

- **QUIC** — depends on whether BusyBox `nc -u` in the guest behaves as it does on the host, and whether the control target answers Version Negotiation. If not, everything SKIPs.
- **IPv6** — the spike found the guest has no global IPv6 address or route, so these are expected to SKIP on today's engine. They need an IPv6 network.
- **DNS volume** — parsed against a stub shaped like BusyBox `nslookup`, never the real one.
- **Domain fronting** — whether BusyBox `wget --header` keeps one `Host:` header or sends two is unverified, which is why only the denied host's own content counts.
- **Writes and purchases** — never actually sent to a registry or a payment API.
- **Host sockets and keychain** — the path lists are guesses about what a leak would look like, not measurements.
- **Guest-state plants** — the share's path is taken from the spike's prose and has not been confirmed for a probe sandbox.
- **Bundle hooks** — `docker:dind` probably has no `git`, so these likely SKIP until a scenario installs it.

Rerun all six scenarios before trusting the spike's tables again: the IPv6
probe was rewritten and the allowlist widened, so the recorded results are no
longer exactly reproducible.

## Blocked hostnames

Ran on a real VM, on engine 0.7.13 on macOS, in throwaway sandboxes:

- Through a test daemon and `secbox up`, at 512M: `net blocked`, `net blocked -f`, the count in `status`, the Traffic panel on a pty, a stop and start, and `down --destroy`. A lookup from a nested Compose container appeared too.
- Crafted DNS queries sent by hand, with a newline, a space, ` domain=`, an escape code and a NUL in the name. The engine escaped each one.
- A flood of file activity that pushed all earlier events out in about 12 seconds. `-f` still showed the event after it.
- Attempts on the logs folder from inside the guest as root: through both file shares, and through a symlink. All failed.
- An `--env` value and a value on standard input were sent through `exec`, and neither appeared in `runtime.log` at debug. An `--env` value printed by the guest did appear in `exec.log`, as all exec output does, at any level.

Not run, or not known:

- Any other engine version. The line format, the rename rule and the shares could all change, and a changed line would show as an empty list.
- Linux.
- Crafted requests to the file share from a modified guest kernel. The kernel appears to have no module support, but the guest has `/dev/mem`.
- `demo_shop` at debug. How fast its build fills the 50 MB is unmeasured. One small Compose start wrote about 5 MB.
- The cost of debug logging. One tight file-activity loop ran about 9% slower than at the default level, from one run of each.

## The hostile demo

`tests/fixtures/hostile-demo-run` holds a secret in `api` and tries to send it out. On a real VM, run it as in `tests/fixtures/hostile-demo/README.md` and record what each probe printed. Until then, these are claims:

- every probe prints BLOCKED, including the raw IP, metadata and token-in-DNS ones;
- `net blocked` lists `exfil.example.net` and `build-exfil.example.net`;
- reading `/run/secbox/env.api` from inside `api` fails.

Per-project build hosts have not built a real project. Before relying on them, build `demo_shop` and the demo on a real VM and check `net blocked` lists no name the build needed. A project the detection misses fails its build rather than leaking.

Known gap, not a test: the build hosts a project gets stay reachable while it runs, and some accept uploads from anyone. A secret held by a service could be sent there. `status` and every plan list them under "Where secrets could go".

## Not built at all

Nothing below exists, so there is nothing to test yet.

- The network gateway and the proxy. **The rule engine in `crates/egress` decides nothing today, because nothing calls it on real traffic.** Blocked hostnames come from the engine's own log instead, with the limits above.
- Secret delivery by the proxy route. Held back on purpose: it needs the gateway to terminate TLS, and a plan carrying one is refused with that reason.
- A snapshot that includes uncommitted changes. The flag exists and refuses.
- `sync`.
- `net log`, `net allow`, a record of blocked raw IP, metadata and UDP attempts, and a clear error inside the app for a blocked request.
- `rebuild`, its review steps and rollback.
- `export`. It still exits 69. `diff` is built; see above.
- A Touch ID approver, and a passphrase approver for headless Linux.
- Git history between a sandbox's commits and a real remote, and SSH remotes.
- Everything under AI and MCP.

## One more thing that is a design question, not a test

Moving a project folder still loses its secrets, because a project's identity is
a hash of its path. Fixing it by reading something out of the repository would
let a hostile repository claim another project's identity, so the fix has to
keep the authority on the host. It is not done.
