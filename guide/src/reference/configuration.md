# Configuration and files

What secbox reads from a project, which environment variables it uses, and which files it keeps on your machine.

## In a project: `secbox.yaml`

Optional, at the project root, committed with the project. It holds requests, never secret values. Today it has one block:

```yaml
resources:
  memory: 6G      # sizes take K, M, G or T, such as 512M or 1.5G
  cpus: 4         # a number, such as 2 or 1.5
  disk: 30G
  build:
    memory: 8G    # optional, a higher limit while building
```

- Every value is a **request**. It is lowered to what your machine can spare. See [RAM, CPU and disk](../guides/resources.md).
- Unknown keys are an error.
- The file can be at most 64 KB.
- It cannot change editor checks, network policy, secrets or approvals.

## A network policy file

Used by `secbox net check`. See [Network](../guides/network.md#policy-file-format).

```yaml
version: 1
rules:
  - name: npm-read-only
    host: registry.npmjs.org
    ports: [443]
    methods: [GET, HEAD]
```

## Environment variables

| Variable | Read by | Meaning |
| --- | --- | --- |
| `SECBOX_HOME` | All | Use `$SECBOX_HOME/.secbox` as the state folder instead of `~/.secbox`. Must be an absolute path. Keep it short, because of the socket path limit |
| `SECBOX_ENGINE_BIN` | `secboxd`, `doctor` | Absolute path of the engine (`msb`). Default `~/.secbox/msb/bin/msb` |
| `SECBOX_EDITOR_BIN` | `open` | Absolute path of the editor program, if it is not on `PATH` |
| `NO_COLOR` | `secbox` | Any non-empty value turns color off |
| `TERM` | `secbox` | `dumb` turns color off |

secbox never looks for the engine on your `PATH` or in the current folder, so a binary planted in a project cannot be picked up.

## The state folder: `~/.secbox`

Owner-only (`0700`). Holds:

| What | |
| --- | --- |
| `secboxd.sock` | The daemon's Unix socket, `0600` |
| `.state-dir-marker` | Marks the folder as secbox's, so `uninstall` never removes a folder it did not create |
| `settings.json` | The daemon's settings |
| `daemon-audit.jsonl` | The daemon's audit log: starts, stops, shells and `exec` calls. No values |
| `editor-checks.json` | Which [editor checks](../guides/editors.md) are on |
| `editor-audit.jsonl` | Records each time you opened an editor with `--force` |
| `projects/<id>/project.json` | Which folder this project is |
| `projects/<id>/secrets.json` | Secret names, how each was stored, and which services get them. No values |
| `projects/<id>/audit.jsonl` | Secret changes, by name only |
| `projects/<id>/approvals.json`, `sandbox.json` | What you approved, and the sandbox's own record |
| `projects/<id>/baseline.git` | A small Git repository holding the commit of the snapshot the sandbox was given, which [`diff`](../guides/reviewing-changes.md) compares with. Removed by `down --destroy` |
| `msb/` | The engine's home: downloaded images, its database and VM state, if you installed the engine there |

**Secret values are not here.** They are in the macOS Keychain under the service `secbox`, or in Linux Secret Service.

A project's id is its folder name plus a hash of its path, for example `clean-13907af8dbe9c79d`. `secbox secrets list` shows it.

## Inside a sandbox

| Path | |
| --- | --- |
| `/work` | The copy of your project, from the chosen commit |
| `/work/.git` | The same commit as the host's baseline. The sandbox can change it; `diff` compares with the host's copy |
| `/work/compose.yaml` | The generated Compose file, for a project with only a Dockerfile |
| `/run/secrets/NAME` | A secret delivered with `--as-file`, on a `tmpfs` that disappears when the sandbox stops |
