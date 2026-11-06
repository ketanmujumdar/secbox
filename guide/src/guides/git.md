# Git without handing over your keys

You can use any Git command on a sandboxed project, and so can your AI agent. Your SSH key, token and agent never go into the sandbox. This page explains why that matters, what you can do, and how it stays safe.

> Built, and tested with real Git against a fake engine, where a local folder stands in for the VM and a local repository stands in for GitHub. It has never run on a real VM or against a real Git server. See [What works today](../start/status.md).

## Why this is useful

Most supply-chain attacks today follow the same path: a package runs a script at install time, finds the developer's credentials, and uses them to spread. A GitHub token or SSH key is one of the first things these scripts look for, because it lets them push code to every repository you can reach.

Running the project in a sandbox stops the script from reading your files. Git is the hard part, though, because you still need to fetch, commit, branch and push. The usual shortcuts each put the credential back where the attacker can reach it:

| Shortcut | What a malicious script in the sandbox gets |
| --- | --- |
| Copy a token into the sandbox | The token, to use from anywhere until you revoke it |
| Forward your SSH agent into the sandbox | Your key's signatures, for any repository, while connected |
| Run Git on your machine against the sandbox's folder | Code execution on your machine, through hooks or settings planted in `.git` |

secbox avoids all three:

- **Inside the sandbox, Git is ordinary Git.** You can branch, stash, rebase, bisect and blame, using your usual commands.
- **No credential is in the sandbox,** so a hook, a planted setting or a tricked agent has nothing to steal.
- **Only fetch, pull and push reach the remote.** They go through a relay on your machine that connects to the project's own repository with your key, and only that repository.
- **Every push is checked before anything is sent.** The relay refuses pushes to `main` or `master`, deleted branches, tags and push options.
- **Your machine never runs Git inside the sandbox's `.git`,** so a hook planted there can't run on your machine.

What you end up with: an AI agent, or a compromised package, can at worst push a branch to the project's own repository, and only after your SSH agent asks you to approve it. A person still reviews and merges that branch.

## How it works

```
your machine                                          the sandbox VM
────────────                                          ──────────────
secbox git run push                                   /work: a real clone
 or the Git command box,                              no key, no token, no agent
 or the git tool over MCP ───────────────────────────► git push runs here
                                                      │
   secboxd ◄── the connection is handed back ─────────┘
     1. is this a push? then only a push is allowed
     2. reads the branch list: not main or master,
        no deletes, no tags, no push options
     3. starts ssh with your agent, to the project's own URL only
     4. copies the rest across, unread
```

Commands other than fetch, pull and push get no relay at all. If one tries to reach the remote, it finds the clone's `core.sshCommand` pointing at a message that says how to push through secbox, and no credential to use.

The full design, including the exact `ssh` options and what has been tested, is in [`docs/GIT.md`](https://github.com/ketanmujumdar/secbox/blob/main/docs/GIT.md).

## Using it

Start from a remote, as in [Bringing a project in](new.md):

```sh
secbox new git@github.com:acme/shop.git
```

Then run Git in any of three ways. The same daemon request sits behind each one, so the same rules apply to all of them.

**Command line.** Put `--project` before `run`, because everything after `run` goes to Git:

```sh
secbox git --project git@github.com:acme/shop.git run switch -c fix-refunds
secbox git --project git@github.com:acme/shop.git run commit -am "Fix the refund total"
secbox git --project git@github.com:acme/shop.git run push -u origin HEAD
```

Inside the project's folder, you can leave `--project` out. `secbox git status`, `pull`, `commit` and `push` are still there as shortcuts.

**Desktop app.** The project's Changes tab has a **Git command** box. Type a command without the leading `git`, such as `log --oneline -10` or `stash list`. Quote an argument that has spaces in it. If a pull stops because your uncommitted changes are in the way, the tab offers **Stash, pull, reapply**, which runs `pull --autostash`.

**AI agents.** Over MCP, the `git` tool takes the arguments as a list, such as `["rebase", "main"]`. See [AI agents over MCP](ai-agents.md).

### Things to know

- **There is no terminal.** Give commit messages with `-m`. A command that would open an editor or ask for a password fails straight away and says why. Pagers are turned off.
- **Nothing may come before the command.** Options such as `-c`, `-C` and `--git-dir` are refused. Put settings in the clone with `git config` instead, which only affects the sandbox.
- **A networked command can't choose its own remote program.** `--upload-pack`, `--receive-pack` and `--exec` are refused, and so are `-o` and `--push-option`.
- **One connection per command.** `fetch --all` across several remotes, submodules and Git LFS are not relayed.
- **Your name goes on commits.** The command line, the desktop app and MCP read `user.name` and `user.email` from your global Git settings.
- **Every command is in the audit log** under its name, such as `git_ran commit`. Arguments are not logged, because they can hold a commit message. Each push is also logged with its branch and both commit ids.

## What it does not protect

- **The sandbox can push any branch except the protected ones,** to the project's own repository, once your SSH agent approves. Your CI may run on that branch. Don't give CI on unreviewed branches secrets it doesn't need.
- **A force-push to a feature branch looks like a normal push.** Telling them apart would mean reading the pack, which the relay never does. Turn on branch protection or rulesets on GitHub to refuse force-pushes.
- **The clone holds the full history,** including any secret ever committed to it.
- **Your `~/.ssh/config` runs** on every relayed connection, including any `ProxyCommand` in it. It is your own file.

For the threat model as a whole, see [Security model](../concepts/security-model.md).
