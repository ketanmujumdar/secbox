# What secbox protects against

This page lists each protection, the real attack it answers, and the test that
proves it. It is written to be quoted, so every claim carries its status. Copy
the status along with the claim.

| Status | Meaning |
| --- | --- |
| **Ran on a real VM** | Seen working inside a real microVM on a Mac |
| **Tested in code** | Proved by automated tests against the fake engine. The rule exists and is enforced by the daemon, but nothing has booted a VM to show the end result |
| **Not yet** | Designed, not built |

secbox is an early build. Until the rows below say "Ran on a real VM", do not
rely on it to contain untrusted code. `docs/NEEDS_A_VM.md` lists what still
has to run on real hardware.

## The short version

- **Your keys never touch disk, git or a command line.** They live in the OS
  keychain, travel into the sandbox over a pipe, sit in RAM only, and go only
  to the services you name.
- **One hostile container cannot take another's keys.** A project with
  secrets will not start if any service could reach outside its container.
- **You can see exactly where a key could go.** Every plan and `status` list
  each secret, who holds it, and every host the sandbox can reach.
- **Keys are hidden in logs.** Printed, base64'd, URL-encoded, hex'd, or cut
  into a DNS name, they show as `[NAME]`.
- **Nothing a sandbox writes reaches your machine unseen.** The VM works on a
  copy; a planted AI-assistant hook stays in the VM until a reviewed export.

## Real attacks, and what stops them

Techniques are taken from published campaigns. `tests/fixtures/hostile-demo`
reproduces each one with a fake token; its `without-secbox.yaml` runs the same
attack in plain Docker for comparison, where seven of them succeeded.

| Attack | Seen in | Protection | Status |
| --- | --- | --- | --- |
| Install script reads tokens from the environment and posts them out | Shai-Hulud (2025), Keyv / ChainDrop (Aug 2026) | Network deny-by-default; the key only reaches the hosts listed under "Where secrets could go" | Denied hosts over HTTP and HTTPS were blocked on a real VM, from nested containers too (spike 0001). With secbox delivering the secret: **tested in code** |
| Sweep the disk for tokens (TruffleHog) | Shai-Hulud | Keys are never in the project or its snapshot; `.env` files are left out of the snapshot | **Tested in code** |
| Ask the cloud metadata service for credentials | LiteLLM on PyPI (Mar 2026) | Metadata addresses are blocked by the engine | The gateway denied `169.254.169.254` on a real VM (spike 0001). There was no real metadata service to prove the opposite, so that is the evidence |
| Send the key inside a DNS lookup | Common exfiltration | DNS is answered only for allowed names; refused names are recorded, with the key masked | Refusal: ran on a real VM (spike 0001). Recording: ran on a real VM. Masking: **tested in code** |
| Post the key to a raw IP | Common exfiltration | Raw IPs are blocked by the engine | A raw UDP address (`1.1.1.1`) was blocked on a real VM. Raw TCP to a public IP has not been tested: **not yet proven** |
| Plant a hook for Claude, Cursor or VS Code to run later | s1ngularity / Nx (2025), testmetest (Oct 2026) | The VM gets a copy of the project and can't write to yours; `.claude/`, `.vscode/`, `CLAUDE.md`, workflows and install scripts are flagged on review | Copy: by design. `secbox diff` flags the hook first, and exits 5: **tested in code** |
| A sidecar reads another container's env via the Docker socket | Container escape | Refused at plan time when the project has secrets | **Tested in code** |
| A sidecar reads `/proc` with `pid: host`, or goes `privileged` | Container escape | Refused at plan time when the project has secrets | **Tested in code** |
| A sidecar mounts the VM's `/run` where secrets are delivered | Container escape | Refused at plan time when the project has secrets | **Tested in code** |
| A service prints its key, or an encoded form of it | Careless code, or an attacker using logs as a channel | `logs` and `net blocked` show `[NAME]` instead | **Tested in code** |
| A take-home "coding assignment" runs code on the engineer's machine as soon as it is set up | Binance, $6.3B (Oct 2026), the step that put the attacker on a second engineer's laptop | The project runs only in the VM. `check` and the plan read its files as data. The host export never reads the project's own `.git/config`, so a filter driver planted there (`[filter "x"] smudge = <cmd>` plus `* filter=x`) does not run when `up` snapshots the folder | `a_filter_driver_in_the_repo_config_does_not_run_on_snapshot`: **tested in code**. Before this, `up` on such a folder ran the command on the host |
| Malware in the app's own environment reaches the daemon | Host-side token theft | The desktop app starts the daemon with eight named variables and a fixed `PATH` | **Tested in code** |

