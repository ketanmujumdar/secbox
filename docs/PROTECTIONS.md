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

- **Your keys stay out of git and command lines.** They live in the OS
  keychain, travel into the sandbox over a pipe, and go only to the services
  you name. They do reach a disk: Docker in the VM writes a service's
  environment, and whatever it prints, to the VM's own disk, which is a file
  on your machine. See "Secret values on the host's disk" below.
- **One hostile container cannot take another's keys.** A project with
  secrets will not start if any service could reach outside its container.
- **You can see exactly where a key could go.** Every plan and `status` list
  each secret, who holds it, and every host the sandbox can reach.
- **Keys are hidden in logs.** Printed, base64'd, URL-encoded, hex'd, or cut
  into a DNS name, they show as `[NAME]`.
- **A start that fails leaves nothing open.** If Compose cannot build or start
  the project, the sandbox is stopped, which closes its ports and drops the
  secrets it was given; its data is kept. The audit log records
  `build_failed` with Compose's exit code, and the project's Problems tab
  lists it. Compose's last lines are shown masked, one per line.
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
| Send the key inside a DNS lookup | Common exfiltration | DNS is answered only for allowed names; refused names are listed with the key masked, and kept in the daemon's memory only | Refusal: ran on a real VM (spike 0001). Recording: ran on a real VM. Masking, and keeping names off the disk: **tested in code** |
| Post the key to a raw IP | Common exfiltration | Raw IPs are blocked by the engine | A raw UDP address (`1.1.1.1`) was blocked on a real VM. Raw TCP to public IPs was tried by hand on a real Mac with engine 0.7.13: ports 22, 25, 443, 853 and 8000 were refused. HTTP on port 80, and TCP on 53, does connect, but to the engine's own network layer, which answers 403 or nothing; it does not reach the address. Not a repeatable test yet: **not yet proven** |
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
| In the VM, values are delivered to a RAM-only `tmpfs`; if it can't be made, the start is refused | `delivery_stops_when_there_is_nowhere_safe_to_put_a_secret` | Tested in code. Docker's copies are kept in memory too: see "Secret values on the host's disk" |
| Each service gets only the secrets assigned to it | `an_environment_secret_is_written_as_one_line_for_the_named_service_only`, `the_runnable_demo_starts_and_its_secret_goes_only_to_api_over_standard_input` | Tested in code |
| The audit log and every plan name secrets, never values | `the_host_records_which_secret_went_where_by_name_only`, `a_plan_names_a_secret_and_its_route_but_never_its_value` | Tested in code |
| A project with only a Dockerfile gets its secrets the same way | `a_dockerfile_projects_secret_reaches_its_one_service` | Tested in code |
| A service not ticked for a secret gets no environment variable, no `/run/secrets` file and no line in the override for it | `a_secret_ticked_for_web_only_never_reaches_api`, `a_file_secret_moved_to_web_is_mounted_in_web_and_not_in_api`, `unticking_every_service_stops_delivery_but_keeps_the_value` | Tested in code. The daemon's real output also ran in real containers with Docker 29.9 and Compose 5.6, outside a VM (Nov 7, 2026) |
| A Dockerfile-only project offers its one service, refuses any other, and its secrets never reach the build or the image | `a_dockerfile_project_offers_its_one_service_and_unticked_means_nowhere`, `a_dockerfile_projects_secret_is_never_part_of_the_build` | Tested in code; also ran in a real container, outside a VM: `RUN env` saw no secret and the image's own environment held none |
| A template such as `.env.example` adds names only; its values are dropped unread and the keychain is not opened | `a_template_adds_empty_names_and_drops_its_values`, `a_template_never_hides_a_stored_value` | Tested in code |
| Removing a project deletes its secrets from the keychain, by the names in its records and any more the keychain lists under its id, without reading a value; one that cannot be deleted is named and audited (`secrets_left`). `down --destroy` keeps them. `secrets prune` finds those left by projects already gone | `removing_a_project_deletes_its_secrets_from_the_keychain`, `destroying_a_sandbox_keeps_the_projects_secrets`, `a_secret_that_cannot_be_deleted_is_named_and_the_project_still_goes`, `prune_deletes_only_what_projects_no_longer_here_left_and_asks_first` | Tested in code; the attribute-only listing was run against the real Keychain, without a prompt |
| The desktop window can set a value but never read one. It has no keychain access: the daemon stores the value, checks the name and that each service is in the project's Compose file, and never repeats a bad name | `set_secret_stores_the_value_assigns_it_and_fills_an_empty_name`, `set_secret_refuses_without_storing_and_never_repeats_a_bad_name`, `the_window_lists_names_and_stores_a_secret_through_the_daemon` | Tested in code; the window was seen only in a browser preview |
| A value sent by a client never shows in debug output | `a_secret_value_never_shows_in_debug_output` (`proto`) | Tested in code |

### Isolation between services

