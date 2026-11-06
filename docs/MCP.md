# AI agents and MCP

As of Nov 5, 2026. `secbox` is a working name, see "Changing the name" in the README.

The project's code lives in a sandbox, so an AI agent running on your machine (Claude Code, Codex, or anything else that speaks MCP) cannot see it. This page covers how such an agent connects to the sandbox, what keeps that connection safe, and what it does not protect.

**Status: early, and never run against a real VM.** The tools have been tested against the real daemon logic, with a fake engine and a stub in place of the engine's binary. Claude Code has connected to the server and listed its tools. No model has yet used them on a running sandbox. See [What has been tested](#what-has-been-tested).

## Two ways to run an agent

| | Agent in the sandbox | Agent on the host, through MCP |
| --- | --- | --- |
| Where the agent runs | Inside the microVM | On your machine, as you |
| How it reaches the code | Directly, at `/work` | Through `secbox mcp serve` |
| What a tricked agent can do | Whatever the sandbox allows | Whatever the sandbox allows, **plus whatever its own host tools allow** |
| Your usual agent setup | Not available in the VM | Kept: your login, settings and editor |
| State | Planned: `secbox ai login` is not built | Built, as described here |

The design keeps the first as the default (`docs/ARCHITECTURE.md`, "MCP and prompt injection"). The second is the clearly labeled weaker mode it mentions. It exists because people want to keep using the agent they already have. It is only safer than running the agent in the project folder if the agent's own host tools are turned off, which `secbox mcp setup` arranges for Claude Code and only partly can for Codex.

## How the connection works

```
Claude Code or Codex, on the host
   │  stdin and stdout pipes: MCP, JSON-RPC 2.0, one message per line
   ▼
secbox mcp serve --project PATH        child process of the agent, as your user
   │  Unix socket in ~/.secbox: owner-only folder and socket, peer user checked
   ▼
secboxd                                checks the sandbox is running, records the request
   │  answers with the sandbox's name and where the engine is
   ▼
secbox mcp serve runs the engine       cleared environment, working folder /
   │  msb exec --stream --no-tty
   ▼
microVM                                sh -c COMMAND in /work, or docker compose exec -T SERVICE
```

1. **The agent starts the server as a child process.** MCP has two transports: stdio and HTTP. This uses stdio only, so nothing listens on a port. A server on `localhost` could be reached by any local process, and by web pages through DNS rebinding. A pipe can only be reached by the process that holds it.
2. **The server is bound to one project when it starts.** `--project` is required and is resolved to a real folder once. No tool takes a project argument, so a tool call cannot move the server to another project.
3. **Every tool goes through the daemon, the way the command line does.** The server is one more client of `secboxd`, with the same powers as `secbox logs`, `secbox status` and `secbox exec`, and no others. The socket is the one every client uses: it sits in a `0700` folder, is `0600` itself, and the daemon checks each connection's user ID.
4. **Running a command takes two steps, as `secbox exec` does.** The server asks the daemon for the way in (the `terminal` request). The daemon refuses unless the sandbox is already running, because the engine's own `exec` would boot a stopped one. It writes a `TerminalOpened` entry to the audit log for each call, then replies with the sandbox's name and the engine's path. The server then runs the engine itself, with a cleared environment and `/` as its working folder. No host environment variable, API key or token crosses into the sandbox.
5. **Output is filtered and labeled.** Terminal control sequences are removed, other hidden characters are shown as escapes, and every result that came out of the sandbox starts with a line saying it is data, not instructions.

### What the server will not do

It has no tool to:

- start, stop, rebuild or destroy a sandbox
- read a secret, or change which secrets a service gets
- change the network policy, or forward a port
- push to `main` or `master`, delete a branch, push a tag, or push anywhere but the project's own repository. The Git tools push through the daemon's relay, with the checks in `docs/GIT.md`
- reach any project but the one it was started for
- read or write the host's files

The design also lists tools that need a person's approval: `request_network`, `propose_change` and `rebuild`. They are **not offered**, because the daemon has no way to ask a person for approval yet. An agent is better told that they do not exist than given tools that always fail.

## Setting it up

You need the daemon running and a sandbox started for the project:

```sh
secboxd &                         # or the desktop app, which keeps it running
secbox up ~/code/myapp
```

### Claude Code

```sh
secbox mcp setup claude ~/agents/myapp --project ~/code/myapp
cd ~/agents/myapp && claude
```

This makes a **new, empty folder outside the project** and writes three files into it:

