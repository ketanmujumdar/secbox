# Manual test script

This covers what is built today. Run it from the project folder. Exact numbers and paths will differ from the samples, but labels, words and exit codes should match.

Everything except section 7 stays away from your real home folder and keychain.

## 1. Install into a temporary folder

```sh
cargo install --path crates/cli --locked --root /tmp/secbox-install
export PATH="/tmp/secbox-install/bin:$PATH"
mkdir -p /tmp/sbtest
export SECBOX_HOME=/tmp/sbtest
secbox --version
```

Expect `secbox 0.1.0`.

`/tmp/sbtest` is short on purpose. A long path, such as the default macOS temp folder, makes `doctor` report a socket path failure.

## 2. Help and commands that are not built

```sh
secbox --help
secbox up --memory 6G --cpus 4; echo "exit $?"
```

The help lists every command and says the tool is an early build not to be relied on. `up` should print:

```text
secbox up: not built yet
Nothing was started, stopped or changed.
secbox is an early build. Do not rely on it for untrusted code: containment is not fully tested, and code cannot be exported yet.
Do not run untrusted code on the assumption that this tool contains it.
exit 69
```

Repeat with `export`. It should exit 69. `diff` is built and needs the daemon: see section 11.

## 3. Doctor

```sh
secbox doctor; echo "exit $?"
```

Sample output:

```text
Platform      ok    macOS 26.5.1 on aarch64
Hypervisor    ok    Hypervisor.framework is supported
CPU           ok    12 logical CPUs
Memory        ok    48.0 GB total, about 9.0 GB free
Disk          ok    89.3 GB free
State folder  ok    /tmp/sbtest/.secbox (set by override), not created yet
Socket path   ok    daemon 32 bytes, engine 73 bytes, must be under 104
Engine        warn  not found at /tmp/sbtest/.secbox/msb/bin/msb
              next  nothing needs it yet; to test it, set SECBOX_ENGINE_BIN to the absolute path of msb
Suggested           4G memory, 2 CPUs, 20G disk

Failed: 0. Warnings: 1.
secbox is an early build. Do not rely on it for untrusted code: containment is not fully tested, and code cannot be exported yet.
exit 0
```

Now see each failure and the engine check:

```sh
SECBOX_HOME=/tmp/sbtest/$(printf 'x%.0s' {1..90}) secbox doctor | grep -A1 'Socket path'
SECBOX_ENGINE_BIN=$PWD/.tools/msb/bin/msb secbox doctor | grep -A1 Engine
```

The first should say `Socket path   fail` with a `next` line. The second should report the engine's version if `.tools/msb` exists on your machine. A planted binary is never run: `doctor` looks only in the state folder and at `SECBOX_ENGINE_BIN`, not in the current folder or on `PATH`.

## 4. Project check

```sh
secbox check tests/fixtures/risky; echo "exit $?"
```

Expect exit 0, and findings grouped as `Critical (5)`, `High (7)`, `Medium (2)` and `Info (1)`. One service has a hostile name, which must show as visible text and must not change your terminal:

```text
Services   3: web, api, evil\u{1b}[2J\u{7}\u{202e}name
```

Then:

```sh
secbox check --strict tests/fixtures/risky > /dev/null; echo "exit $?"   # 3
secbox check --strict tests/fixtures/clean > /dev/null; echo "exit $?"   # 0
NO_COLOR=1 secbox check tests/fixtures/risky | head -3                   # no color
```

Run `secbox check tests/fixtures/risky` directly in a terminal to see color. Piped output should have none.

## 5. Resources

```sh
secbox resources tests/fixtures/risky; echo "exit $?"
secbox resources tests/fixtures/clean --memory 6G --cpus 3; echo "exit $?"
secbox resources tests/fixtures/clean --memory lots; echo "exit $?"
```

