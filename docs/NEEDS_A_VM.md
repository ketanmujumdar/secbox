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
| git 2.43, and `gitbroker` requires 2.50 | 10 unit tests, all 54 tests in `tests/hostile.rs` and the one in `tests/env_scrub.rs` never ran. They compile |

## Logs and terminals: what ran on a real VM

On Nov 5, 2026, on a Mac, against a running `demo_shop` sandbox, using a second daemon with its own state folder. Its engine path was a test wrapper that pinned the engine's state folder, refused lifecycle commands and set `HISTFILE=/dev/null`.

- Ran: `logs` for all services and for one, `-f` with Ctrl-C on a pty, `shell`, `shell SERVICE`, `exec` on a terminal and on pipes, and the TUI Logs panel. After Ctrl-C the sandbox was still running, no `compose logs` process was left in the guest, and the terminal settings were unchanged.
- [ ] Refusing a sandbox that is really stopped. This ran only with a stub engine that reports `Stopped`, because the real sandbox was in use.
- [ ] Hostile output from inside a VM. Hostile bytes ran only from a stub engine through the real binaries and socket, and in tests.
- [ ] Commands typed into `shell SERVICE`. Only opening it and leaving with Ctrl-D ran, so as not to write a history file in the container.

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

## Built, but never run against anything real

| Thing | State |
| --- | --- |
| Snapshot through the git broker | Wired. Needs git 2.50, so it has never run, here or on a VM |
| `gitref` HEAD resolution | 9 tests pass against handmade `.git` directories. Never run against a real repository's packed refs at scale |
| The daemon's socket on macOS | The peer check uses a different system call there (`LocalPeerCred`). That branch has never been compiled, let alone run |
| Editor checks | Verified against planted files. The real settings paths on macOS, and actually launching an editor, are unexercised |
| Leak probes | Self-test passes, 167 checks. **No probe has run in a guest.** See below |

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

## Not built at all

Nothing below exists, so there is nothing to test yet.

- The network gateway and the proxy. **The rule engine in `crates/egress` decides nothing today, because nothing calls it on real traffic.** `status` says outbound requests are not recorded, rather than reporting none.
- Secret delivery into a sandbox, by any route.
- A snapshot that includes uncommitted changes. The flag exists and refuses.
- `logs`, `shell`, `exec`, `sync`.
- `net blocked`, `net log`, `net allow`, and a clear error inside the app for a blocked request.
- `rebuild`, its review steps and rollback.
- `diff` and `export`. They still exit 69.
- A Touch ID approver, and a passphrase approver for headless Linux.
- Git history between a sandbox's commits and a real remote, and SSH remotes.
- Everything under AI, MCP, and the terminal interface.

## One more thing that is a design question, not a test

Moving a project folder still loses its secrets, because a project's identity is
a hash of its path. Fixing it by reading something out of the repository would
let a hostile repository claim another project's identity, so the fix has to
keep the authority on the host. It is not done.
