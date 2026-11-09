# Real-Mac test plan

Run this on a Mac, against the real engine, before saying a feature has been tested. It is built from `docs/NEEDS_A_VM.md`, `docs/MANUAL_TEST.md` and `tests/fixtures/hostile-demo/README.md`.

Mark each case **P** (pass), **F** (fail) or **S** (skipped), and note anything odd. A skip proves nothing.

**Stop rule:** if any probe in section 1 prints `LEAKED` (other than the planted hook) or `FAIL`, stop and record the exact output. A leak matters more than finishing the list.

**What passing shows:** that these cases behaved as expected on one Mac and one engine version. It does not show that a sandbox can't be escaped, so the warning not to rely on secbox stays until containment has had its own review.

## Before you start

Use only a throwaway repository and made-up secret values. Never use a real project or a real key.

| # | Do | Ready when |
| --- | --- | --- |
| S1 | Unlock 1Password, and be at the Mac for Touch ID | Prompts can be answered |
| S2 | Stop the sandbox holding port 3000: find it in the app, then Stop | `lsof -nP -iTCP:3000 -sTCP:LISTEN` prints nothing |
| S3 | `apps/desktop/scripts/run-dev.sh` from this checkout, in its own terminal | The window opens, and the daemon says running |
| S4 | `cargo build -p cli -p daemon && export PATH="$PWD/target/debug:$PATH"` in the test terminal | `which secbox` is this checkout, not the older copy on your `PATH` |
| S5 | Create a **private, throwaway** GitHub repository holding the file below on `main` | `git@github.com:YOU/secbox-test.git` can be cloned with your key |
| S6 | Clone it to `~/code/secbox-test` too | Used for the folder cases |

```yaml
# compose.yaml in the throwaway repository
services:
  web:
    image: nginx:1.27-alpine
    ports:
      - "127.0.0.1:3000:80"
  api:
    image: alpine:3.20
    command: ["sleep", "infinity"]
```

```sh
export R=git@github.com:YOU/secbox-test.git   # the remote
export P=~/code/secbox-test                   # the folder
```

## 1. Containment: start here

The cases everything else depends on. Each secret value is made up: use `fake-token-1234`.

`up` starts from a commit, so a project must be the top folder of a Git repository. A fixture inside this checkout is not one, and `up` refuses it at the plan. Copy each fixture into a repository of its own first:

```sh
fixture() {   # fixture NAME: copies tests/fixtures/NAME to ~/code/NAME and commits it
  cp -R "tests/fixtures/$1" ~/code/"$1"
  git -C ~/code/"$1" init -q -b main
  git -C ~/code/"$1" add .
  git -C ~/code/"$1" -c commit.gpgsign=false commit -qm fixture
}
fixture hostile-demo; fixture hostile-demo-run; fixture clean; fixture dockerfile-only
```

### Refused before starting

```sh
secbox secrets set DEMO_TOKEN --service api --project ~/code/hostile-demo
secbox up ~/code/hostile-demo
```

| # | Pass when | Result |
| --- | --- | --- |
| 1.1 | The plan lists four blockers, all naming `evil`, and nothing starts | |

### Hostile project, running

```sh
H=~/code/hostile-demo-run
secbox secrets set DEMO_TOKEN --service api --project $H
secbox up $H
secbox logs api --project $H
secbox net blocked --project $H
secbox status $H
secbox diff $H; echo "exit $?"
```

| # | Pass when | Result |
| --- | --- | --- |
| 1.2 | Every probe prints `BLOCKED`: HTTPS to a denied host, raw IP, metadata, token in DNS, and the disk sweep. `BLOCKED` alone is weak, because those targets would not answer anyway. So also run the control: `secbox exec --service api --project $H -- wget -q -T 5 -O /dev/null https://example.com` and the same for `http://1.1.1.1/` both fail from the sandbox, while `curl -sI https://example.com` and `curl -sI http://1.1.1.1/` succeed from the Mac | |
| 1.3 | The hook probe says `LEAKED`, which is expected because the write inside the sandbox succeeds. The file is not on the Mac: `git -C $H status` is clean | |
| 1.4 | `diff` lists `.claude/settings.json` first as a `FLAG`, and exits 5 | |
| 1.5 | `net blocked` lists `exfil.example.net` and `build-exfil.example.net` | |
| 1.6 | `logs` shows `[DEMO_TOKEN]`, never `fake-token-1234`, including in the base64 the probe sends | |
| 1.7 | `status` lists the secret and every host it could reach under "Where secrets could go" | |
| 1.8 | `grep -rl fake-token-1234 ~/.secbox` and `grep -rl faketoken1234 ~/.secbox` find nothing. The second is the form the DNS probe sends, with the hyphens stripped | |

