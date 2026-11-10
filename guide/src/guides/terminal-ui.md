# Full-screen terminal interface

Run `secbox` with no arguments to open a full-screen interface. It shows every project the daemon knows about, and lets you bring a project in, start, stop and destroy sandboxes, follow logs, approve blocked hosts, read and commit changes, run Git, set secrets and remove projects. It is one more client of the same daemon, with the same powers as the commands.

```sh
secbox
```

The terminal must be at least 60 columns by 14 rows. The red tag at the right of the top bar, saying this is an early build, never goes away. The bar along the bottom lists only the keys that work where you are, and says at the right whether the daemon answered (`● daemon`, or `daemon off`).

The screenshots on this page show the previous layout and have not been retaken yet.

## Bringing a project in

With no project yet, the page offers two ways in, in two boxes:

- `g` **Clone from Git.** Paste an SSH remote and press Enter. It is fetched on this machine with your SSH key, which never goes into the sandbox.
- `f` **Add a folder.** Type a path (a leading `~` is your home folder) and press Enter. If you started `secbox` inside a folder that looks like a project (it has a Compose file, a Dockerfile, a dev container or a package manifest), that folder is already filled in and chosen.

Left and right choose a box and Enter opens it. While typing, Esc cancels. Either way the next thing you see is the plan, as described under [Starting](#starting); nothing starts until you agree to it. `secbox new git@…` or `secbox new .` does the same without this screen.

Once there are projects, `+` (or `N`) shows the same two boxes beside the list, and Esc goes back.

## Projects and tabs

Projects are listed down the left, each with a mark for its state: `●` running, `○` stopped, `·` no sandbox, `?` unknown. Above the list is `+ New project`. On the right, the selected project's name, state and folder sit over its tabs. Each tab opens with the letter drawn on it:

| Key | Tab | Shows |
| --- | --- | --- |
| `o` | Overview | Boxes for the services, ports on `127.0.0.1`, resource limits and blocked lookups |
| `l` | Logs | Recent and new log lines, for all services or one |
| `n` | Network | Blocked hostnames, newest last. The tab carries the number of hostnames refused |
| `c` | Changes | What changed in the sandbox, flags first, with each file's changed lines. Commit, Git, pull and push are here too. The tab carries the number of changed files |
| `s` | Secrets | Secret names, how each was stored and which services get it. Set or replace a value here. Never shows one |
| `t` | Settings | The project, its limits, and destroying or removing it |

`Tab` and Shift-Tab move to the next and previous tab.

## Keys

| Key | Where | What it does |
| --- | --- | --- |
| Up, Down (or `k`, `j`) | Overview, Settings, Secrets with no secrets | Choose a project |
| Up, Down, PgUp, PgDn | Logs, Network | Scroll |
| Up, Down | Changes | Choose a file. After Enter, scroll it |
| Enter, Esc | Changes | Read the chosen file with up and down, and go back to the list |
| PgUp, PgDn | Changes | Scroll what is open |
| Up, Down, Enter | Secrets | Choose a secret, and set or replace its value and who gets it |
| `a` | Secrets | Add a secret |
| `f` | Secrets | Use an env file for this project's secrets. See [Secrets](secrets.md#using-an-env-file) |
| `a` | Network | Approve the hostname on the bottom row, after asking |
| `u` (or F2) | Overview, not running | Start the selected project, after showing the plan |
| `S` (or F3) | Overview, running | Stop it, after confirming |
| `x` | Overview, Settings, with a sandbox | Destroy it, after confirming |
| `D` | Settings | Remove the project from the list, after confirming |
| `e` | Overview, Logs, running | Open a shell, in the chosen service on Logs |
| `v` | Overview, Logs, running | Open the sandbox's files in VS Code. See [Editors](editors.md#editing-inside-the-sandbox) |
| Left, Right (or `[`, `]`) | Logs | Choose a service or All |
| `f` or End | Logs | Follow new lines again |
| `r` (or F5) | Every tab | Read again. On Changes, compare now |
| `m` | Changes | Commit every change, with a message you type |
| `g` | Changes | Run any Git command, typed without the leading `git` |
| `p`, `P` | Changes | Git pull, and push of the current branch, with your SSH key on this machine |
| `+` or `N` | Every tab | New project |
| `g`, `f` | New project | Clone from Git, add a folder |
| `y` | Questions | Yes. Only `y` agrees |
| `n`, Esc, Enter | Questions | No, which is the default |
| `?` (or F1) | Everywhere | Help for where you are |
| `q`, Ctrl-C (or F10) | Everywhere but while typing | Quit |

Starting, stopping and destroying work only on Overview, and only the ones the project's state allows: a running project offers Stop and no Start. The letter that opens a tab never does anything else, so a key pressed to look at something cannot stop a sandbox.

## Logs

The Logs tab follows new lines by itself and says `Following` at the top right. Scrolling up pauses it: the tag changes to `Paused` and counts lines that arrive. `f`, End, or scrolling back to the newest line follows again.

## Shells

Pressing `e` steps the interface aside and opens the same shell as `secbox shell`. When you leave the shell, the interface comes back and says the shell's exit code.

## Starting

Pressing `u` asks the daemon for the plan and shows it in a box, in the same groups as the review in the desktop app:

- **Reach these sites:** the hosts allowed while building and while running.
- **Open ports on this computer only:** each port that would be forwarded to `127.0.0.1`, and the service behind it.
- **Get these secrets:** each secret's name, the services it goes to and how, and every host it could be sent to.
- **Use up to:** memory, CPUs and disk.
- **Findings:** the high and critical findings, files that look like they hold a secret, and a count of the rest.

`y` starts it with those ports. `n`, Esc and Enter cancel: No is the default. A plan longer than the terminal scrolls with up and down, and `y` works only once all of it has been on screen. Keys pressed before the box appeared, such as the rest of a paste, are thrown away. To pick ports one by one, use `secbox up --port` instead. While it starts, a box lists each step as the daemon reports it, the same steps `secbox up` prints. Keys do nothing until it ends.

On a Mac, after `y` the daemon asks you to confirm with Touch ID or your password, in macOS's own prompt. The busy line and the box say "Confirm with Touch ID or your login password" until you answer. The interface can't answer it for you. On Linux there is no prompt: the box says so, and the result ends with "Not confirmed by fingerprint". See [Confirming with Touch ID](../concepts/confirming.md).

## Changes

The Changes tab shows the same comparison as [`secbox diff --patch`](reviewing-changes.md). It compares by itself when the tab opens with the sandbox running, unless it compared in the last 15 seconds, and again after a commit, pull or other Git command that can move files. `r` compares now. The comparison runs on its own, and the interface keeps working meanwhile.

Files are listed with flagged ones first, marked `!`, each with its lines added and removed, or `not shown`. On a wide terminal the chosen file's lines sit beside the list. On a narrow one, Enter opens the file and Esc goes back. A long line wraps, so nothing hides past the edge. `Summary`, at the top of the list, is everything `secbox diff` prints.

A file that is not shown says why: it looks like it holds a secret, or it is binary, too large or a submodule. Treat it as unread, because a sandbox can cause any of these on purpose. See [Reviewing changes](reviewing-changes.md).

`m` commits every change, under your own Git name, and asks for the message. `g` runs any Git command inside the sandbox, with quotes read as a shell reads them and nothing expanded. `p` pulls and `P` pushes the current branch through the daemon, with your SSH key on this machine. The key never enters the sandbox. If uncommitted changes stop a pull, the box offers to stash them, pull and put them back: `y` agrees, and anything else leaves them alone. A long answer scrolls with up and down, and any other key closes the box.

![The Changes tab, before comparing](../images/cli/tui-review.png)

## Network

![The Network tab, with no engine to ask, so it shows an error](../images/cli/tui-traffic.png)

Pressing `a` on a blocked hostname asks for Touch ID on a Mac before anything is saved or restarted.

## Secrets

![The Secrets tab, with no secrets recorded](../images/cli/tui-secrets.png)

Up and down choose a secret. Enter sets a value for an empty name or replaces a stored one, and `a` adds a new name. You type the value twice and see only dots, then tick the services that may read it with Space. Nothing here can show a stored value. The value goes through the daemon into your keychain, and a running sandbox gets it on its next start. The list itself reads the same metadata as `secbox secrets list` and never opens the keychain. Names added from a template such as `.env.example` that have no value yet show in amber as `empty / not set yet`.

## Settings

The Settings tab shows the project's name, path, where it came from, its state and its limits. `x` destroys the sandbox and leaves the project in the list. `D` destroys its sandbox and removes the project from the list, along with the copy of a project imported from a remote. A folder of your own is not touched. Its stored secrets are deleted from your keychain. Both ask first, and only `y` agrees.

## What only the desktop app has

- **Connecting an AI agent** with one button. Here, the Settings tab names the command: `secbox mcp setup claude --project PATH FOLDER`. See [AI agents over MCP](ai-agents.md).
- **Restarting the daemon.** Here, the bottom bar says whether the daemon answered, and starting it is up to you.
