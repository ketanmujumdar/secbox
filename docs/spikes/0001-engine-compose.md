# Spike 0001: Can microsandbox run Docker Compose?

**Status: blocked. The main question is unanswered.** No VM was booted, so this spike has no result on Docker, Compose, performance or isolation. A user decision is needed before it can continue (see [Decision needed](#decision-needed)).

Run on 2026-11-05 on macOS 26.5.1 (25F80), Apple Silicon, 48 GB RAM, 12 CPUs, with microsandbox v0.7.13.

## The blocker

Every command that creates a sandbox fails before boot:

```console
$ MSB_HOME=$PWD/.tools/msb .tools/msb/bin/msb create docker:dind --name spike-dind --memory 2G --cpus 2 --root-disk flat:10G --quiet
error: invalid config: sandbox runtime socket path is too long: shortest derived path is 120 bytes, but Unix socket paths on this platform must be shorter than 104 bytes; set MSB_HOME to a shorter directory
exit=1
```

`msb run docker:dind` and `msb create alpine` fail with the same error, so it doesn't depend on the image or the command.

**Why it can't be fixed within the spike's limits:**

- `MSB_HOME` here is 70 bytes and the derived path is 120 bytes, so the CLI adds 50 bytes. That figure comes from one observation and fits the source below.
- `MSB_HOME` therefore has to be 53 bytes or fewer. The project root alone is 59 bytes, so no directory inside the project can work.
- A relative value doesn't help: `MSB_HOME=.tools/msb` gave the same error, and `msb doctor` showed it resolved to the absolute path.
- In the v0.7.13 source, `run_dir()` is always `home/run` (`sdk/rust/lib/config/mod.rs`), and `home` is made absolute (`sdk/rust/lib/config/runtime_paths.rs`). The `paths.*` settings listed there have no run directory entry.

**What was deliberately not tried:** a shorter `MSB_HOME` outside `.tools/`, a short symlink pointing into `.tools/`, moving the project, and patching the source. Each would put something outside `.tools/` or change the tool. The spike's rule was to stop and report in that case, so whether any of them works is unknown.

**This is not evidence against microsandbox's ability to run Compose.** It only comes from the depth of this project's path.

## Decision needed

Each option needs the user's approval, because each changes a limit or moves the project.

| Option | What changes | Trade-off |
| --- | --- | --- |
| A. Symlink, such as `/tmp/sbx` to `.tools/msb` (recommended) | One link outside `.tools/`. All data stays in `.tools/` | Smallest change. Not tested, so the CLI might not accept it |
| B. Short `MSB_HOME` outside `.tools/`, such as `~/.sbx-msb` | Up to about 40 GB of state outside the project | Matches the CLI's own advice. Needs separate cleanup |
| C. Move or clone the project to a root of 42 bytes or fewer, such as `~/dev/secbox` | Project path | Keeps every limit as written, but disrupts agents working in this path now |

## Results by question

Nothing below was skipped by choice. Each item needs a running VM.

| # | Question | Result |
| --- | --- | --- |
| 1 | Does dind start? Does `docker compose version` work? | **Not run** |
| 2 | Does `docker compose up --build` succeed for demo_shop? | **Not run** |
| 3 | Boot time, Docker-ready time, build time, peak and idle memory | **Not measured.** The architecture doc's estimates stay unverified |
| 4 | Isolation probes, allowlist and secrets, including nested traffic | **Not run.** The most important security finding remains open |
| 5 | Published ports on `127.0.0.1`, checked with `lsof` | **Not run** |
| 6 | Memory and CPU limits hold under an allocation test | **Not run** |

## Verdict

**No verdict.** The evidence supports neither using microsandbox as the first engine, using it partly, nor rejecting it. No alternative is recommended, because nothing essential has been shown not to work.

Nested traffic is what decides this. Run it first once a VM boots: if allowlists or secret placeholders don't hold for nested containers and `docker build` steps, the architecture's egress model doesn't hold on this engine.

## What was observed

**Install.** The published bundle was downloaded directly, without running `install.sh`, because that script writes to `~/.microsandbox` and `~/.local/bin` by default.

```console
exp=5eb53ca1d6beb36caa64d19602284b9ce37872eff340a8db0542ab91b5808c96
act=5eb53ca1d6beb36caa64d19602284b9ce37872eff340a8db0542ab91b5808c96
CHECKSUM_OK
$ .tools/msb/bin/msb --version
msb 0.7.13
```

**Host check.** `msb doctor` passes, even though creating a sandbox fails. It doesn't check the socket path length. It writes its report to stderr, even on success.

```console
  ✓ libkrunfw           .../.tools/msb/bin/../lib/libkrunfw.5.dylib
  ✓ Root clone          reflink supported
  ✓ Architecture        Apple silicon (arm64)
  ✓ Host setup is ready.
exit=0
```

**Order of failure.** Image pull runs before the socket check, so `msb create docker:dind` downloaded the image (about 630 MB on disk) and then failed. A preflight check saves that wait.

**CLI behavior with no sandboxes:**

| Command | Output | Exit |
| --- | --- | --- |
| `status -a --format json`, `status --format json`, `list --format json` | `[]` | 0 |
| `status nope --format json` | stderr: `error: sandbox not found: nope` | 1 |
| `exec`, `logs`, `stop`, `remove`, `copy`, `start`, `metrics` on `nope` | stderr: `error: sandbox not found: nope` | 1 |

**Test copy.** `git ls-files` listed `.env.example` and `infra/.env.production.example` but no `.env`. The copy at `.spike/demo_shop` (11 MB) was made with `git archive` and has no `.env`. Neither the source repo nor any env file was read beyond that listing. No VM ever saw the copy.

**Other limits.** Docker Desktop wasn't touched. No ports were published, and no sandbox exists (`msb status -a` shows `No sandboxes found.`). `~/.microsandbox` and `~/.local/bin/msb` don't exist.

## Computed, not observed

These use the 50-byte figure above and were not run.

| State directory | Bytes | Derived path | Under the 104-byte limit? |
| --- | --- | --- | --- |
| `/Users/ketanmujumdar/.microsandbox` (CLI default) | 34 | 84 | Yes |
| `/Users/ketanmujumdar/Library/Application Support/secbox/msb` | 59 | 109 | **No** |

If this holds, it's a product constraint on macOS: the usual Application Support location would fail here, and longer usernames leave less room. The daemon would need a short state directory and a clear error.

## Documented, not tested

From the docs and v0.7.13 source. These are the vendor's claims and need testing.

| Topic | What it says | Source |
| --- | --- | --- |
| Compose | Not mentioned. `docker:dind` is shown with `--memory 2G` and `--root-disk flat:10G` | Docker guide |
| Default egress | Public internet and gateway DNS only. Private ranges, loopback, link-local, metadata and the host are denied | `security/network.md` |
| Enforcement | A host-side user-space network stack checks every packet | `security/network.md` |
| Host access | `host.microsandbox.internal`, denied by default | `security/network.md` |
| Published ports | Bind to `127.0.0.1` by default | `security/network.md`; `default_host_bind` in `crates/network/lib/model/config/types.rs` |
| Secrets | Placeholder in the guest, swapped on the host. By default this needs intercepted TLS and an allowed host | `security/secrets.md` |
| Nested containers and `docker build` | **Neither page mentions them** | Both pages above |

Host-side enforcement suggests nested traffic would be covered, since it has to leave through the same interface. That is a hypothesis, not a result. TLS interception is a particular unknown, because a nested container or build step wouldn't automatically trust an interception CA.

## Leak probes

`tests/leak/probes/` holds probes that run inside the guest. Each prints `PROBE <name> REACHED|BLOCKED|SKIPPED <detail>`. **They have never run in a guest**, so none of their results here say anything about isolation.

| Script | Checks |
| --- | --- |
| `run-all.sh` | Entry point: guest, then nested containers, then builds |
| `context.sh` | Controls, then all network and secret probes for one context |
| `control.sh` | Own listener, closed port and an allowed target |
| `files-host.sh` | Host canary file, macOS paths, unexpected host shares |
| `net-host.sh` | Host `127.0.0.1` listener through loopback, alias and gateway |
| `net-lan.sh`, `net-metadata.sh`, `net-denied.sh` | LAN address, `169.254.169.254`, denied name, raw IP and DNS |
| `secret-env.sh`, `secret-inject.sh` | Real value in the environment; injection to allowed and denied hosts |
| `nested-container.sh` | The same context in default bridge, user-defined bridge (as Compose uses) and host network containers |
| `nested-build.sh` | The same context in `docker build` RUN steps, with BuildKit and the legacy builder |

**How a broken probe is kept from looking like a pass:**

- A failure is only BLOCKED if all three controls passed in that same context. Otherwise it is SKIPPED. A probe run alone can't report a network BLOCKED.
- Each nested context runs its own controls.
- Failed controls are SKIPPED, never BLOCKED.
- Host listener checks need a token in the response. An answer without the token is SKIPPED, so a gateway error page can't count as either result.
- Missing tools, an unready Docker, an image that isn't loaded and runs with no output are all SKIPPED. Probes never pull images.
- On a non-Linux machine the probes refuse to use `docker`, so they can't reach Docker Desktop.

**Self-test.** `tests/leak/selftest.sh` checks the reporting logic on the host, with a throwaway `127.0.0.1` listener and a stub docker: 50 checks, 0 failed. It includes the required positive control, showing that the same probe reports REACHED on a live target and BLOCKED on a closed one. This is a test of the probes, not the positive control the coordinator asked for in the guest, which is still to do.

It found two real bugs, both fixed: a leading space that broke the line format, and macOS `nc -w` not limiting the connect phase (a dropped-packet target took 75 seconds against a 2-second limit).

**Known probe gaps:**

- Only tested with macOS tools. Alpine's BusyBox `nc`, `wget` and `nslookup`, and the `ip route` gateway lookup, are untested.
- The docker paths have only run against a stub.
- Raw TCP checks treat any completed connect as REACHED. If the gateway accepts and then drops connections, BLOCKED targets will show as REACHED. That errs on the safe side but may need HTTP-level checks.
- A control proves the path to the allowed target, not that other targets take the same path.
- Not covered: IPv6, UDP and QUIC, IPv6 metadata, `/proc` environment scans, and DNS tunneling beyond one lookup.
- The host listener, canary file, fake secret and echo services must be set up by whoever runs the probes. No host-side harness exists yet.
- `shellcheck` isn't installed here, so the scripts haven't been linted.

## Next steps once a VM boots

1. Pass `PROBE_CONTROL_TARGET` and run `tests/leak/probes/run-all.sh` with the default policy. Confirm the controls report REACHED in every context before reading any BLOCKED.
2. Repeat with an allowlist and a fake secret. This is the nested-traffic result.
3. Run question 1, then 2, 3, 5 and 6, keeping within the spike's disk and time limits.
4. Enable adapter operations in `crates/engine` only after each has been seen working.

## Left on disk

| Path | Size | Contents |
| --- | --- | --- |
| `.tools/msb` | 695 MB | CLI, libkrunfw, cached `docker:dind` layers, empty database |
| `.tools/dl-0.7.13` | 31 MB | Verified bundle and checksums |
| `.spike/demo_shop` | 11 MB | Test copy |

All of it is git-ignored and nothing is running. The image cache was kept so that a rerun doesn't download it again. Remove `.tools/` and `.spike/` to clean up.