## Each protection, and its proof

### Secrets

| Claim | Proof | Status |
| --- | --- | --- |
| Values are stored only in macOS Keychain or Linux Secret Service, never in secbox's files | `crates/secrets`, `crates/cli/tests` (`set_stores_the_value_and_never_shows_or_writes_it`) | Tested in code; Keychain on a real Mac |
| A value never appears in any command line, on the host or in the VM | `a_secret_reaches_the_sandbox_over_standard_input_and_never_in_an_argument` | Tested in code |
| In the VM, values live on a RAM-only `tmpfs`; if it can't be made, the start is refused | `delivery_stops_when_there_is_nowhere_safe_to_put_a_secret` | Tested in code |
| Each service gets only the secrets assigned to it | `an_environment_secret_is_written_as_one_line_for_the_named_service_only`, `the_runnable_demo_starts_and_its_secret_goes_only_to_api_over_standard_input` | Tested in code |
| The audit log and every plan name secrets, never values | `the_host_records_which_secret_went_where_by_name_only`, `a_plan_names_a_secret_and_its_route_but_never_its_value` | Tested in code |
| A project with only a Dockerfile gets its secrets the same way | `a_dockerfile_projects_secret_reaches_its_one_service` | Tested in code |

### Isolation between services

| Claim | Proof | Status |
| --- | --- | --- |
| With secrets assigned, `privileged`, host namespaces, extra capabilities, devices, Docker socket and VM mounts are blockers with no override | `a_service_that_could_reach_another_services_secret_stops_the_start` | Tested in code |
| Without secrets, the same settings are shown as findings, since they reach only the disposable VM | `the_same_settings_without_secrets_are_findings_and_not_blockers` | Tested in code |

### Network

| Claim | Proof | Status |
| --- | --- | --- |
| Deny by default; the engine allows only listed names, and never `allow@dns` | `create_allowlist_adds_rules_and_no_dns_rule` | Allowlist ran on a real VM (spike 0001) |
| Published ports bind to `127.0.0.1` only, and only after approval | `create_never_binds_beyond_loopback`, `a_published_port_is_opened_only_from_loopback_under_a_closed_network` | Tested in code; loopback seen on a real VM |
| Build hosts come from a fixed menu, chosen by the project's files; a repo can't name its own | `crates/daemon/src/buildhosts.rs` tests, `a_node_project_gets_the_npm_registry_and_adding_it_moves_the_digest` | Tested in code |
| Run-time hosts come only from approvals stored on the host, never from the repo | `the_runnable_demo_starts_and_its_secret_goes_only_to_api_over_standard_input` | Tested in code |
| Every plan and `status` list each secret, who holds it, every reachable host, and which of those accept uploads from anyone | `proto` exposure tests, `the_plan_says_where_a_secret_could_be_sent_including_build_hosts` | Tested in code |

### Logs and visibility

| Claim | Proof | Status |
| --- | --- | --- |
| Terminal control codes from a sandbox are removed before display | `proto::text` tests | Ran on a real VM |
| Secret values are masked in logs, raw, base64 at any alignment, URL-encoded and hex, even split across reads | `crates/daemon/src/mask.rs` tests, `a_secret_printed_by_a_service_is_masked_in_logs_even_split_or_encoded` | Tested in code |
| A key cut into a hostname label is masked in `net blocked` | `a_token_cut_into_a_hostname_label_is_hidden` | Tested in code |

### Reviewing what a sandbox changed

| Claim | Proof | Status |
| --- | --- | --- |
| `secbox diff`, the Review panel and the desktop Git review page show every file a sandbox changed, compared with a baseline commit the host made before anything ran | `crates/daemon/tests/diff.rs` | Tested in code |
| A hook planted for Claude or Cursor is flagged first, as code that would run on your machine; workflows, git hooks, editor settings and install scripts are flagged too, including case and Unicode lookalikes | `crates/daemon/tests/diff.rs`, `crates/gitbroker/tests/hostile.rs` | Tested in code |
| What comes out of the sandbox is hostile data: git on the host never runs in a folder the VM wrote, nothing from it is run or checked out, and a malformed, oversized or tricked bundle is refused | `crates/daemon/tests/diff.rs`, `crates/gitbroker/tests/hostile.rs` | Tested in code |

### Host safety

