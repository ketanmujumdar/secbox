# MCP tools

The tools `secbox mcp serve` offers to an AI agent, with their arguments and limits. Every tool acts only on the sandbox of the project the server was started for.

| Tool | Arguments | What it does | Limits |
| --- | --- | --- | --- |
| `status` | none | Whether the sandbox is running, its services and their health, forwarded ports, and how many hostname lookups were refused | 30 s for the daemon to answer |
| `logs` | `service`, `tail` (default 200, at most 5000) | The last lines from `docker compose logs`. Never follows | 256 KiB, 60 s |
| `exec` | `command` (required), `service`, `timeout_seconds` (default 120, at most 600) | Runs `sh -c COMMAND` in `/work`, or inside one Compose service with `docker compose exec -T`. Returns the exit code, stdout and stderr. No terminal and no input | 256 KiB per stream, 64 KiB command |
| `read_file` | `path` (required) | Reads a file under `/work` | 1 MiB, 60 s |
| `write_file` | `path`, `content` (both required) | Replaces a file under `/work`, making its folders. The content goes in on stdin and the path as an argument, never as part of a script | 1 MiB, 60 s |
| `git` | `args` (required): the command and its arguments without the leading `git`, such as `["switch", "-c", "fix"]` | Runs any Git command in the clone, inside the sandbox. `fetch`, `pull` and `ls-remote` go through the relay as a fetch, and `push` as a push with its checks. Every other command gets no relay. Options before the command, and `--upload-pack`, `--receive-pack`, `--exec`, `-o` on a networked command, are refused | 64 arguments, 16 KiB each; 5 min, or 15 min with the relay |
| `git_status`, `git_pull`, `git_commit`, `git_push` | `git_commit`: `message` (required), `paths` | Shortcuts: status, pull with merge, commit everything or the paths given, push the current branch. On a Mac a push waits for the person's Touch ID, and fails if they cancel | 60 s, or 15 min with the relay |

The Git tools are only for a project imported from a remote. See [Git without handing over your keys](../guides/git.md).

## Paths

Paths for `read_file` and `write_file` are relative to `/work`, or start with `/work/`. A path containing `..`, `.`, an empty part or a control character is refused before anything is asked.

These two tools are tied to `/work` to keep them about the project, not because the rest of the VM is out of reach: `exec` can read anything in the sandbox.

## Text

`read_file` keeps tabs, newlines and carriage returns. Every other control or invisible character, and every invalid UTF-8 byte, is shown as an escape such as `\u{1b}`, and the reply says so. Use `exec` with `base64` for exact bytes.

Every result from the sandbox starts with a line saying it is data, not instructions.

## Errors

- Bad arguments come back as a failed tool call with the reason, so the model can correct itself.
- An unknown tool name is a protocol error.
- A non-zero exit code marks an `exec` call as failed. The output is still returned.
- On timeout the engine's process is stopped. Whether that also stops the command inside the VM has not been checked on a real engine.

## Protocol

- Transport: stdio only. JSON-RPC 2.0, one message per line.
- Protocol revisions: `2025-11-25`, `2025-06-18`, `2025-03-26` and `2024-11-05`.
- Capabilities: tools only.
- One call at a time, in order. Cancellations are ignored, because each tool has its own time limit.

## Try it by hand

With a sandbox running, this sends a handshake, lists the tools and runs one command:

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"exec","arguments":{"command":"pwd"}}}' \
  | secbox mcp serve --project ~/code/myapp
```
