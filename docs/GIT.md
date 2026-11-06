# Git: import, pull and push

As of Nov 5, 2026. `secbox` is a working name, see "Changing the name" in the README.

**Status: built, and never run against a real VM or a real Git server.** Every piece below has run with real Git on both sides, in tests where a local folder stands in for the VM and a local repository stands in for GitHub. The engine's two-way exec, Git inside the guest, and your SSH agent have not been tried together. See [What has been tested](#what-has-been-tested).

## In one picture

```
your Mac                                            the sandbox VM
────────                                            ──────────────
secbox up git@github.com:org/shop.git
  1. secboxd fetches with your ssh and agent ─┐
     (1Password asks you to approve)          │
  2. you see the plan and say yes             │
  3. the VM clones through the relay ─────────┼──►  /work: a real clone, full history
                                              │     no key, no token, no agent
secbox git pull / git_pull (MCP) / Pull ──────┤     git pull runs here, merges here
secbox git push / git_push (MCP) / Push ──────┘     git push runs here
     secboxd reads the branch list first:
     not main or master, no deletes, no tags
     then relays the rest to GitHub, unread
```

## Starting from a remote

```sh
secbox up git@github.com:org/shop.git                 # the remote's default branch
secbox up git@github.com:org/shop.git --ref release   # a branch, tag or full commit id
secbox up ssh://git@git.example.com:2222/srv/shop.git
```

You don't need a clone on your machine. `up`:

1. **Fetches on your Mac** into a mirror in the daemon's state folder, `~/.secbox/remote/<host>-<repo>-<hash>/`, and exports the commit's files there, with likely secrets left out. The plan is made from those files.
2. **Shows the plan** and waits for your yes, as for any project.
3. **Clones into the new VM** at `/work` through the relay, at exactly the commit you approved. The VM gets the full history: `log`, `blame`, branches, tags, everything Git normally has.

Every other command takes the remote in place of a folder, and only looks it up: `secbox status REMOTE`, `secbox shell --project REMOTE`, `secbox mcp setup claude FOLDER --project REMOTE`.

**Accepted remotes:**

- `[user@]host:path`, as Git's scp-like form, and `ssh://[user@]host[:port]/path`.
- Not HTTPS yet.
- Strict on purpose: nothing may start with `-`, so nothing can turn into an `ssh` or Git option. Only printable ASCII is allowed, with no `%`, `::` or `..`.

A clone holds everything ever committed. Unlike the snapshot of a folder project, which leaves `.env` and other likely secrets out, a clone includes any secret committed to the repository at any point in its history. That is what having the real history means.

## Pull, commit and push

Inside the sandbox it's plain Git. `secbox shell --project REMOTE` gives you a shell in `/work`, where `git switch -c`, `commit`, `merge`, `rebase`, `stash` and the rest work as usual. Only talking to the remote goes through secbox:

| What | Command line | Terminal UI | Desktop app | MCP tool |
| --- | --- | --- | --- | --- |
| Import | `secbox up REMOTE` | `g` on the New project page (`+`), then paste the URL | Import box in the sidebar | none, a person imports |
| Status | `secbox git status` | | Git tab, Status | `git_status` |
| Pull | `secbox git pull` | `g` | Git tab, Pull | `git_pull` |
| Commit | `secbox git commit -m MSG [PATHS]` | | Git tab, Commit | `git_commit` |
| Push | `secbox git push` | `G` | Git tab, Push branch | `git_push` |

Add `--project REMOTE` (or a folder) to name the project from elsewhere.

- **Pull** fetches and merges the current branch's upstream (`git pull --no-rebase`). Conflicts are left in `/work` to resolve and commit as usual.
- **Commit** stages everything that changed, or only the paths given, and commits under your name.
- **Push** pushes the current branch to the same name on the remote and sets it as the upstream (`git push --set-upstream origin HEAD`).
- **Your name on commits:** the command line, the desktop app and MCP read `user.name` and `user.email` from your global Git settings, and only those. `GIT_AUTHOR_NAME` and `GIT_AUTHOR_EMAIL` override them. Without either, commits say `Sandbox <sandbox@localhost.invalid>`. Commits you make by hand in the shell use whatever the VM's Git is set to.
- **A `git push` typed inside the VM fails** with a message saying how to push through secbox. The clone's `core.sshCommand` points at that message, and there is no credential in the VM to use.