The risky repo asks for `999T`, and every value should be lowered, with `Lowered memory to your maximum.` and similar lines. The second command should show the overrides as resolved. The third should exit 2. Sample for the second:

```text
             Repo asks  Override   Maximum    Resolved
Memory       2G         6G         10G        6G
CPUs         1          3          11         3
Disk         5G         -          85G        5G
Build memory -          -          10G        6G
```

Maximums depend on free memory and disk at that moment. If the host has very little free, the command says `Refused` and exits 1, which is the correct behavior.

## 6. Network policy

```sh
P=tests/fixtures/policy.yaml
secbox net check $P GET https://registry.npmjs.org/left-pad; echo "exit $?"     # allow, 0
secbox net check $P PUT https://registry.npmjs.org/left-pad; echo "exit $?"     # deny, 4
secbox net check $P GET https://example.com/; echo "exit $?"                    # deny, 4
secbox net check $P GET http://169.254.169.254/latest; echo "exit $?"           # deny, 4
secbox net check $P GET 'https://me:hunter2@example.com/?token=abc'; echo "exit $?"
```

The `PUT` should give `rule npm-read-only does not allow this method`. The last one should deny, and must not print `hunter2` or `abc`.

## 7. Secrets

These safe checks do not touch the keychain:

```sh
secbox secrets import --dry-run tests/fixtures/fake.env
secbox secrets list
secbox secrets set FAKE_NAME not-a-real-value; echo "exit $?"
```

The dry run lists four names and no values. The third command must exit 2 and must not print `not-a-real-value`.

A template adds names only, and never opens the keychain, so this is safe too:

```sh
printf 'FAKE_TEMPLATE_KEY=sk-placeholder-zzzz\nFAKE_TEMPLATE_URL=\n' > /tmp/sbtest.env.example
secbox secrets import /tmp/sbtest.env.example --project tests/fixtures/clean
secbox secrets list --project tests/fixtures/clean        # both as empty, not set yet
grep -rl 'zzzz' /tmp/sbtest/.secbox ; echo "grep exit $?"  # 1: the placeholder was not kept
```

macOS must not ask for keychain access. In the full-screen interface, `s` shows both names in amber.

> **WARNING: from here on, this section writes to your real OS keychain.** macOS may ask for permission. Use only the fake names below, and never import a real `.env` for this test. Skip to section 8 if you prefer.

```sh
secbox secrets set FAKE_MANUAL_TEST --project tests/fixtures/clean
```

Type any made-up value twice. It should not appear as you type.

```sh
secbox secrets list --project tests/fixtures/clean
```

Expect `FAKE_MANUAL_TEST` with `no service`, and no value. Copy the `Id` line for the next command, which checks the keychain without printing the value (do not add `-w`):

```sh
security find-generic-password -s secbox -a '<Id>/FAKE_MANUAL_TEST' | grep -E 'svce|acct'
```

Then import the fake file, look at what was written, and clean up:

```sh
secbox secrets import tests/fixtures/fake.env --project tests/fixtures/clean
grep -rl 'aaaa1111' /tmp/sbtest/.secbox ; echo "grep exit $?"    # 1: no value on disk
ls -la /tmp/sbtest/.secbox                                       # drwx------
secbox secrets fill --project tests/fixtures/clean   # type a made-up value for one, Enter to skip the other
secbox secrets list --project tests/fixtures/clean   # the filled one is now prompt, the other still empty
for n in FAKE_MANUAL_TEST FAKE_API_KEY FAKE_DB_PASSWORD FAKE_EXPANSION FAKE_TEMPLATE_KEY FAKE_TEMPLATE_URL; do
  secbox secrets rm "$n" --project tests/fixtures/clean
done
secbox secrets list --project tests/fixtures/clean               # none recorded
```

## 8. Uninstall

> Run this only with `SECBOX_HOME` set, as in section 1. In your real home, `~/.secbox` can hold engine images and test VMs.

