# Spike 0001: Can microsandbox run Docker Compose?

**Yes.** `docker compose up --build` for `demo_shop` succeeded inside a microsandbox VM in about 85 seconds, and an allowlist constrained traffic from nested containers and `docker build` steps as well as the guest.

**Verdict: use microsandbox as the first engine, with the conditions in [Verdict](#verdict).** The main unsolved problem is that secret injection needs TLS interception, and with interception on, HTTPS from nested containers and build steps fails until they trust the gateway's CA.

Run on 2026-11-05 on macOS 26.5.1 (25F80), Apple Silicon, 48 GB RAM, 12 CPUs, with microsandbox v0.7.13 (`docker:dind` with Docker 29.9.3 and Compose v5.7.0, kernel 6.12.113, arm64).

Raw logs are in `.spike/logs/` (git-ignored). State lives in `~/.secbox/msb`, because the project path is too long for it (see [State directory](#state-directory)).

## Results at a glance

These are observed results.

| # | Question | Result |
| --- | --- | --- |
| 1 | Does dind start? Does Compose work? | **Yes.** Compose v5.7.0 is already in `docker:dind`, so no install is needed |
| 2 | Does `compose up --build` work for demo_shop? | **Yes**, with 3 GB or more. **2 GB fails** with OOM kills during `go build` |
| 3 | Time and memory | Docker ready in 2.2 to 3.1 s, build in 83 to 104 s, peak guest use 2.4 to 2.8 GiB, idle 0.6 to 0.7 GiB |
| 4 | Isolation, including nested traffic | Host, LAN and metadata blocked, and the allowlist holds in all 6 contexts, with positive controls. Secret injection works, but nested HTTPS needs the CA added |
| 5 | Published ports | **Yes**, same port numbers, bound to `127.0.0.1` only. Closed on the LAN address |
| 6 | Limits per sandbox | **Hold** for memory, CPU and disk |

## 1. Docker and Compose

```console
$ msb exec spike-dind --no-stdin --no-tty -- sh -c 'docker --version; docker compose version'
Docker version 29.9.3, build 5457199
Docker Compose version v5.7.0
```

`dockerd` is not started by `msb create`, even though the image's entrypoint is listed as the sandbox command. It has to be started as a detached job. `hello-world` then ran, and the daemon used `overlayfs` on cgroup v2.

## 2. demo_shop build

```console
$ msb exec spike-crm --no-stdin --no-tty --timeout 28m -w /work -- sh -c 'docker compose up --build -d --wait --wait-timeout 300'
up_exit=0 up_seconds=88.5
SERVICE    STATE     HEALTH
api        running   healthy
mailpit    running   healthy
postgres   running   healthy
s3         running
seed       exited    (exit 0)
web        running
worker     running
worker     running
```

The goal was all default services healthy. All 3 services with a health check are healthy. `web`, `s3` and both `worker` replicas define no health check, and `seed` is a one-shot that exited 0. `web` returned 200 from the host, and `api` returned `{"status":"ok"}` from `/healthz`.

No real secrets were provided. The copy has no `.env`.

**Memory needed for a clean build, all at 4 CPUs:**

| VM memory | Result | Time | Peak guest use | OOM lines in `dmesg` |
| --- | --- | --- | --- | --- |
| 6G | Success (2 runs) | 88.5 s, 104.0 s | 2376, 2531 MiB | 0 |
| 4G | Success (2 runs) | 82.9 s, 83.1 s | 2808, 2618 MiB | 0 |
| 3G | Success | 84.7 s | 2505 MiB | 0 |
| 2G | **Failed 3 of 3 tries** | 54 s, 25 s, 24 s | 1934 MiB | 9 |

```text
target seed: failed to solve: ResourceExhausted: process "/bin/sh -c go build -trimpath ...
```

One earlier 2G attempt failed instead on a transient network error fetching from `production.cloudfront.docker.com`. That wasn't memory, so the test was rerun.

A sandbox's `--memory 2G`, the value in microsandbox's Docker guide, isn't enough for this project.

**After a stop and start**, the stack came back with its data: Docker was ready in 2.77 s, `compose up -d --wait` took 30.57 s, `web` returned 200, and Postgres still had its 59 tables. Two things to handle:

- `dockerd` doesn't restart on its own, and it refused to start until the stale pid file was removed:
  ```text
  failed to start daemon, ensure docker is not running or delete /var/run/docker.pid: process with PID 230 is still running
  ```
- `msb stop` took 0.41 s with the stack up, and Postgres then logged `end-of-recovery`. So it isn't a clean shutdown for nested containers. Stop the workload first when that matters.

## 3. Time and memory

| Measure | Observed | Source |
| --- | --- | --- |
| `msb create` returns (image cached) | 0.30 to 0.75 s | Host timer, 7 runs |
| First `exec` succeeds | 0.70 to 0.88 s after create starts | Host timer, 3 runs |
| Docker ready | 2.22 to 3.08 s after create starts | Host timer, polling `docker info` |
| Build and start demo_shop | 82.9 to 104.0 s | Host timer |
| Peak guest memory, build | 2376 to 2808 MiB | `MemTotal - MemAvailable` in the guest, every 2 s |
| Peak guest memory, build | 2736 MiB (4G run) | `msb metrics` |
| Idle guest memory, stack running | 624 to 743 MiB | `msb metrics`, 45 s or more after the build |
| Idle, sum of containers | About 190 MiB | `docker stats`, first run |
| Fresh VM with Docker, no project | 159 MiB in guest, 314 MiB host RSS | `msb metrics`, 5 s after boot |
| Guest disk used after build | 5.2 GiB, of which 3.96 GB is build cache | `df`, `docker system df` |
| Host disk for one sandbox | 5.6 GiB | `du` |

**Host memory stays high after a build.** The host process kept almost the full VM size resident, and dropping the guest's caches did not release it within 6 seconds:

| VM memory | Host RSS after build | After `drop_caches` in the guest |
| --- | --- | --- |
| 6G | 6108 MiB (45 s after build) | Not measured |
| 4G | 4092 MiB | 4092 MiB |
| 3G | 3068 MiB | Not measured |
| 2G | 2044 MiB | 2044 MiB |

In the first 6G run, `msb metrics` later showed 524 MiB and `ps` showed 650 MiB, after a longer idle period that wasn't timed. So RSS can fall, but what causes that wasn't determined.

**Compared with the architecture doc's estimates:**

| Usage | Estimate | Observed | Reading |
| --- | --- | --- | --- |
| VM, Docker and gateway | 0.5 to 1 GB | 159 MiB in guest, 314 to 538 MiB on the host | Lower than estimated |
| `demo_shop` running | 1 to 2 GB | 624 to 743 MiB in guest | Lower in the guest. Host use can stay at the full VM size for a while |
| `demo_shop` building | 4 to 6 GB | 2.4 to 2.8 GiB peak; 3G works, 2G doesn't | Lower. Budget the full VM size on the host |

These numbers come from one project on one machine, with images pulled fresh each time.

## 4. Isolation

### How to read the tables

Probes are in `tests/leak/probes/`, and `tests/leak/run-guest.sh` runs a whole scenario. Results come from `.spike/logs/final-*.txt`. The 6 contexts are:

- the guest itself
- containers on the default bridge
- containers on a user-defined bridge, as Compose uses
- containers on host networking
- `docker build` RUN steps with BuildKit
- `docker build` RUN steps with the legacy builder

**A BLOCKED only counts after a positive control passed in the same context.** The controls passed in all 6 contexts of every scenario: the method detects its own listener, detects a closed port, and reaches an allowed target.

**Host canaries.** A throwaway listener on the host's `127.0.0.1` received each payload, or didn't. It was stopped after each run.

### Nested traffic: the main security finding

In every row, all 6 contexts gave the same result, except where noted. R is REACHED, B is BLOCKED, S is SKIPPED.

| Check | Default policy | `--net host` (positive control) | Allowlist |
| --- | --- | --- | --- |
| Canary reaches host listener, TCP, by alias and gateway | B | **R** | B |
| Canary reaches host listener, UDP, by alias and gateway | B | **R** | B |
| HTTP to host listener, token returned, by alias and gateway | B | **R** | B |
| Host LAN address (`192.168.50.219:7000`) | B | B | B |
| LAN router (`192.168.50.1:80`) | B, gateway denied | B, gateway denied | B, gateway denied |
| `169.254.169.254:80` | B, gateway denied | B, gateway denied | B, gateway denied |
| Denied host, real content over HTTP | R | R | B |
| Denied host, real content over HTTPS | R | R | B |
| External DNS server over UDP (`1.1.1.1`) | R | R | B |
| DNS lookup of a denied name | R | R | **B without `allow@dns`, R with it** |
| Allowed host, real content over HTTP and HTTPS | R | R | R |

**LAN positive control.** From the host, the router returned 200 and the AirPlay port returned `403 AirTunes`. A guest created with `--net private` reached both, `PROBE lan.router REACHED response:192.168.50.1:80:HTTP/1.0 200 OK`, so the BLOCKED results above mean what they say. That guest was still denied metadata. There's no real metadata service on this laptop, so its evidence is the gateway's explicit denial, not a positive control.

**Real build under an allowlist.** demo_shop built and started in 87.5 s, with the same result as above, with only these hosts allowed and `--net-strict=false`:

```text
registry-1.docker.io auth.docker.io production.cloudflare.docker.com production.cloudfront.docker.com
docker-images-prod.6aa30f8b08e16409b46e0173d6de2f56.r2.cloudflarestorage.com
registry.npmjs.org proxy.golang.org sum.golang.org storage.googleapis.com dl-cdn.alpinelinux.org
fonts.googleapis.com fonts.gstatic.com
```

Without the two font hosts the build failed, which shows that the allowlist applied inside a real `docker build`:

```text
#25 6.299 Failed to fetch Inter from Google Fonts.
#25 ERROR: process "/bin/sh -c npm run build" did not complete successfully: exit code: 1
```

From the running `api` container, `example.com` failed with `bad address` while `registry.npmjs.org` worked.

### Raw TCP connects through the gateway are misleading

- With no allow rule, port 80 to any private address "connects", and the gateway answers itself:
  ```text
  HTTP/1.1 403 Forbidden
  {"code":"network_policy_denied","domain":"x","message":"Request blocked by network policy."}
  ```
- With `--tls-intercept`, `1.1.1.1:443` "connects" while real HTTPS content stays blocked.
- In strict mode without interception, `registry-1.docker.io:443` "connects" and then TLS ends in an EOF.

The probes therefore use the response content where they can. A connect alone was the first version's approach, and it reported metadata as REACHED, which was wrong.

### Secrets

Using a fake value, with `--tls-intercept --secret 'PROBE_FAKE_SECRET@httpbin.org'`. `postman-echo.com` was allowed on the network, but not for this secret.

| Check | Guest | Nested, default trust | Nested, with TLS checks off |
| --- | --- | --- | --- |
| Real value in the environment | B, `$MSB_PROBE_FAKE_SECRET` | B | B |
| Real value arrives at `httpbin.org` over HTTPS | **R** | **S, `Connection reset by peer`** | **R** |
| Real value arrives at `postman-echo.com` | B, 403 | S, reset | B, 403 |
| Real value arrives at `httpbin.org` over plain HTTP | B, 403 | B, 403 | B, 403 |

Nested results were the same across all 5 nested contexts. The table uses the runs with a 15-second limit, because a 5-second limit gave a false BLOCKED against the slow echo service.

So placeholder injection applies to nested traffic. However, nested containers and build steps don't trust the interception CA, so their HTTPS fails by default. A real build fails the same way:

```text
0.436 ...tls_post_process_server_certificate:certificate verify failed
#13 ERROR: process "/bin/sh -c apk add --no-cache poppler-utils tesseract-ocr tesseract-ocr-data-eng" did not complete successfully: exit code: 3
```

Adding the guest's CA worked for `wget` in one container, by mounting `/.msb/tls/ca.pem` and appending it to the CA bundle. A full build with the CA added wasn't tried, and it would need Dockerfile changes or a base image change.

### DNS

- **`allow@dns` opens a channel.** With it, any name resolves, such as `www.example.org`. That can carry data out through a resolver. The docs' example uses this rule, so it's easy to copy.
- **Without it, DNS is filtered.** Allowed names resolve, and others return `NXDOMAIN`, including random subdomains. Queries to other DNS servers got no answer.

### IPv6

**Not determined.** The guest has no global IPv6 address or default route, so all IPv6 probes reported SKIPPED. Forcing an address and default route in the guest gave `Host is unreachable`. The host itself has no global IPv6 address here, so that says nothing about a host that has one. This needs testing on an IPv6 network.

### Host files

- `files.canary`: a canary file on the host was not visible. `files.macos-paths`: `/Users`, `/Volumes` and others are absent. No unexpected host shares appear in `/proc/mounts`. The file control passed.
- **`/.msb` is a writable host directory**, `~/.secbox/msb/sandboxes/<name>/guest-state`. It's the engine's own share, and it's present in every sandbox:
  - A guest-made symlink becomes a **real symlink on the host**: `lnk -> /etc/hosts`. Inside the guest it resolves within the guest. Host code that follows paths in that directory could be sent elsewhere.
  - Writes stop at about 16 MiB. A 3 GiB write ended at 16515072 bytes.
  - `chmod 4755` in the guest showed `-rwsr-xr-x` there, while the host file stayed `-rw-------`, owned by the user.

### Sandbox to sandbox

A second sandbox couldn't connect to the first's guest IP on ports 3000, 8080 or 5432, or reach its published ports through loopback or the host alias.

## 5. Published ports

```console
$ lsof -nP -iTCP -sTCP:LISTEN | awk '$1=="msb" {print $1,$9}'
msb 127.0.0.1:8333
msb 127.0.0.1:8888
msb 127.0.0.1:8080
msb 127.0.0.1:8025
msb 127.0.0.1:1025
msb 127.0.0.1:3000
msb 127.0.0.1:5432
```

All 7 keep their port numbers, and the `msb` process binds them, not Docker Desktop. From the host: web 200, `/healthz` 200, Mailpit 200, the SeaweedFS browser 200, S3 403 (no credentials, so an expected answer), and SMTP `220 ... Mailpit ESMTP Service ready`. Postgres accepted a TCP connection, though no login was attempted.

`192.168.50.219:3000`, `:8080` and `:5432` were closed. Compose asks for all interfaces inside the guest, but that doesn't reach the host's LAN. After `msb stop`, the ports were gone.

## 6. Resource limits

A VM created with `--memory 512M --cpus 2 --root-disk flat:2G`:

| Limit | Test | Result |
| --- | --- | --- |
| Memory | Allocate 2048 MiB with Python | OOM-killed at about 448 MiB. The VM stayed up |
| CPU | 4 busy loops | 198% host CPU on `ps`, so about 2 cores |
| Disk | Write 3 GiB | Stopped at 1.8 GiB, disk at 100% |
| Host memory | `ps` RSS | 639 MiB on a 512 MiB VM, so about 127 MiB of overhead there |

```text
Out of memory: Killed process 227 (python3) total-vm:485368kB, anon-rss:466432kB ...
```

`--memory`, `--cpus` and the disk size show correctly in `msb status`. Live resizing wasn't tested.

## Verdict

**Use microsandbox as the first engine.**

**Conclusions, drawn from the results above:**

1. It answers the main question. Compose, image builds and multi-service networking all work, with a boot time short enough not to matter.
2. It enforces egress where the architecture needs it. The allowlist applied to all nested traffic tested, which also fits the docs' claim of host-side enforcement.
3. The memory estimates in the architecture doc can come down, but a host budget should use the full VM size.

**Conditions to design for:**

| Finding | What it means |
| --- | --- |
| Secret injection needs TLS interception, which breaks nested HTTPS until the CA is trusted | **The main unsolved problem.** Options to test: build with name-only rules and run with interception, or install the CA in images. Whether a sandbox's network can change between build and run wasn't tested |
| `--net-strict=false` trusts the TLS server name only | Weaker than interception. Domain fronting through an allowed CDN wasn't tested |
| `allow@dns` lets any name resolve | Never use it |
| `/.msb` is a host directory the guest can write to, including symlinks | Never follow paths under `guest-state` on the host |
| `msb stop` isn't a clean shutdown for nested containers | Stop the workload first |
| `dockerd` doesn't start by itself, and a stale pid file blocks it after restart | The daemon has to handle both |
| `exec` on a stopped sandbox boots it for that command, and `copy` tries to | The adapter checks state first |
| `--quiet` hides errors on `remove` and `start` | The adapter doesn't use it there |
| A secret's host variable is needed again at each start | The daemon must supply it for the sandbox's whole life |
| The state directory must be 53 bytes or shorter | See below |
| Interception's CA private key is stored at `~/.secbox/msb/tls/ca.key` | A shared host secret. Mode is `0600` |
| Project is beta, and its CLI's behavior isn't a stable contract | Pin the version, and rerun these tests on upgrade |

**Not tested:**

- Documenso profile, or x86-only images
- Linux hosts, or any host with IPv6
- VM escape or attacks on the hypervisor
- DNS tunneling volume, domain fronting, or QUIC
- Long-running stability, or several large VMs at once
- Live resizing, snapshots, or `msb modify`

If the interception problem can't be solved with this engine, the alternative worth testing next is Docker Sandboxes, which the architecture doc names as the benchmark. That alternative hasn't been tested here.

## State directory

Observed with `MSB_HOME` inside this project:

```text
error: invalid config: sandbox runtime socket path is too long: shortest derived path is 120 bytes, but Unix socket paths on this platform must be shorter than 104 bytes; set MSB_HOME to a shorter directory
```

A 70-byte home gave 120 bytes, so the limit is 53 bytes. `~/.secbox/msb` (32 bytes here) works, and was approved for this spike. `~/Library/Application Support/secbox/msb` would be 59 bytes here and fail. A longer username leaves less room, so `MsbEngine::preflight` checks this.

## Probes and how far to trust them

`sh tests/leak/selftest.sh` checks the reporting logic on the host, with 54 checks. `tests/leak/run-guest.sh <scenario>` runs a scenario in a real VM and removes it afterward. Scenarios are `default`, `host-allowed`, `allowlist`, `allowlist-dns`, `allowlist-intercept` and `secrets`.

**Bugs real runs found, all fixed:**

- Raw TCP connects counted gateway answers as REACHED.
- BusyBox `nc` closed its send side too early, so some servers hung up. This showed as SKIPPED, not a false BLOCKED.
- `nc` has no `-6` option on BusyBox.
- Values with spaces were cut off in the build environment file.
- A 5-second limit gave a false BLOCKED against a slow service. The secret runs use 15 seconds.
- Image pull failures under an allowlist correctly showed as SKIPPED, not BLOCKED.

**Known gaps:**

- `allowlist-dns`, `allowlist-intercept` and `secrets` last ran before the `nc` stdin fix. That fix only affects results that were SKIPPED as `connected-no-response`, so their BLOCKED results still stand. `default`, `allowlist` and `host-allowed` were rerun after it.
- `denied.by-ip` is a raw connect and shows R under interception. Use `denied.content` and `denied.https` there.
- `shellcheck` isn't installed here, so the scripts aren't linted.

## Run it by hand

These ran as written under zsh, from the project root, on 2026-11-05. The copy at `.spike/demo_shop` must already exist.

```sh
export MSB_HOME="$HOME/.secbox/msb"
MSB="$PWD/.tools/msb/bin/msb"

"$MSB" create docker:dind --name crm-manual --memory 6G --cpus 4 --root-disk flat:30G \
  -p 127.0.0.1:3000:3000 -p 127.0.0.1:8080:8080 -p 127.0.0.1:5432:5432 \
  -p 127.0.0.1:8333:8333 -p 127.0.0.1:8888:8888 -p 127.0.0.1:8025:8025 -p 127.0.0.1:1025:1025

"$MSB" copy "$PWD/.spike/demo_shop" crm-manual:/work
"$MSB" exec --detach crm-manual --no-stdin -- sh -c 'dockerd >/tmp/dockerd.log 2>&1'
until "$MSB" exec crm-manual --no-stdin --no-tty -- docker info >/dev/null 2>&1; do sleep 1; done

"$MSB" exec crm-manual --no-stdin --no-tty --timeout 25m --workdir /work -- \
  docker compose up --build -d --wait --wait-timeout 300
"$MSB" exec crm-manual --no-stdin --no-tty --workdir /work -- docker compose ps -a

curl -i http://127.0.0.1:8080/healthz    # then open http://127.0.0.1:3000
"$MSB" exec crm-manual --workdir /work   # a shell inside the VM

"$MSB" remove --force crm-manual         # removes the VM and everything in it
"$MSB" status -a
```

Ports 3000, 8080, 5432, 8333, 8888, 8025 and 1025 must be free on the host. The interactive shell line wasn't run here, since it needs a terminal. To try the allowlist, add `--no-net --net-strict=false` and one `--net-rule allow@<host>` for each host listed above to `create`.

## Left on disk

Nothing is running: `msb status -a` shows `No sandboxes found.`

| Path | Size | Contents |
| --- | --- | --- |
| `~/.secbox/msb/cache` | 643 MB | Cached `docker:dind` and `alpine` images |
| `~/.secbox/msb/db` | 5 MB | State database |
| `~/.secbox/msb/tls` | 8 KB | Interception CA, including its private key |
| `.tools/msb` | 62 MB | CLI and libkrunfw |
| `.tools/dl-0.7.13` | 31 MB | Verified bundle and checksums |
| `.spike/` | About 11 MB plus logs | Test copy, logs and run scripts |

Total in `~/.secbox` is 648 MB. To remove everything, delete `~/.secbox`, `.tools/` and `.spike/`.