| Claim | Proof | Status |
| --- | --- | --- |
| With secrets assigned, `privileged`, host namespaces, extra capabilities, devices, Docker socket and VM mounts are blockers with no override | `a_service_that_could_reach_another_services_secret_stops_the_start` | Tested in code |
| With secrets assigned, a service that shares another's mounts or namespaces (`volumes_from`, `pid`, `ipc` or `network_mode: service:...`) stops the start | `a_service_that_shares_web_s_mounts_or_processes_stops_the_start` | Tested in code |
| Without secrets, the same settings are shown as findings, since they reach only the disposable VM | `the_same_settings_without_secrets_are_findings_and_not_blockers` | Tested in code |

### Network

| Claim | Proof | Status |
| --- | --- | --- |
| Deny by default; the engine allows only listed names, and never `allow@dns` | `create_allowlist_adds_rules_and_no_dns_rule` | Allowlist ran on a real VM (spike 0001) |
| Published ports bind to `127.0.0.1` only, and only after approval | `create_never_binds_beyond_loopback`, `a_published_port_is_opened_only_from_loopback_under_a_closed_network` | Tested in code; loopback seen on a real VM |
| A port opened or moved later still binds to `127.0.0.1` only, changes nothing until a start the person confirms, and that start's prompt names it | `set_ports_removes_by_host_number_and_adds_on_loopback`, `a_port_moved_to_another_number_is_forwarded_there_after_a_restart`, `a_stopped_sandbox_opens_a_port_when_it_starts_again` | Tested in code with a fake engine; not yet run on a real VM |
| A port can be opened later only on a sandbox whose engine rule lets it in from the host's loopback; one made before this cannot open new ports | `a_new_sandbox_lets_in_every_planned_port_so_any_can_open_later`, `a_sandbox_from_before_ports_could_change_cannot_open_new_ones` | Tested in code |
| Build hosts come from a fixed menu, chosen by the project's files; a repo can't name its own | `crates/daemon/src/buildhosts.rs` tests, `a_node_project_gets_the_npm_registry_and_adding_it_moves_the_digest` | Tested in code |
| Run-time hosts come only from approvals stored on the host, never from the repo | `the_runnable_demo_starts_and_its_secret_goes_only_to_api_over_standard_input` | Tested in code |
| An approved host can be taken back: the sandbox is replaced without it, data kept, a stopped one stays stopped, and nobody is asked because it only narrows | `blocking_an_approved_host_again_replaces_the_sandbox_without_it_and_asks_nobody`, `a_stopped_sandbox_is_replaced_without_the_host_and_stays_stopped` | Tested in code with the fake engine |
| Every plan and `status` list each secret, who holds it, every reachable host, and which of those accept uploads from anyone | `proto` exposure tests, `the_plan_says_where_a_secret_could_be_sent_including_build_hosts` | Tested in code |

### Logs and visibility

| Claim | Proof | Status |
| --- | --- | --- |
| Terminal control codes from a sandbox are removed before display | `proto::text` tests | Ran on a real VM |
| Secret values are masked in logs, raw, base64 at any alignment, URL-encoded and hex, even split across reads | `crates/daemon/src/mask.rs` tests, `a_secret_printed_by_a_service_is_masked_in_logs_even_split_or_encoded` | Tested in code |
| A key cut into a hostname label is masked in `net blocked` | `a_token_cut_into_a_hostname_label_is_hidden` | Tested in code |

### Secret values on the host's disk

On Nov 8, 2026, after the hostile demo ran on a real Mac with the fake token
`fake-token-1234`, the token was found in three files under the state folder:
the engine's record of every `exec`'s output, the engine's debug log (cut
into a refused DNS name), and the daemon's own list of refused names. A
sandbox chooses every byte of what it prints and looks up, so masking known
encodings cannot be relied on there. These are the changes:

| Claim | Proof | Status |
| --- | --- | --- |
| The engine keeps no copy of what `exec` prints. Engine 0.7.13 has no setting for this, so a folder is put where its file would be; it then writes nothing and `exec` works as before. Whatever the file held is cleared at the next start | `sealing_clears_the_captured_output_and_leaves_nothing_to_write_it_to`, `start_seals_the_logs_before_the_engine_runs` | Tested in code. The engine's side ran on a real VM by hand, with the engine alone and not through the daemon: `exec`, `exec --stream` with input, stop and start, and `remove` all worked, and its log said "capture disabled" |
| The engine keeps no copy of the guest's console, which root in the guest can write to. It points at `/dev/null`; a folder there stops the VM booting | the same tests | Tested in code. Ran on a real VM by hand, as above |
| Refused names are read from the engine's debug log every 5 seconds, and what was read is overwritten with zeros, renamed copies included | `a_name_the_guest_looked_up_is_listed_but_kept_on_no_disk` | Tested in code. Overwriting a live log ran on a real VM by hand: the engine writes in append mode and carried on after the zeros |
| The daemon's own record, `blocked-seen.json`, holds counts and positions, never a name. Names are kept in its memory, so a restart loses them, and the list then says how many it can no longer show. A record from an older version has its names dropped the first time it is read | `after_a_restart_the_names_no_longer_shown_are_counted`, `a_record_from_an_older_version_loses_its_names` | Tested in code |
| The engine's log files are made readable by their owner only as they are read, and the daemon starts the engine with a umask of `077` | `zero_ranges` in `crates/daemon/src/blocked.rs`, `crates/daemon/src/main.rs` | Not tested on its own. On a real VM the engine made its renamed logs `0666` whatever the umask |

