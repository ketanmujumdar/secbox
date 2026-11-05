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

Repeat with `down`, `status`, `diff` and `export`. Each should exit 69.

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
for n in FAKE_MANUAL_TEST FAKE_API_KEY FAKE_DB_PASSWORD FAKE_EXPANSION; do
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

## What can't be tested yet

These are not built, so nothing above tests them, and no output above is evidence of protection:

- Starting a project in a VM, stopping it and resource limits actually applying (`up`, `down`, `status`).
- Network enforcement. `net check` explains a policy, and no proxy applies it.
- Delivering secrets to a VM by proxy, injection or build secret. Assignments are only recorded.
- Git review and export (`diff`, `export`).
- Leak tests through this CLI. The probes in `tests/leak` are run separately, not by any command here.
- `brew install`, since no tap exists. The formula template has only had a Ruby syntax check.

## Placeholder: VM flows, Git export and leak tests

To be written when those commands exist.