```sh
secbox down --destroy $H
```

### Attacking a running sandbox

The script needs exactly one running sandbox, so stop any others first. Start the throwaway project with `secbox up "$P" --port 3000`, then:

```sh
python3 -I tests/leak/check-running.py web; echo "exit $?"
```

| # | Pass when | Result |
| --- | --- | --- |
| 1.9 | No `FAIL` lines. Read each `GAP` and `SKIPPED`, because they prove nothing | |
| 1.10 | Raw TCP to a public IP is stopped. `docs/PROTECTIONS.md` lists this as not yet proven | |

## 2. Secret delivery

The audit log shows 0 real deliveries so far. Three design assumptions depend on these cases.

```sh
secbox secrets set FAKE_WEB_ONLY --service web --project "$P"   # type fake-web-5678
secbox down "$P"; secbox up "$P" --port 3000
secbox exec --service web --project "$P" -- sh -c 'env | grep -c FAKE_WEB_ONLY; ls -la /run/secrets 2>&1'
secbox exec --service api --project "$P" -- sh -c 'env | grep -c FAKE_WEB_ONLY; ls -la /run/secrets 2>&1'
secbox logs --project "$P" | grep -c fake-web-5678
```

| # | Pass when | Result |
| --- | --- | --- |
| 2.1 | The start succeeds. If it fails on mounting `tmpfs` or reading standard input, record the message | |
| 2.2 | `web` has the secret | |
| 2.3 | `api` has no variable and no secret file | |
| 2.4 | The value isn't in `logs`, and `grep -rl fake-web-5678 ~/.secbox` finds nothing | |
| 2.5 | After `down` then `up`, `web` has it again. It is not written to the sandbox's disk, so it comes back only by being delivered | |
| 2.6 | The app's Secrets tab: set a value with only `web` ticked. macOS doesn't ask for keychain access, and typing doesn't offer to save a password | |
| 2.7 | The audit log gains `secret_delivered` entries | |

## 3. Git through the relay

The audit log shows 0 real pushes so far. Expect a 1Password prompt at each fetch, clone, pull and push.

```sh
secbox up "$R"
secbox exec --project "$R" -- git log --oneline -3
secbox exec --project "$R" -- sh -c 'env | grep -c SSH_; ls ~/.ssh 2>&1'
secbox exec --project "$R" -- git push origin HEAD:refs/heads/x
secbox exec --project "$R" -- git switch -c secbox-test
secbox exec --project "$R" -- sh -c 'date > secbox-test.txt'
secbox git commit -m "Try the relay" --project "$R"
secbox git push --project "$R"
```

| # | Pass when | Result |
| --- | --- | --- |
| 3.1 | The clone has the real history at the approved commit | |
| 3.2 | No `SSH_` variables, and no `~/.ssh` in the sandbox | |
| 3.3 | `git push` typed inside the sandbox fails, saying it holds no Git credentials | |
| 3.4 | Touch ID names the branch and commit before the push | |
| 3.5 | **The branch is on GitHub, with the commit under your name** | |
| 3.6 | Cancel at Touch ID on a second push: nothing reaches GitHub | |
| 3.7 | Deny in 1Password: it fails quickly with a hint | |

```sh
secbox exec --project "$R" -- git switch main
secbox exec --project "$R" -- sh -c 'date >> README.md'
secbox git commit -m "Should not leave" --project "$R"
secbox git push --project "$R"; echo "exit $?"
```

| # | Pass when | Result |
| --- | --- | --- |
| 3.8 | Refused, exit 1, and `main` on GitHub did not move | |
| 3.9 | Deleting or tagging is refused too: `secbox git run --project "$R" push origin --delete secbox-test` | |
| 3.10 | Commit to `main` on GitHub, then `secbox git pull --project "$R"`: the commit appears, and nothing is recreated. First drop the local "Should not leave" commit | |
| 3.11 | A host not in `known_hosts`, such as a GitLab URL: fails with the hint, and `shasum ~/.ssh/known_hosts` is unchanged | |
| 3.12 | Optional: a large repository clones within the 15-minute limit | |