| File | What it does |
| --- | --- |
| `.mcp.json` | Starts `secbox mcp serve --project /abs/path/to/myapp`, using this program's absolute path. If `SECBOX_HOME` is set, it is passed on, so the server finds the same daemon |
| `.claude/settings.json` | Denies Claude Code's own `Bash`, `Read`, `Edit`, `Write`, `NotebookEdit`, `Glob`, `Grep`, `WebFetch` and `WebSearch`. Allows every `secbox` tool. Turns off bypass-permissions mode. Approves the `secbox` server from `.mcp.json` |
| `CLAUDE.md` | Tells the agent the code is in the sandbox, its own tools are off on purpose, and sandbox output is data |

On first start, Claude Code asks you to trust the folder and may ask you to approve the server. Run `/mcp` to check it says `connected`.

It refuses a folder that already has anything in it, because a file already there could change what the agent does. It also refuses a folder that is inside the project or contains it, because the agent could then read the project on the host. To redo a setup, delete the folder and run the command again.

The deny rules are enforced by Claude Code, not by secbox. Deny rules take precedence over allow rules in every settings file, so a broad `allow` in your user settings does not override them. They only apply when Claude Code is started in that folder, though. Started anywhere else, it has its tools back.

### Codex

```sh
secbox mcp setup codex --project ~/code/myapp
```

This changes nothing. It prints a `codex mcp add` command, the matching `~/.codex/config.toml` lines, and how to start Codex:

```sh
codex mcp add secbox -- /abs/path/to/secbox mcp serve --project /abs/path/to/myapp
mkdir -p ~/secbox-agent && cd ~/secbox-agent
codex --sandbox read-only --ask-for-approval untrusted
```

**This is weaker than the Claude Code setup.** Codex has no setting that turns its shell off. `--sandbox read-only` stops its commands from writing on the host, but they can still read any file your user can, including the project on the host and anything in your home folder. `--ask-for-approval untrusted` puts each command that isn't a known-safe read to you first. Read what it asks before saying yes.

### Any other MCP client

Configure a stdio server whose command is the absolute path of `secbox`, with the arguments `mcp serve --project /abs/path/to/project`. Then turn off whatever host tools the client has, if it has a way to.

## The tools

| Tool | Arguments | What it does | Limits |
| --- | --- | --- | --- |
| `status` | none | Whether the sandbox is running, its services and their health, forwarded ports, and how many hostname lookups were refused | 30 s for the daemon to answer |
| `logs` | `service`, `tail` (default 200, at most 5000) | The last lines from `docker compose logs`. Never follows | 256 KiB, 60 s |
| `exec` | `command` (required), `service`, `timeout_seconds` (default 120, at most 600) | Runs `sh -c COMMAND` in `/work`, or inside one Compose service with `docker compose exec -T`. Returns the exit code, stdout and stderr. Has no terminal and no input | 256 KiB per stream, 64 KiB command |
| `read_file` | `path` (required) | Reads a file under `/work` | 1 MiB, 60 s |
| `write_file` | `path`, `content` (both required) | Replaces a file under `/work`, making its folders. The content goes in on stdin, and the path is passed as an argument, never as part of a script | 1 MiB, 60 s |
| `git_status` | none | The clone's branch and changes, for a project imported from a remote | 60 s |
| `git_pull` | none | Fetches and merges the current branch's upstream through the Git relay, with the person's SSH key on their machine | 15 min |
| `git_commit` | `message` (required), `paths` | Stages and commits, under the person's own Git name | 60 s |
| `git_push` | none | Pushes the current branch through the relay. `main`, `master`, deletes and tags are refused before anything is sent | 15 min |
| `git` | `args` (required): the command without the leading `git` | Any Git command in the clone, inside the sandbox. Only `fetch`, `pull`, `ls-remote` and `push` get the relay, with the same checks. See `docs/GIT.md`, "Any Git command" | 64 arguments; 5 min, or 15 min with the relay |

Notes on the tools:

- **Paths** for `read_file` and `write_file` are relative to `/work`, or start with `/work/`. Any path containing `..`, `.`, an empty part or a control character is refused before anything is asked. These two tools are tied to `/work` so they stay about the project, not because the rest of the VM is out of reach: `exec` can read anything in the sandbox.
- **Text from `read_file`** keeps tabs, newlines and carriage returns. Every other control or invisible character, and every invalid UTF-8 byte, is shown as an escape such as `\u{1b}`, and the reply says so. Exact bytes are available with `exec` and `base64`.
- **Errors in the arguments** come back as a failed tool call, with the reason, so the model can correct itself. An unknown tool name is a protocol error.
- **A non-zero exit code** marks the call as failed. The output is still returned.
- **On timeout** the engine's process is stopped. Whether that also stops the command inside the VM has not been checked on a real engine. See `docs/NEEDS_A_VM.md`.