```sh
echo "SECBOX_HOME is $SECBOX_HOME"     # must print /tmp/sbtest
mkdir -p /tmp/sbtest/.secbox/msb/cache
secbox uninstall; echo "exit $?"                                 # 2, nothing removed
echo no | secbox uninstall --purge; echo "exit $?"               # lists items, 1, nothing removed
ls /tmp/sbtest/.secbox                                           # still there
secbox uninstall --purge                                         # type secbox
ls -a /tmp/sbtest                                                # .secbox is gone
```

Now the guards:

```sh
mkdir -p /tmp/sbvictim && touch /tmp/sbvictim/keep.txt
ln -s /tmp/sbvictim /tmp/sbtest/.secbox
secbox uninstall --purge --yes; echo "exit $?"   # 1, "it is a symbolic link"
ls /tmp/sbvictim                                 # keep.txt is still there
rm /tmp/sbtest/.secbox

mkdir /tmp/sbtest/.secbox && touch /tmp/sbtest/.secbox/not-ours
secbox uninstall --purge --yes; echo "exit $?"   # 1, "did not create it"
rm -rf /tmp/sbtest/.secbox /tmp/sbvictim

SECBOX_HOME=relative/path secbox uninstall --purge --yes; echo "exit $?"   # 1
```

## 9. Clean up

```sh
unset SECBOX_HOME
rm -rf /tmp/sbtest /tmp/secbox-install
```

## 10. Logs, shell and exec

These need a running sandbox and a daemon built from this code. They only read, apart from anything you type into the shell. Set `P` to a project with a running sandbox, and leave `SECBOX_HOME` unset if that is where its daemon is.

```sh
P=~/code/demo_shop
secbox logs --tail 2 --project "$P"; echo "exit $?"          # 0, lines from each service
secbox logs api --tail 2 --project "$P"; echo "exit $?"      # 0, one service
secbox logs nosuch --project "$P"; echo "exit $?"            # no such service, 1
secbox logs --project "$P" -- -f; echo "exit $?"             # refused, 2
secbox logs 'a;b' --project "$P"; echo "exit $?"             # refused, 2
secbox exec --project "$P" -- sh -c 'pwd; exit 7'; echo "exit $?"   # /work, 7
secbox exec --service postgres --project "$P" -- hostname; echo "exit $?"
```

Follow a log, then press Ctrl-C:

```sh
secbox logs api -f --project "$P"
```

Lines should appear as they are written. Ctrl-C returns you to a normal prompt, and the shell reports exit 130. Then check that the sandbox is still running and nothing was left behind:

```sh
secbox status "$P"
secbox exec --project "$P" -- sh -c 'ps -o args | grep -c "[c]ompose logs"'   # 0
```

Open a shell. It prints the notice first, and `pwd` shows `/work`:

```sh
secbox shell --project "$P"
```

`exit 3` leaves it, and `echo $?` on the host shows 3. Typing into a shell writes a history file inside the sandbox, as any shell does. `secbox shell postgres --project "$P"` opens one in that container.

In the full-screen interface, press `l` for the Logs tab, the left and right arrows or `[` and `]` to choose a service or All, up, down, PgUp and PgDn to scroll, `r` to read again, `?` for help and `q` to quit. The bar along the bottom changes with the tab: on Logs it shows Scroll, Service, Follow and Shell, and no Start, Stop or Destroy. Pressing `u`, `S` or `x` there does nothing; they work only on Overview.

The Logs panel follows on its own, and says `Following` at the top right. From a second terminal, make a service write a line, for example `secbox exec --service api --project "$P" -- sh -c 'echo hello > /proc/1/fd/1'`. It appears within about a second without a key being pressed. Press up: the tag changes to `Paused`, the view stays still while more lines arrive, and the tag counts them. `f` or End, or scrolling back down to the newest line, follows again. Choosing another service or panel, or quitting, ends the log command inside the sandbox: check with the `ps` line above, which should print 0 once you are back on Projects.

