# Logs, shell and exec

Three commands let you look inside a running sandbox: `logs` reads what services printed, `shell` opens an interactive shell, and `exec` runs one command. None of them starts a stopped sandbox. They refuse and tell you how to start it.

```sh
secbox logs                     # the last 200 lines from each service
secbox logs api -f              # follow one service; Ctrl-C stops following only
secbox shell                    # a shell in the VM, in /work
secbox shell postgres           # a shell in the postgres container
secbox exec -- ls -la           # one command, with its exit code passed back
secbox exec --service api -- env
```

Add `--project PATH` to any of them to name a project in another folder.

## logs

```sh
secbox logs [SERVICE] [-f] [--tail N] [--project PATH]
```

| Option | Default | What it does |
| --- | --- | --- |
| `SERVICE` | all | One Compose service |
| `-f`, `--follow` | off | Keep showing new lines until Ctrl-C. Ctrl-C stops following; the sandbox keeps running |
| `--tail N` | 200 | How many recent lines to show from each service |

Terminal control codes are removed, so a service cannot clear or take over your terminal through its log.

### Secret values are masked

`logs` replaces the value of each secret assigned to the project with its name, such as `[DEMO_TOKEN]`, before you see it. That covers the value as it is, in base64 at any alignment, URL-encoded and in hex, even when it is split across reads.

> Masking stops accidental leaks into logs, not a thief. A value that has been encrypted or reworked cannot be recognised. `shell` and `exec` run on your own terminal, so their output is **not** masked.

## shell

```sh
secbox shell [SERVICE] [--project PATH]
```

With no service, opens a shell in the VM itself, in `/work`, where the project is. With a service name, runs `docker compose exec SERVICE sh` inside the VM.

The shell uses your real terminal, so control codes cannot be filtered there. `exit N` leaves it, and secbox exits with the same code.

Anything you paste into the shell is inside the sandbox. Typing into a shell writes a history file inside the sandbox, as any shell does.

## exec

```sh
secbox exec [--service SERVICE] [--project PATH] -- COMMAND...
```

Runs one command and passes its exit code back. Everything after `--` is the command.

```sh
secbox exec -- sh -c 'pwd; exit 7'; echo $?      # prints /work, then 7
secbox exec --service postgres -- hostname
secbox exec -- npm install
```

Your editor's built-in terminal runs on your machine, not in the sandbox. Run project commands such as `npm install` or test suites through `secbox exec`.

## How these reach the VM

There is no SSH server and no open port in the VM. The daemon checks that the sandbox is running and records the request in its audit log, then tells the CLI where the engine is. The CLI runs the engine with a **cleared environment**, so none of your environment variables, tokens or keys cross into the sandbox.

## In the full-screen interface

Press `l` for the selected project's Logs tab. It follows new lines by itself. Scrolling up pauses it, and `f` or End follows again. Press `e` on Overview or Logs to open the same shell as `secbox shell`. See [Full-screen terminal interface](terminal-ui.md).