| Claim | Proof | Status |
| --- | --- | --- |
| `shell` and `exec` run the engine with an emptied environment | `the_engine_gets_a_cleared_environment_and_a_fixed_argument_list`, `shell_runs_the_engine_with_a_cleared_environment_and_prints_the_notice_first` | Tested in code; ran on a real VM |
| The desktop app passes the daemon only the variables it reads | `the_daemon_gets_only_the_variables_it_reads` | Tested in code |
| The desktop app opens no local web port and shows sandbox text only as plain text | `apps/desktop/src-tauri/capabilities/main.json`, CSP in `tauri.conf.json` | By design; ran under Xvfb on Linux |
| Starting anything needs the plan's digest, so what starts is exactly what was shown | `crates/daemon/tests/service.rs` digest tests | Tested in code. Any process running as you can fetch and send that digest, so on its own it does not prove a person agreed |
| On macOS, starting a sandbox, allowing a hostname and each relayed push wait for the person in the system's own prompt (Touch ID or login password), put up by the daemon and naming exactly what will happen. Declining does nothing: no sandbox, no saved host, and no bytes to the remote | `a_start_the_person_declines_creates_nothing`, `allowing_a_host_the_person_declines_saves_nothing`, `a_push_the_person_declines_sends_the_remote_nothing`, `a_push_typed_as_a_git_command_is_asked_about_too` | Tested in code with a stand-in person. The macOS prompt is compiled and linted for macOS but **has never run on a Mac** |

## What it does not protect against yet

Say these alongside any claim above.

### Measured against the Binance attack (Oct 2026)

Binance's report describes: a fake Homebrew site from a search ad; a fake job
interview whose assignment ran on setup; an internal AI tool used to reach two
more engineers; a container-deployment token taken from one of their laptops;
and a container that asked the wallet signing service to sign. secbox answers
only the assignment step, and only if the assignment never touches the host
first. Once code runs as you on your machine, secbox does not stop it:

- **Code running as you can still go around the daemon.** Starting, `net
  allow` and pushes now wait for Touch ID on a Mac, which stops a tricked AI
  agent or a script that uses secbox's own API. But such code can run the
  engine itself to `exec` into a running sandbox and read the secrets
  delivered there, use your SSH agent directly, or replace secbox's programs.
  `shell` and `exec` are not asked about, since they go to the engine anyway.
  On Linux nothing is asked yet. Status: **partly done**.
- **Cloning or unzipping the assignment on the host, then opening it in an
  editor, runs before secbox is involved.** Use `secbox new git@…` instead: it
  fetches into secbox's own state folder, where git never runs a checkout,
  hook or the remote's config, and nothing opens the files. `secbox open` checks automatic tasks and
  Workspace Trust, but it opens the host copy: an editor extension or language
  server in a trusted folder can still run project code (rust-analyzer runs
  `build.rs`, Python tools run the project's interpreter). Status: **not yet**.
- **A deploy or signing credential in a sandbox is readable by every
  dependency of the service that gets it**, and can be used directly against
  the host it is meant for, which the allowlist has to permit. Binance lost a
  single container-deployment token. Do not give a sandbox production deploy,
  wallet or signing credentials. Proxy-injected secrets, where the value never
  enters the VM, are refused for now.
- **A push does not consult `diff`'s flags.** On a Mac every push, including
  an AI agent's `git_push`, now needs your fingerprint, and the prompt names
  the branch and commit. It does not yet say that the commit adds a CI
  workflow or an AI tool hook, so read `secbox diff` before you agree. Status:
  **partly done**.
- **secbox itself has no signed release.** The Homebrew formula is a
  placeholder. A fake download site is how the Binance attack began, so a
  release needs published checksums and a signature before anyone is told
  where to install it from. Status: **not yet**.

### Other gaps

- **Build hosts stay open while the project runs.** The engine fixes a
  sandbox's rules when it is made. A service holding a key can reach the build
  hosts its project uses, and Docker Hub accepts pushes to any account. The fix
  is a separate build sandbox with no secrets. Status: **not yet**.
- **A service can read its own secrets.** Masking stops accidental leaks into
  logs, not a thief inside that service. The network rules are what limit
  where a stolen key can go.
- **`shell` and `exec` output is not masked**, because it goes straight to
  your terminal.
- **Export and approval by fingerprint** are Milestone 3. `diff` shows and flags changes; nothing takes them out of the sandbox yet. Status: **not yet**.
- **`diff` needs `git` inside the VM image**, which has not been checked on a real VM.
- **Blocked raw IP, metadata and UDP attempts are blocked but not recorded**,
  so an empty `net blocked` list proves nothing.