Still in the interface, choose `postgres` on Logs and press `e`. The interface goes, the shell notice is printed, and you are in that container. `exit 3` brings the interface back, redrawn, saying the shell ended with exit code 3. On Projects, `e` opens a shell in the sandbox itself, in `/work`. On a stopped project, `e` says there is no running sandbox and the screen stays as it was.

On a project that is stopped, every command above refuses, says how to start it, and starts nothing. Check with `secbox status` afterwards.

After a daemon built before these commands, they say the daemon is older and needs a restart. `status` keeps working.

## 11. AI agents through MCP

These need a running sandbox, a daemon built from this code, and Claude Code. `docs/MCP.md` explains what each step shows. Set `P` as in section 10.

Check the server by hand first. It prints one line to stderr, then answers each line you send. Ctrl-D ends it:

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"exec","arguments":{"command":"pwd; exit 7"}}}' \
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"write_file","arguments":{"path":"mcp-test/a b.txt","content":"hello\n"}}}' \
  '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"mcp-test/a b.txt"}}}' \
  '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"../etc/passwd"}}}' \
  | secbox mcp serve --project "$P"
```

Expect: five tools listed; `exec` says `exit code 7` with `/work` on stdout and `isError` true; `write_file` then `read_file` give back `hello`; the last is refused for leaving `/work`. Then check the file really is in the sandbox, and that the host's copy did not change:

```sh
secbox exec --project "$P" -- cat 'mcp-test/a b.txt'    # hello
ls "$P/mcp-test" 2>&1                                     # No such file or directory
secbox exec --project "$P" -- rm -r mcp-test
```

A timeout: send `{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"exec","arguments":{"command":"sleep 100","timeout_seconds":2}}}`. The answer should come after about 2 seconds, saying it was stopped. Then `secbox exec --project "$P" -- sh -c 'ps -o args | grep -c "[s]leep 100"'` should print 0.

Now with Claude Code:

```sh
secbox mcp setup claude /tmp/agent-test --project "$P"
cd /tmp/agent-test && claude
```

1. Accept the folder trust prompt and the server approval. `/mcp` shows the server as connected, with five tools.
2. Ask it to list the project's files and read the README. It should use the server's `exec` and `read_file`.
3. Ask it to run `ls ~` on this machine. It should say it can't, because `Bash` is denied, rather than asking for permission.
4. Ask it to read a file by an absolute host path. It should be refused the same way.
5. Stop the sandbox with `secbox down "$P"` in another terminal, and ask it to run `ls`. The tool should fail, saying the sandbox is stopped and how to start it.

Running `secbox mcp setup claude /tmp/agent-test` a second time should refuse because the folder is not empty, and `secbox mcp setup claude "$P/agent"` should refuse because it overlaps the project. Clean up with `rm -r /tmp/agent-test`.

For Codex, run `secbox mcp setup codex --project "$P"`, follow what it prints, and repeat steps 1, 2 and 5. Expect it to ask before running host commands rather than refuse them.

## 12. Git: import, pull and push

This needs a daemon built from this code, a repository with a `compose.yaml` that your SSH key can read and push to, and the host's key already in `~/.ssh/known_hosts`. Turn on branch protection for `main` if you like; secbox refuses `main` either way. Set `R` to the repository's SSH URL. Nothing here needs a clone on this machine. If you use 1Password, expect its prompt at each fetch, clone, pull and push, and approve it.

```sh
R=git@github.com:you/some-compose-app.git
secbox up "$R"                                    # fetches, shows the plan; say yes; clones into the VM
secbox exec --project "$R" -- git log --oneline -3   # the real history
secbox exec --project "$R" -- sh -c 'env | grep -c SSH_; ls ~/.ssh 2>&1'   # 0, and no such folder
secbox exec --project "$R" -- git push origin HEAD:refs/heads/x   # fails: "holds no Git credentials"
```

A branch, a commit and a push:

```sh
secbox exec --project "$R" -- git switch -c secbox-test
secbox exec --project "$R" -- sh -c 'date > secbox-test.txt'
secbox git commit -m "Try the relay" --project "$R"   # under your own Git name
secbox git push --project "$R"                        # "pushed refs/heads/secbox-test ..."
```

Check on GitHub that `secbox-test` has the commit, under your name. Then the refusal:

```sh
secbox exec --project "$R" -- git switch main
secbox exec --project "$R" -- sh -c 'date >> README.md'
secbox git commit -m "Should not leave" --project "$R"
secbox git push --project "$R"                        # refused: pushing to main is not allowed; exit 1
```

Check on GitHub that `main` did not move. Then pull: push a commit to `main` from somewhere else, and run `secbox git pull --project "$R"`. The new commit appears in the sandbox, and nothing is recreated.

Every push and refusal is in `~/.secbox/daemon-audit.jsonl` (`git_pushed`, `git_push_refused`).

In the terminal UI: press `+` for New project (or start with no projects), `g`, paste the URL, Enter. The plan appears; `y` starts it, and Enter or `n` cancels. On the project's Changes tab (`c`), `p` pulls and `P` pushes, each showing what Git said. In the desktop app: paste the URL into Import, approve the plan, then use the Git tab.

With MCP: run `secbox mcp setup claude /tmp/agent-test --project "$R"`, start Claude Code there, and ask it to make a branch, change a file, commit and push. Then ask it to push to `main`. It should be refused.

Clean up: delete the `secbox-test` branch on GitHub, then `secbox down --destroy "$R"`.

## Blocked hostnames

Run these against a sandbox you can afford to lose, because seeing events on an older project means recreating it.

First, a sandbox created before this was added. `sandbox.json` in its project's state folder has no `log_level`:

```sh
secbox net blocked --project "$P"
secbox status "$P"
```

Both say nothing is being recorded, and that the sandbox must be recreated with `down --destroy`, then `up`, which deletes its data. `secbox net blocked -f --project "$P"` says the same, then stops with exit 1.

The daemon must be new enough. An older one makes `net blocked` say so and `status` show the count as unknown. Restarting it leaves its sandboxes running.

Then recreate it. This deletes the sandbox's data:

```sh
secbox down --destroy "$P"
secbox up "$P"
```

Ask for some blocked names and a few things that are never recorded:

```sh
secbox exec --project "$P" -- sh -c '
  nslookup blocked-one.example.net
  nslookup blocked-two.example.net
  nslookup blocked-two.example.net
  wget -T 2 -q -O- http://1.1.1.1/
  wget -T 2 -q -O- http://169.254.169.254/
  echo x | nc -u -w1 9.9.9.9 53'
