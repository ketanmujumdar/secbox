# Troubleshooting

Common problems and how to fix them. Start with `secbox doctor`, which checks most of what can go wrong on the host and says what to do next.

## Installing

| Symptom | Fix |
| --- | --- |
| Build fails in `libdbus-sys` with "pkg-config" or "dbus-1" | Install the D-Bus headers: `sudo apt install pkg-config libdbus-1-dev`, or `sudo dnf install pkgconf-pkg-config dbus-devel` |
| `secbox: command not found` after `cargo install` | Add `~/.cargo/bin` to your `PATH`. With asdf, run `asdf reshim rust` |

## doctor

| Line | Fix |
| --- | --- |
| `Hypervisor fail /dev/kvm does not exist` | Enable KVM. A cloud VM needs nested virtualization turned on |
| `Socket path fail` | The state folder's path is too long for a Unix socket. Set `SECBOX_HOME` to a short folder, such as `/tmp/sbtest` |
| `Engine warn not found` | Install `msb` at `~/.secbox/msb/bin/msb`, or start the daemon with `SECBOX_ENGINE_BIN=/abs/path/to/msb` |
| `Memory warn` or `Disk warn` | A typical Compose project needs a 3 GB or larger VM and about 30 GB of disk. Free some up, or give the project smaller limits |

## The daemon

| Symptom | Fix |
| --- | --- |
| `the secbox daemon is not running` | Start `secboxd` in another terminal, or open the desktop app |
| A command says the daemon is older than this command | Restart `secboxd`. Its sandboxes keep running |
| The daemon runs but the CLI cannot find it | Both must see the same `SECBOX_HOME`. If one has it set and the other does not, they look in different folders |

## Starting a project

| Symptom | Fix |
| --- | --- |
| `this folder is not the top folder of a Git repository` | `up` starts from a commit, and only a repository's top folder has one. Start that folder, or copy this one into a repository of its own: `git init`, then commit. A folder inside a repository, such as `tests/fixtures/clean`, is refused |
| `this repository has no commits yet` | Make a first commit |
| `including uncommitted changes is not built yet` | Commit your changes, then run `up` again |
| `This cannot be started yet`, with a memory, CPU or disk line | The host would be left with too little. Free resources, or lower the request with `secbox.yaml` |
| A blocker about a host mount, the Docker socket or `privileged` | The project has secrets assigned, so privileged services are refused. Remove the setting, or remove the secret assignments |
| `a secret is held by the proxy` | Secrets set with `--proxy-host` are not supported yet. Remove it with `secbox secrets rm NAME` and set it again without `--proxy-host` |
| `up: this project does not publish 9000` | `--port` accepts only ports in the plan. Run `secbox up --plan-only` to see them |
| `unexpected argument '--project'` | `up`, `down` and `status` take the folder as a plain argument: `secbox up ~/code/myapp` |
| A service cannot reach a website | Only allowed hostnames resolve. Check `secbox net blocked` for the refused name |
| `not confirmed with Touch ID or your login password, so nothing was done: you cancelled (LocalAuthentication -2)` | The Touch ID prompt was cancelled. Run the command again and confirm, if it was you who asked |
| `… there is no screen to ask on, such as over SSH` | The daemon can't show a prompt without a logged-in screen. Run it from the Mac itself |
| `… Touch ID is locked` | Unlock the Mac with its password once, then try again |
| `… nobody answered within 120 seconds` | The prompt was withdrawn. Try again |
| A Touch ID prompt you did not expect | Something else on your machine asked. Cancel it, then look at `daemon-audit.jsonl` in the state folder for `person_declined` |

## Logs, shell and exec

| Symptom | Fix |
| --- | --- |
| Refused because the sandbox is stopped | Start it with `secbox up`. These commands never start a sandbox |
| `no such service` | Use a service name from `secbox status` |
| `net blocked` says nothing is being recorded | The sandbox was created before recording existed. Recreate it with `secbox down --destroy`, then `secbox up`. This deletes its data |

## Reviewing changes

| Symptom | Fix |
| --- | --- |
| `there is no record of what this sandbox started from` | The sandbox was created before `diff` existed. Recreate it with `secbox down --destroy`, then `secbox up`. This deletes its data |
| `diff` exits 5 | Something was flagged, or the review stopped at a limit. Read the `FLAG` lines |
| `the sandbox is not running, and the comparison runs inside it` | Start it with `secbox up`. `diff` never starts one |
| `the sandbox's history does not start from the snapshot` | Something in the sandbox rewrote `/work/.git`, so the list may be incomplete. Treat the sandbox as suspect |

## Secrets

| Symptom | Fix |
| --- | --- |
| `the hidden prompt needs a terminal` | Run `secrets set` in a real terminal, not through a pipe |
| macOS asks for keychain access | Expected the first time a secret is read for delivery. `secrets list` never asks |
| On Linux, storing fails | A Secret Service provider, such as GNOME Keyring, must be running and unlocked |

## AI agents

See the [troubleshooting table on the AI agents page](guides/ai-agents.md#troubleshooting).

## Still stuck

Open an issue at [github.com/ketanmujumdar/secbox/issues](https://github.com/ketanmujumdar/secbox/issues) with the output of `secbox doctor` and the command that failed. Remove anything secret first. secbox never prints secret values, but your project's own output might.
