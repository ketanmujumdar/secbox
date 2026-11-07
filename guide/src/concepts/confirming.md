# Confirming with Touch ID

> **macOS only, and not yet tried on a real Mac.** The prompt is built and checked for macOS, and every rule around it is tested with a stand-in person. Nobody has seen the real prompt appear yet. On Linux there is no prompt at all.

Three actions can send something off your machine, and secbox asks **you**, not the program that asked, before each one:

| Action | What the prompt says |
| --- | --- |
| Starting a sandbox (`up`, `new`, Start in the app, `u` in the terminal interface) | "start shop in its sandbox, with 2 secrets, able to reach 5 hosts, opening port 3000" |
| Letting a sandbox reach one more hostname (`net allow`, Approve in the app, `a` on the Network tab) | "let shop reach api.example.com while it runs, and restart it" |
| Every push through the relay (`git push`, typed `git run push`, Push in the app, `P` on Changes, the `git_push` MCP tool) | "push my-change (1f2a120 to 9c3d4e5) from shop to git@github.com:org/shop.git" |

On a Mac the prompt is macOS's own: Touch ID, or your login password if there is no Touch ID or it is locked. It sits on top of the window you are using, wherever the request came from.

## Why the daemon asks, and not the window

Before this, "you agreed" meant a program sent the daemon the plan's digest. Any program running as you can send the same digest, and that includes one started by a bad dependency on your Mac, or an AI agent talked into it. The fake job assignment in the October 2026 Binance intrusion ran exactly that way: as the engineer, on their laptop.

So `secboxd` puts up the prompt itself. Clients only show that it is waiting: none of them can answer it. A program in the background cannot answer it either, because macOS draws it and takes the fingerprint or password itself. One touch confirms one action: nothing is remembered, not even an unlock a moment ago.

## What to check in the prompt

- **It names what you just asked for.** The project, the host, or the branch and commit.
- **If you did not ask for anything, cancel it.** Something else on your machine asked. Cancelling does nothing at all: no sandbox is made, no host is saved, and the remote gets no bytes of the push.

A prompt that gets no answer is withdrawn after two minutes, and counts as cancelled.

## In each client

The prompt is the same wherever the request came from, because the daemon puts it up. What each client shows while it waits:

| Client | While the prompt is up | If you cancel |
| --- | --- | --- |
| Command line (`up`, `new`, `net allow`, `git push`, `git run push`) | A line starting "Confirm with Touch ID or your login password" | The command fails with "not confirmed with Touch ID or your login password, so nothing was done", and why |
| Terminal interface (`u`, `a` on Network, `P` on Changes) | The same sentence on the busy line, and in the start box | The same message on the status line, or in the push box |
| Desktop app (Start, Approve, Push) | A "Waiting for you to confirm in the system prompt" step on the start page, and the sentence on the busy line | The step reads "Not confirmed, so nothing was started", or a red banner |
| AI agent (`git_push`, or `git` with `push`) | The tool call waits | The tool returns the refusal. Its description tells the agent not to retry and to ask you |

## On Linux

There is no prompt. The action goes ahead on the request alone, as it did before this was added, and every client says so instead of saying you confirmed:

- the command line and the terminal interface's start box print "This computer has no fingerprint or password check, so this goes ahead on this request alone";
- the terminal interface adds "Not confirmed by fingerprint" to the result of a start, an approval or a push;
- the desktop app's start page shows "Started on this window's request alone", and Approve and Push end with "Not confirmed by fingerprint";
- the audit log records `person_unchecked`.

A Linux build never claims a person confirmed anything. A prompt for Linux, such as polkit with a fingerprint reader, is not planned yet.

## What it does not do

- **It guards what goes through the daemon.** Code already running as you can still use your SSH agent directly, run the engine itself to get into a running sandbox and read the secrets delivered there, or replace secbox's own programs. The prompt makes secbox stop being the easy way to do those things. It does not make a compromised Mac safe.
- **`shell`, `exec`, pull, stop and destroy do not ask.** Pull only brings code into the sandbox. `shell` and `exec` go to the engine directly, so a prompt in the daemon would not stop them.
- **It has not been run on a real Mac yet.** See the checklist in `docs/NEEDS_A_VM.md` under "Confirming with Touch ID".

## In the audit log

`person_confirmed`, `person_declined` (with the reason, such as "you cancelled") and `person_unchecked`, each with the project and the action. Never anything you typed.