### What a push may change

Before a push sends anything, Git lists which branches it will change and from which commit to which. The daemon reads that list and refuses the whole push, sending the remote nothing at all, if any line:

- changes `main` or `master`,
- deletes a branch,
- touches anything other than a branch, such as a tag or a note,
- or isn't in Git's normal form.

What the daemon can't tell: a force-push that rewrites a feature branch looks the same as a normal push, because telling them apart means reading the pack, and the host never parses data from the VM. Turn on branch protection on GitHub to stop those. Every push and every refusal is in the audit log, with the branch and both commit ids.

## How your SSH key is used

**Your key never goes into the VM.** The VM's Git talks to the remote through a relay:

1. **Git in the VM thinks it is running `ssh`.** Its `ssh` program is a small script that writes one header line saying which Git service it wants: fetch (`git-upload-pack`) or push (`git-receive-pack`). Then it joins Git to the channel back to the host.
2. **The daemon checks the service** against the operation. A pull may only fetch, a push may only push. It ignores the repository path in the header.
3. **The daemon starts the real `ssh` on your Mac,** to the URL the project was imported from, never one the VM names. Your agent signs.
4. **The daemon copies bytes both ways.** For a push it first reads and checks the branch list, as above, and parses nothing else from the VM.

`ssh` gets one fixed set of options:

| Option | Why |
| --- | --- |
| `BatchMode=yes` | `ssh` never prompts for a passphrase, password or yes/no. Your agent's own approval prompt, such as 1Password's, still appears, because that is the agent asking, not `ssh` |
| `StrictHostKeyChecking=yes`, `UpdateHostKeys=no` | Refuse a host whose key is not already in `known_hosts`, and never write to it |
| `ForwardAgent=no`, `ForwardX11=no`, `ClearAllForwardings=yes` | Nothing goes back the other way |
| `PermitLocalCommand=no`, `RequestTTY=no` | No local command, no terminal |
| `ControlMaster=no`, `ControlPath=none` | No reuse of a connection opened with other options |
| `ConnectTimeout=30`, `ServerAliveInterval=15`, `ServerAliveCountMax=4` | A dead connection ends |

Everything else comes from your `~/.ssh/config`: `Host` aliases, `HostName`, `Port`, `User`, `IdentityFile`, `IdentityAgent`, `ProxyJump`. `ssh` finds `~/.ssh` from the system's user database, so the daemon's cleared environment doesn't hide it. The program is `/usr/bin/ssh`, or failing that `/usr/local/bin/ssh` or `/opt/homebrew/bin/ssh`. On macOS, `/usr/bin/ssh` is Apple's.

**Which agent:** the command line and the desktop app pass the path of their `SSH_AUTH_SOCK` (a path, never a key), which the daemon uses only if it is a socket owned by you. Without one, the daemon uses its own `SSH_AUTH_SOCK`. An `IdentityAgent` line in `~/.ssh/config` beats both, as it does for any `ssh`.

### 1Password

1Password's SSH agent works as it does for `git` in your terminal:

1. In 1Password, turn on the SSH agent (Settings, Developer, "Use the SSH agent").
2. Point `ssh` at it, as 1Password suggests, in `~/.ssh/config`:

   ```
   Host *
     IdentityAgent "~/Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock"
   ```

   Because the daemon's `ssh` reads this file, it works even when the daemon was started by the desktop app at login, with no `SSH_AUTH_SOCK` of its own.
3. Add the host's key once, for example `ssh -T git@github.com`.

On each import, pull and push, 1Password asks whether to allow the key. The request comes from `ssh`, started by `secboxd`. Approve it, and 1Password's own settings decide how long that approval lasts. If you deny it, the command fails with a hint and nothing is sent.

### Other agents and keys

| Your setup | Works? |
| --- | --- |
| A key loaded in `ssh-agent` (`ssh-add`), with or without a passphrase | Yes |
| Secretive or another agent, through `SSH_AUTH_SOCK` or `IdentityAgent` | Yes. Untested |
| macOS with `UseKeychain yes` and `AddKeysToAgent yes` | Yes, once the key is in the agent |
| A key file with no passphrase, no agent | Yes |
| A key file with a passphrase, no agent | No. Nothing can type the passphrase. Run `ssh-add` first |
| A hardware key (FIDO, `sk-` keys) through an agent | Untested. A touch should work; a PIN prompt may not |