## 4. Limits and usage

New, and only the engine layer has run on real hardware. Use the throwaway project in the desktop app.

| # | Do | Pass when | Result |
| --- | --- | --- | --- |
| 4.1 | Open Overview while it runs | CPU, memory and disk meters show plausible numbers and update within 10 seconds | |
| 4.2 | `secbox exec --project "$P" -- sh -c 'timeout 30 sh -c "while :; do :; done"'` | The CPU meter climbs | |
| 4.3 | Settings, while running: 6 GB and 4 CPUs, Save | "Restart to apply" appears, and nothing changed yet | |
| 4.4 | Restart to apply | The review shows 6 GB and 4 CPUs. Limits apply after restart, with `nproc` 4 and matching `free -m` | |
| 4.5 | Stay at those limits: change only CPUs, restart | **Works with no disk change.** This is the bug the real engine caught | |
| 4.6 | Change only the disk upward, restart | `df -m /` grows | |
| 4.7 | Files from before all of the above | Still there | |
| 4.8 | Disk below the current size | Refused, saying it can only grow | |
| 4.9 | Memory above "can spare" | Refused, and not saved | |
| 4.10 | Use the project's own, then restart | Back to the project's values, with disk unchanged | |
| 4.11 | The audit log | `resources_chosen` and `sandbox_resized` entries, with no secrets | |

## 5. Touch ID

Only one real confirmation so far.

| # | Do | Pass when | Result |
| --- | --- | --- | --- |
| 5.1 | `secbox up` with the daemon started from a terminal | The prompt names the project and what will happen | |
| 5.2 | Cancel | "Not confirmed", and no sandbox | |
| 5.3 | Daemon started by the app (from Finder if you have a build) | The prompt still appears | |
| 5.4 | Over SSH to this Mac | Refused at once, saying there is no screen to ask on | |
| 5.5 | Leave a prompt for two minutes | It goes away, and the request is refused | |
| 5.6 | Desktop Start and `secbox git push` together | The prompts come one after the other, each with its own reason | |
| 5.7 | Choose the password option at the prompt | The login password works | |

## 6. Lifecycle and ports

```sh
C=~/code/clean
secbox up $C --port 8080
curl -sI http://127.0.0.1:8080 | head -1
lsof -nP -iTCP:8080 -sTCP:LISTEN
secbox status $C
```

| # | Do | Pass when | Result |
| --- | --- | --- | --- |
| 6.1 | The commands above | HTTP 200, bound to `127.0.0.1` only, and services listed correctly from real Compose output | |
| 6.2 | `secbox exec --project $C -- touch /work/marker`, then `down` and `up` | The marker survives, the stack returns, and the sandbox isn't recreated | |
| 6.3 | `down`, then `logs`, `exec` and `shell` | Each refuses, says how to start it, and starts nothing | |
| 6.4 | Port after `down` | Gone | |
| 6.5 | `secbox up ~/code/dockerfile-only --port 3000` | Builds, including the heredoc. 3000 answers, and unapproved 8080 is **closed** | |
| 6.6 | On that project: `logs`, `shell app`, `exec --service app`, then `down` and `up` | Each finds the generated Compose file | |
| 6.7 | `secbox down --destroy $C` | Gone, and `status` says no sandbox | |
| 6.8 | `secbox remove $C` | The project leaves the list, and the folder stays | |

## 7. Reviewing changes

```sh
secbox diff "$P"; echo "exit $?"
secbox exec --project "$P" -- sh -c 'mkdir -p .claude && echo "{\"hooks\":{\"SessionStart\":[{\"command\":\"touch /tmp/ran\"}]}}" > .claude/settings.json'
secbox diff "$P"; echo "exit $?"
secbox exec --project "$P" -- sh -c 'mkdir -p .CLAUDE && cp .claude/settings.json .CLAUDE/Settings.json'
secbox diff "$P" | grep CLAUDE
```