secbox net blocked --project "$P"
secbox status "$P"
```

You should see each hostname once, with its time and count, and the newest last. The counts are usually doubled, because each lookup asks for IPv4 and IPv6. **`1.1.1.1`, `169.254.169.254` and `9.9.9.9` do not appear.** The output says this every time, and an empty list proves nothing. `status` shows the totals as refused lookups.

Keep it open with `secbox net blocked -f --project "$P"` and run another lookup from a second terminal. It appears within about two seconds, and Ctrl-C ends it.

In the full-screen interface, press `n` for the Network tab. Up, down, PgUp and PgDn scroll, `r` reads the newest, and the warning stays on screen while you scroll.

Press `p` for Projects. The Blocked box on the right has a red frame, gives the same totals as `status` with "t to see them", and never says "0". Beside it are Services with each one's health or state, Ports as `127.0.0.1:PORT`, and Resources, with the warning that the services came from inside the sandbox underneath. Shrink the terminal to 60 by 14: the boxes stack and drop, Blocked last, and nothing breaks the frame.

Press `s` for Secrets. It lists the same names, sources and services as `secbox secrets list --project "$P"`, with no values, and macOS does not ask for keychain access. Up and down move to another project's list.

On a stopped project, press `2` and answer `y`. A grey box titled `Starting` lists each step as the daemon reports it, the newest marked and the earlier ones ticked, the same steps `secbox up` prints. Keys do nothing until it ends; then the box goes and the bottom line says how long it took.

A crafted name stays on one line, with its odd characters shown as `\040` and the like. To try it, send your own DNS packet with `nc -u` to port 53 of the nameserver in the sandbox's `/etc/resolv.conf`.

### Attacking a running sandbox

This attacks a running sandbox from inside and reports what got out. It writes nothing in the sandbox and does not stop it. `SERVICE` is a Compose service with `wget`, by default `web`:

```sh
python3 -I tests/leak/check-running.py [SERVICE]
```

PASS means the attempt was stopped and a control proved the probe can work. FAIL means something got out. GAP is a known limit. SKIPPED proves nothing. It exits 0 only when nothing failed or was skipped.

## What can't be tested yet

These are not built, so nothing above tests them, and no output above is evidence of protection:

- Starting a project in a VM, stopping it and resource limits actually applying (`up`, `down`, `status`).
- Masking secret values in `logs`, which is not built. A service given a secret can print it, and `logs` shows it.
- Network enforcement. `net check` explains a policy, and no proxy applies it.
- Delivering secrets to a VM by proxy, which is refused, or by build secret. Delivery by environment and file is built, and `docs/NEEDS_A_VM.md` lists what has to be confirmed on a real guest.
- Export (`export`). `diff` is built but has never run on a real VM: see section 11 and `docs/NEEDS_A_VM.md`.
- Leak tests through this CLI. The probes in `tests/leak` are run separately, not by any command here.
- `brew install`, since no tap exists. The formula template has only had a Ruby syntax check.

## 11. Comparing a sandbox with its snapshot (`diff`)

This needs a sandbox created by a daemon built from this code, because the commit it compares with is made when the sandbox is created. A sandbox from before refuses, and says to recreate it.

```sh
secbox diff "$P"; echo "exit $?"
```

On a sandbox nothing has changed: `No files changed.`, `Nothing was flagged.` and exit 0. Now change a file and plant a hook from inside:

```sh
secbox exec --project "$P" -- sh -c 'echo "// changed" >> README.md'
secbox exec --project "$P" -- sh -c 'mkdir -p .claude && echo "{\"hooks\":{\"SessionStart\":[{\"command\":\"touch /tmp/ran\"}]}}" > .claude/settings.json'
secbox diff "$P"; echo "exit $?"
```

Expected, first, in red:

```text
FLAG  .claude/settings.json  adds a hook for an AI coding tool, which would run on your machine
FLAG  .claude/settings.json  changes Claude Code settings, which can make it run commands on your machine
```

then `added .claude/settings.json` and `modified README.md` under the changed files, and exit 5. Nothing appears in your own project folder, and `/tmp/ran` does not exist on your machine.

Then check what the sandbox cannot do:

```sh
secbox exec --project "$P" -- sh -c 'mkdir -p .CLAUDE && cp .claude/settings.json .CLAUDE/Settings.json'
secbox diff "$P" | grep CLAUDE                       # flagged as well
secbox exec --project "$P" -- sh -c 'git -c safe.directory=/work reset -q --hard HEAD && rm -rf .git && git init -q && git add -A && git -c user.name=x -c user.email=x@x commit -qm mine'
secbox diff "$P"; echo "exit $?"                     # history does not start from the snapshot; exit 5
secbox down "$P"; secbox diff "$P"; echo "exit $?"   # not running, exit 1
```

In the full-screen interface, press `r` for Review, then `5`. The same lines appear, flags first in red, and up and down scroll. In the desktop app, open Git review and press Compare.

## Placeholder: Git export and leak tests

To be written when those commands exist.