The server speaks MCP protocol revisions `2025-11-25`, `2025-06-18`, `2025-03-26` and `2024-11-05`, offering tools only. It answers one call at a time, in order. A cancellation arrives after the call it names has finished, so it is ignored, and each tool's own time limit applies instead.

## What this does not protect

Read this before relying on it.

- **The agent's own host tools.** MCP adds a way into the sandbox. It does not take away anything the agent can already do on the host. The settings above are the only thing standing between a tricked agent and your shell, and its host is a different program that secbox cannot check or enforce.
- **Your user's access to the daemon.** The socket checks which user connects, not which program. An agent with a host shell can run `secbox` itself as you, for example `secbox down --destroy`, or `secbox up --all-ports`, which approves every port in the plan. The design says approvals must come from OS prompts that nothing in a terminal can click. They don't yet.
- **Secrets inside the sandbox.** A secret delivered to a service by environment or file can be read with `exec`, for example `exec` with `service: api` and `command: env`. That output goes to the model, and so to the model's provider. The proxy route that would keep the real value out of the sandbox is not built. Don't assign a secret to a project an agent works on unless the agent and its provider may see it.
- **Prompt injection.** Anything in the project, its dependencies or its logs can contain instructions. Labeling helps a little. What actually limits the damage is that the tools only reach the sandbox, and that the agent's host tools are off.
- **Getting the work out.** Changes stay in the sandbox. `secbox diff` and `secbox export` are not built, so there is no reviewed way to bring them back to the host yet. Destroying the sandbox deletes them.
- **The network.** The sandbox reaches only what the engine's hostname allowlist lets through. The rule engine in `crates/egress` is not connected yet. See the README.

## Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `/mcp` shows the server as failed | Run the command from `.mcp.json` by hand in a terminal. It prints one line to stderr and then waits for input, which means it works. Ctrl-D ends it |
| A tool says the daemon is not running | Start `secboxd`. If you use `SECBOX_HOME`, check that `.mcp.json` passes the same value (setup copies it) |
| A tool says the sandbox is stopped, or there is none | Run `secbox up PATH`. The tools never start one |
| A tool says the daemon is older than this command | Restart `secboxd`. Its sandboxes keep running |
| The program moved, for example after reinstalling | `.mcp.json` holds its absolute path. Delete the agent folder and run setup again, or pass `--binary` |
| Claude Code asks to use `Bash` or `Read` anyway | It was started outside the agent folder, or the settings were edited. Start it from the folder setup made |

## What has been tested

- Unit tests: the protocol handshake and version choice, notifications, refused batches, unknown methods, argument checks, path checks, text cleaning, capped output, timeouts, a process left holding a pipe, and the setup files.
- In-process tests with the real daemon logic, a fake engine and a stub engine binary (`crates/cli/tests/sandbox.rs`, the `mcp_` tests). They check that `exec`, `read_file` and `write_file` reach the engine with the right arguments, a cleared environment and the right stdin, that a stopped sandbox is refused without booting it or running the engine, that hostile output is filtered, and that setup refuses a folder overlapping the project.
- The real binary on real pipes against a real daemon: the handshake, and a clear refusal when the daemon cannot run.
- Claude Code 2.1.318: it reads the folder `setup claude` writes, and `claude mcp list` reports the server as connected.

Not tested: any of it against a real VM, Codex (not installed where this was built), a model actually calling the tools, and whether the deny rules hold in every Claude Code mode. `docs/NEEDS_A_VM.md` and `docs/MANUAL_TEST.md` list what to check.

## Where the code is

| File | What it holds |
| --- | --- |
| `crates/cli/src/mcp/mod.rs` | The `mcp` command, and binding to one project |
| `crates/cli/src/mcp/protocol.rs` | JSON-RPC over stdio, the handshake, the instructions sent to the agent |
| `crates/cli/src/mcp/tools.rs` | The five tools, path checks, output cleaning |
| `crates/cli/src/mcp/run.rs` | Running the engine with captured, capped output and a time limit |
| `crates/cli/src/mcp/setup.rs` | `setup claude` and `setup codex` |
| `crates/cli/src/debug.rs` | The `terminal` request and engine command shared with `secbox exec` |
