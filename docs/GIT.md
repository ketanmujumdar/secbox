# Git: getting code in and out

As of Nov 5, 2026. `secbox` is a working name, see "Changing the name" in the README.

**Status: code goes in, and nothing comes out yet.** A sandbox gets a project's files once, when it is created, either from a folder on your machine or from an SSH remote. `secbox diff` and `secbox export`, which would bring reviewed changes back, are not built. Fetching from an SSH remote has never run against a real server. See [What has been tested](#what-has-been-tested).

## Where a project's code comes from

| Source | Command | What reaches the sandbox |
| --- | --- | --- |
| A folder with a Git repository | `secbox up` or `secbox up PATH` | The files of `HEAD`, or of `--commit ID` |
| An SSH remote | `secbox up git@github.com:org/repo.git` | The files of the remote's default branch, or of `--ref NAME` |

Either way:

- **Only a commit's files go in.** There is no `.git` in the sandbox, and uncommitted changes are refused (`--include-uncommitted` is not built).
- **Likely secrets are left out.** `.env`, key files and other names that usually hold secrets are never copied, and files that look like they contain a secret are listed in the plan. The rules are in `crates/gitbroker/src/secretpath.rs`.
- **The plan names the commit**, and your approval covers it.
- **The copy happens once,** when the sandbox is created. `up` on a stopped sandbox restarts it with the files it already has.

## Starting from an SSH remote

```sh
secbox up git@github.com:org/shop.git                 # the remote's default branch
secbox up git@github.com:org/shop.git --ref release   # a branch, tag or full commit id
secbox up ssh://git@git.example.com:2222/srv/shop.git
```

You don't need a clone on your machine. The daemon fetches on the host and keeps the result in its own state folder:

```
~/.secbox/remote/github-com-shop-1a2b3c4d5e6f/
    mirror.git/   a bare repository holding what was fetched, HEAD at that commit
    shop/         that commit's files, secrets left out, plus a .git file pointing at mirror.git
```

`shop/` is then the project, exactly as a folder you named would be. Every other command takes the same remote in place of a folder:

```sh
secbox status git@github.com:org/shop.git
secbox logs api --project git@github.com:org/shop.git
secbox shell --project git@github.com:org/shop.git
secbox mcp setup claude ~/agents/shop --project git@github.com:org/shop.git
```

These only look the remote up. They never fetch.

**Getting a newer commit.** A fetch never replaces the files under an existing sandbox, because the sandbox would still hold the old ones. While a sandbox exists, `up` says so and leaves it alone, `--ref` included. To start from the newest commit, run `secbox down --destroy REMOTE`, then `secbox up REMOTE`. That deletes everything in the sandbox. A proper `pull` is not built.

**Accepted remotes:**

- `[user@]host:path`, as Git's scp-like form, and `ssh://[user@]host[:port]/path`.
- Not HTTPS yet. `git://`, `file://`, transport helpers such as `ext::`, and anything else are refused.
- Strict on purpose: nothing may start with `-`, so nothing can turn into an `ssh` or Git option, as in the classic `ssh://-oProxyCommand=...` attack. Only printable ASCII is allowed, with no `%`, no `::` and no `..` in the path.

A folder that exists with a name like `a:b` is still treated as a folder.

## How your SSH key is used

**Your key never goes into the VM.** Nothing in the sandbox can reach the key, the agent or Git credentials, because the fetch happens before the sandbox gets anything, on the host, in another process.

```
secbox up git@github.com:org/shop.git            your shell
   │  sends the URL, the ref and the path of your SSH_AUTH_SOCK (not a key)
   ▼
secboxd                                          on the host, as you
   │  checks the socket is a socket owned by you
   │  runs git: no hooks, no global config, fsck on every object, ssh only
   ▼
/usr/bin/ssh -o BatchMode=yes -o StrictHostKeyChecking=yes ...
   │  signs with your key through the agent; the key itself stays in the agent
   ▼
github.com                                       objects come back into mirror.git
   │
   ▼
snapshot of one commit → files only → copied into the new sandbox at /work
```