### When it fails

| Message | What to do |
| --- | --- |
| `Host key verification failed` | Check the host's key, then connect once yourself: `ssh -T git@github.com` |
| `Permission denied (publickey)` | Unlock 1Password or `ssh-add`, and check that this key may read (or write) the repository |
| `agent refused operation` | You denied the agent's prompt, or it timed out. Try again and approve |
| `Could not resolve hostname` | The host can't be reached from this machine |
| `refused by secboxd: pushing to main is not allowed` | Make a branch: `git switch -c NAME` in the shell, then push |
| `git is not installed in this sandbox` | The VM's image has no Git. See `docs/NEEDS_A_VM.md` |

## What this does not protect

- **The sandbox can push any branch but the protected ones,** to the project's own repository, while you approve in your agent. A tricked agent can push a branch with bad code, and your CI may run on it. It can't reach `main`, delete branches, push tags or reach another repository.
- **A force-push to a feature branch can't be told apart** from a normal one without reading the pack. Branch protection on GitHub can refuse it.
- **The clone holds the full history,** including any secret ever committed.
- **Git on the host parses the import fetch.** The first fetch, for the plan, goes into the host's mirror through the hardened Git helper, with fsck. After that, the host only relays, never parses.
- **Your `~/.ssh/config` runs.** A `ProxyCommand` or `Match exec` in it runs on every relayed connection. It is your own file.
- **Git LFS and submodules** are not fetched through the relay: LFS uses HTTPS, and submodules would be other repositories.
- **One connection per Git command.** A Git command that opens a second connection to the remote fails on the second.

## Where the code is

| File | What it holds |
| --- | --- |
| `crates/daemon/src/gitrelay.rs` | The relay script, the relay itself, the push checks, status, commit, pull and push |
| `crates/daemon/src/remote.rs` | Import: the mirror, the export for the plan, the remote's record |
| `crates/daemon/src/run.rs` | Cloning into a new sandbox instead of copying files |
| `crates/gitbroker/src/remote.rs` | URL checks, the fixed `ssh` command, starting `ssh` for one Git service |
| `crates/engine/src/lib.rs`, `msb.rs` | The piped exec the relay runs over |
| `crates/cli/src/git_cmd.rs` | `secbox git`, and reading your Git name |
| `crates/cli/src/mcp/tools.rs` | `git_status`, `git_pull`, `git_commit`, `git_push` |
| `crates/cli/src/tui/mod.rs` | Import, Pull and Push in the terminal UI |
| `apps/desktop/` | The Import box and the Git tab |

## What has been tested

- **Unit tests:** URL and ref checks, the `ssh` command line and agent checks, `ssh` started with only the project's URL and the agent, the push list parser (feature branch allowed; `main`, `master`, deletes, tags, notes, signed pushes and malformed lines refused; one bad line stops the whole push; the pack is left unread), the header parser, commit path checks, typing a URL in the terminal UI.
- **End to end with real Git** (`crates/cli/tests/sandbox.rs`): a bare repository stands in for GitHub, and the fake engine runs the VM's Git in a local folder. Covered:
  - `up REMOTE` clones with full history at the approved commit.
  - A `git push` typed in the clone fails with the message.
  - Commit and push of a branch land on the remote and in the audit log.
  - A push to `main` is refused, `main` doesn't move, and the refusal is logged.
  - Pull brings in someone else's commit without making a new sandbox.
  - An agent commits and pushes over MCP, and is refused on `main`.
- **The real binary:** the terminal UI's New project page, drawn at 80 columns.

**Not tested:**

- A real VM, beyond the first clone. On msb 0.7.13, `msb exec` carries both directions at once and passes on the end of stdin, but reports the end of stdout only when the command ends. The relay script allows for that. Pull and push have not run on a real VM.
- A real server.
- 1Password or any agent prompt.
- The desktop app's Import and Git tab clicked through. Its Rust side was type-checked.
- The host-side fetch in `crates/gitbroker` needs Git 2.50, like every Git test there.
