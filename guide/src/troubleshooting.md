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
| `this project is not a git repository` | `up` starts from a commit. Run `git init` and commit |
| `this repository has no commits yet` | Make a first commit |
| `including uncommitted changes is not built yet` | Commit your changes, then run `up` again |
| `This cannot be started yet`, with a memory, CPU or disk line | The host would be left with too little. Free resources, or lower the request with `secbox.yaml` |
| A blocker about a host mount, the Docker socket or `privileged` | The project has secrets assigned, so privileged services are refused. Remove the setting, or remove the secret assignments |
| `a secret is held by the proxy` | Secrets set with `--proxy-host` are not supported yet. Remove it with `secbox secrets rm NAME` and set it again without `--proxy-host` |
| `up: this project does not publish 9000` | `--port` accepts only ports in the plan. Run `secbox up --plan-only` to see them |
| `unexpected argument '--project'` | `up`, `down` and `status` take the folder as a plain argument: `secbox up ~/code/myapp` |
| A service cannot reach a website | Only allowed hostnames resolve. Check `secbox net blocked` for the refused name |

## Logs, shell and exec

| Symptom | Fix |
| --- | --- |
| Refused because the sandbox is stopped | Start it with `secbox up`. These commands never start a sandbox |
| `no such service` | Use a service name from `secbox status` |
| `net blocked` says nothing is being recorded | The sandbox was created before recording existed. Recreate it with `secbox down --destroy`, then `secbox up`. This deletes its data |

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