In order:

1. **The command line sends a path, not a key.** If your shell has `SSH_AUTH_SOCK` set, the command line passes that path to the daemon. That way an agent unlocked in this session works even if the daemon was started before it, for example by the desktop app at login.
2. **The daemon checks the path** is a socket owned by the user it runs as, and refuses anything else. Without one from the client, it uses its own `SSH_AUTH_SOCK`, under the same check.
3. **Git runs hardened** through the same helper as the rest of `crates/gitbroker`: a cleared environment with an empty temporary `HOME`, no global or system Git config, hooks off, no submodules, no tags, and fsck on every object received. SSH is the only transport allowed, and only for this call. The agent socket is the one thing from your environment that reaches it.
4. **`ssh` gets one fixed command line.** Options given with `-o` beat the same options in `~/.ssh/config`:

   | Option | Why |
   | --- | --- |
   | `BatchMode=yes` | Never prompt for a passphrase, password or yes/no. Nobody is there to answer, so a prompt would only hang |
   | `StrictHostKeyChecking=yes`, `UpdateHostKeys=no` | Refuse a host whose key is not already in your `known_hosts`, and never write to it |
   | `ForwardAgent=no`, `ForwardX11=no`, `ClearAllForwardings=yes` | Nothing goes back the other way |
   | `PermitLocalCommand=no`, `RequestTTY=no` | No local command, no terminal |
   | `ControlMaster=no`, `ControlPath=none` | No reuse of a connection opened with other options |
   | `ConnectTimeout=30`, `ServerAliveInterval=15`, `ServerAliveCountMax=4` | A dead connection ends |

5. **Your `~/.ssh/config` still applies to everything else**, such as `Host` aliases, `HostName`, `Port`, `User`, `IdentityFile`, `IdentityAgent`, `ProxyJump` and `ProxyCommand`. A remote behind a bastion, or a key chosen per host, keeps working. It is your own file, and secbox treats it as trusted. `ssh` finds `~/.ssh` from the system's user database, not from `HOME`, so the empty `HOME` Git runs with doesn't hide it.
6. **The program is `/usr/bin/ssh`,** or failing that `/usr/local/bin/ssh` or `/opt/homebrew/bin/ssh`. `PATH` is never searched. The program must be a regular executable that nobody but its owner can write to. On macOS, `/usr/bin/ssh` is Apple's, which can use keys stored in the Keychain.

### Which keys work

| Your setup | Works? |
| --- | --- |
| A key loaded in `ssh-agent` (`ssh-add`), with or without a passphrase | Yes |
| 1Password, Secretive or another agent, through `SSH_AUTH_SOCK` or `IdentityAgent` in `~/.ssh/config` | Yes. Touch ID or the agent's own prompt appears as usual |
| macOS with `UseKeychain yes` and `AddKeysToAgent yes` | Yes, once the key has been added to the agent |
| A key file with no passphrase, no agent | Yes. `ssh` finds it in `~/.ssh` as usual |
| A key file with a passphrase, no agent | No. Nothing can ask for the passphrase. Run `ssh-add` first |
| A hardware key (FIDO, `sk-` keys) | Through an agent, as above. Untested. A key that needs a touch should work. One that asks for a PIN may not, because nothing can type it |

### When it fails

The error shows `ssh`'s own words, plus what to do:

| Message | What to do |
| --- | --- |
| `Host key verification failed` | Check the host's key, then connect once yourself to add it, for example `ssh -T git@github.com` |
| `Permission denied (publickey)` | `ssh-add`, or check that this key may read the repository |
| `Could not resolve hostname` | The host can't be reached from this machine |
| `couldn't find remote ref` | No branch, tag or commit by that name |

### Recommendations

- **Use a read-only key for fetching** where you can: a deploy key on the repository, or a key limited to reading. A fetch never needs to write.
- **Check new host keys** against the provider's published fingerprints before adding them. secbox won't add one for you.
- **Keep the agent locked when you're not using it.** While a fetch runs, `ssh` can use any key the agent offers for that host, the same as any other `ssh` you run.