What this does not cover is listed under "What it does not protect against
yet".

### Reviewing what a sandbox changed

| Claim | Proof | Status |
| --- | --- | --- |
| `secbox diff`, the Review panel and the desktop Git review page show every file a sandbox changed, compared with a baseline commit the host made before anything ran | `crates/daemon/tests/diff.rs` | Tested in code |
| A hook planted for Claude or Cursor is flagged first, as code that would run on your machine; workflows, git hooks, editor settings and install scripts are flagged too, including case and Unicode lookalikes | `crates/daemon/tests/diff.rs`, `crates/gitbroker/tests/hostile.rs` | Tested in code |
| What comes out of the sandbox is hostile data: git on the host never runs in a folder the VM wrote, nothing from it is run or checked out, and a malformed, oversized or tricked bundle is refused | `crates/daemon/tests/diff.rs`, `crates/gitbroker/tests/hostile.rs` | Tested in code |
| Two paths that differ only in case, such as `.claude/settings.json` and `.CLAUDE/Settings.json`, are both bundled and both flagged. A snapshot made on a Mac used to tell git in the guest to ignore case, and then the bundle failed and nothing was reviewed | `paths_that_differ_only_in_case_are_bundled_whatever_the_repository_says`, `two_settings_files_that_differ_only_in_case_are_both_flagged`, `the_repository_given_to_the_guest_does_not_ignore_case` | The failure was seen on a real VM. The fix is tested in code, with real git on a case-sensitive folder. Not yet run on a real VM |
| If the bundle cannot be made or read, for any reason, `diff` says "the review could not run, so treat everything the sandbox changed as unreviewed" and exits 1 | `a_bundle_the_guest_cannot_make_is_reported_as_not_reviewed` | Tested in code |

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
- **The VM's own disk held secret values.** Docker in the VM writes each
  service's environment, secrets included, into `config.v2.json`, the runtime
  bundle under `/run/docker` and containerd's metadata database, all on the
  VM's disk, which is `rootfs.raw` in the engine's folder on the host. Seen on
  a real Mac on Nov 8, 2026. Since Nov 9 those folders, and container logs,
  are kept on a `tmpfs` in the VM, mounted before `dockerd` starts on every
  boot; Docker runs with its classic image store so containerd's database
  holds only containers; and what earlier starts left there is deleted and the
  freed blocks discarded, in the kept disk `previous-root.raw` too (now mode
  `0600`). On a throwaway engine sandbox a fake value set by `docker run -e`
  and by a Compose `env_file` was then in no disk file, across a reboot, and
  two earlier values were gone. Through the daemon on a real Mac on Nov 9,
  with two fake secrets delivered to Compose services, `LC_ALL=C grep -a -rl`
  found neither value, nor the form the DNS probe sends, anywhere under
  `~/.secbox`: after a start, after a stop and start, after approving a host
  and after blocking it again. A sandbox made before this pulls or builds its images once more,
  and container logs no longer survive a restart. What an app writes into its
  own files or volumes is still on the disk.
- **A refused name is on the disk for up to 5 seconds** while the daemon runs,
  and for as long as the daemon is not running, since the engine needs debug
  logging to record it at all. Zeros are written over it; the disk blocks it
  used are not wiped, so disk encryption (FileVault) is what covers those.
- **The first start of a new sandbox** writes `exec` output to the engine's
  capture file until that sandbox stops. The file loses its name the moment
  `create` returns, before anything from the project runs, so nothing can open
  it, and it is gone when the engine closes it.
- **Sandboxes made before this change** keep their old `exec` capture until
  they are next started, and their old log files until the daemon's next pass.
- **The engine's job folders** (`jobs-v1`, owner-only) keep the output of
  `dockerd`, which runs at its default level. Whether a service can get text
  of its choosing into that has not been checked.
- **`shell` and `exec` output is not masked**, because it goes straight to
  your terminal.
- **Export and approval by fingerprint** are Milestone 3. `diff` shows and flags changes; nothing takes them out of the sandbox yet. Status: **not yet**.
- **`diff` needs `git` inside the VM image**, which has not been checked on a real VM.
- **Blocked raw IP, metadata and UDP attempts are blocked but not recorded**,
  so an empty `net blocked` list proves nothing.
- **A raw-IP connection on port 80 or TCP 53 succeeds inside the sandbox.** It
  reaches the engine's own network layer, not the address, and gets a 403 or
  no answer. A probe that only checks whether it connected would call this a
  leak. Other ports tried (22, 25, 443, 853, 8000) were refused, on a real Mac
  with engine 0.7.13.