| # | Pass when | Result |
| --- | --- | --- |
| 7.1 | Unchanged sandbox: nothing flagged, exit 0 | |
| 7.2 | The hook is flagged first, exit 5, and it isn't in `$P`. `/tmp/ran` doesn't exist on this Mac | |
| 7.3 | The uppercase copy is flagged too | |
| 7.4 | Replace the sandbox's `.git` with a new history: it says history doesn't start from the snapshot, exit 5 | |
| 7.5 | The app's Changes tab shows the same flags first | |

## 8. AI agents through MCP

```sh
secbox mcp setup claude /tmp/agent-test --project "$P"
cd /tmp/agent-test && claude
```

| # | Do | Pass when | Result |
| --- | --- | --- | --- |
| 8.1 | `/mcp` | Connected, with its tools listed | |
| 8.2 | Ask it to list the files and read the README | It uses the sandbox's tools | |
| 8.3 | Ask it to write a file with spaces in its path | Correct size and content in the sandbox, and not in `$P` | |
| 8.4 | Ask it to run `ls ~` on this machine, then read an absolute host path | Both refused | |
| 8.5 | Ask it to commit and push a branch, then push `main` | The branch needs Touch ID, and `main` is refused | |
| 8.6 | An `exec` with a 2-second timeout on `sleep 100` | It stops, and `ps` in the sandbox shows no `sleep` | |
| 8.7 | An `exec` printing over 256 KiB | Cut short, with no hang | |
| 8.8 | Stop the sandbox, then ask it to run `ls` | Fails, saying it is stopped | |
| 8.9 | Codex: `secbox mcp setup codex --project "$P"`, then 8.1, 8.2 and 8.8 | Its secbox tools work, and the printed start command runs. Codex can still read host files without asking; the setup text must say so | |

## 9. Desktop app and terminal interface

| # | Do | Pass when | Result |
| --- | --- | --- | --- |
| 9.1 | New project, clone from `$R` | The plan appears before anything starts, and Cancel has the focus | |
| 9.2 | Unticking a port in the review | That port stays closed | |
| 9.3 | Changes tab: compare, commit, pull, push | Each works, with the same prompts as the CLI | |
| 9.4 | Network tab, then Approve on a blocked host | Touch ID, data kept, and the host then works | |
| 9.5 | Open in VS Code | Edits the sandbox's files, with nothing copied here | |
| 9.6 | Close the window, then Quit | The app stays in the menu bar on close. After Quit, the daemon and sandboxes keep running | |
| 9.7 | Restart daemon | Sandboxes keep running | |
| 9.8 | `secbox` in a terminal: each tab, `u`, `S`, `e`, and resize to 60 by 14 | Keys match the bottom bar, nothing breaks the frame, and the shell returns cleanly | |
| 9.9 | Settings, Ports: move a port to another number (3001), Save, then Restart to apply | Touch ID names "opening port 3001 (…)". 3001 answers, the old number is closed, and `port_moved` is in the audit log | |
| 9.10 | `secbox ports close 3001`, then `down` and `up` | No Touch ID mention of the port, it is closed, `port_closed` in the audit log | |
| 9.11 | Network tab, Allowed hosts: Block again on an approved host | No Touch ID. Data kept, the host is refused again, its blocked row offers Approve again, and `host_revoked` is in the audit log | |

## 10. Finish

```sh
python3 -I -c "
import json,os,collections
c=collections.Counter(json.loads(l).get('action') for l in open(os.path.expanduser('~/.secbox/daemon-audit.jsonl')))
for k,v in c.most_common(): print(v,k)"
secbox down --destroy "$P"; secbox down --destroy "$R"
secbox secrets rm FAKE_WEB_ONLY --project "$P"
secbox secrets rm DEMO_TOKEN --project ~/code/hostile-demo
secbox secrets rm DEMO_TOKEN --project ~/code/hostile-demo-run
rm -rf /tmp/agent-test
```

Delete the throwaway repository from GitHub. Keep the counts above with your results.

## Not covered tonight

- Linux, Intel Macs and other engine versions.
- A signed `.app`, `.dmg`, Homebrew and launchd. See `docs/RELEASING.md`.
- The full leak suite in `tests/leak` (QUIC, IPv6, DNS volume, domain fronting), and VM escape.
- Whether network policy can change between build and run. This is an open design question in `docs/NEEDS_A_VM.md`, not a pass or fail.
- Hardware keys and SSH agents other than 1Password.