## What this does not protect

- **Git on the host parses what the remote sends.** Hooks and config are neutralized and fsck checks every object, but a memory-safety bug in Git's pack parsing would be in a host process. This is the same exposure as `git clone` on your laptop. Fetching inside a throwaway VM would remove it, and is planned for bundles already.
- **Your `~/.ssh/config` runs.** A `ProxyCommand` or `Match exec` in it runs during the fetch. That is your own file, but a tool that can write to it can make a fetch run anything.
- **Git LFS files are not downloaded.** The sandbox gets the small pointer files instead. Submodules are not fetched either.
- **The export follows the secret-name rules, not your intent.** A secret committed under an ordinary name is copied in, with a warning in the plan if it looks like one.

## Getting code out

Not built. This is what exists and what is planned:

| Piece | State |
| --- | --- |
| Quarantine a bundle from the VM, review it without running anything from it, approve one commit, push it to a `secbox/<task>` branch | Built in `crates/gitbroker` as a library, with tests that need Git 2.50. No command uses it |
| `secbox diff` and `secbox export` | Not built (Milestone 3) |
| Shared history between the VM's commits and the remote | Not settled. The sandbox has no `.git`. The mirror of a fetched remote holds the base commit, which is what a bundle with prerequisites needs |
| Push over HTTPS with a credential helper | Built in the library, never tried against a real server |
| Push over SSH | Not built. The library refuses SSH remotes for pushing. Allowing it would reuse the fetch's fixed `ssh` command, still only to a `secbox/<task>` branch, and only after you approve |
| Approval | Must come from Touch ID or a passphrase, so nothing in a terminal can approve. Not built |

**Getting work out by hand today:** read files with `secbox exec -- cat FILE`, or with an agent's `read_file` tool. Generate a patch with `secbox exec -- git diff` if you initialized Git in the sandbox yourself, then review it and apply it on the host.

## What has been tested

- URL and ref checks, the `ssh` command line, the agent socket checks, and Git getting the SSH command and socket only on an SSH call. These are unit tests that need no Git.
- A real fetch: Git fetching through a stand-in `ssh` that runs `git-upload-pack` locally, landing one commit and no tags in the mirror, then a snapshot from it. This needs Git 2.50 like every other Git test in `crates/gitbroker`. It passed once on Git 2.43 with that check lowered for the run, and fails on the version check otherwise.
- The daemon and command line with a fake fetch (`crates/cli/tests/sandbox.rs`, after "projects fetched from an SSH remote"):
  - `up REMOTE` fetches once, writes the folders above, and starts from that commit.
  - `--ref` is passed on.
  - Nothing is fetched under an existing sandbox.
  - Other commands find the project by URL without fetching.
  - A failed fetch starts nothing and says what to do.
  - Hostile URLs are refused before anything runs.
  - Only a socket of this user is passed on as the agent.

Not tested: a fetch from a real server, any agent other than none, macOS Keychain and 1Password agents, and a remote behind a `ProxyJump`. `docs/NEEDS_A_VM.md` lists what to check.

## Where the code is

| File | What it holds |
| --- | --- |
| `crates/gitbroker/src/remote.rs` | URL and ref checks, the `ssh` command line, `fetch_ssh` |
| `crates/gitbroker/src/git.rs` | The hardened Git helper, including the SSH-only call |
| `crates/gitbroker/src/snapshot.rs` | Exporting one commit's files, secrets left out |
| `crates/daemon/src/remote.rs` | The `fetch` request: folders, swapping in a new copy, not fetching under a sandbox, error hints |
| `crates/daemon/src/main.rs` | Finding `ssh` and the agent for a real fetch |
| `crates/cli/src/vm.rs`, `crates/cli/src/lib.rs` | `up REMOTE --ref`, and accepting a remote wherever a folder is accepted |
