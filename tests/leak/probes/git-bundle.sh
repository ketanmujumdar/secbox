#!/bin/sh
# A malicious hook inside a Git bundle produced in the VM.
#
# Milestone 3 gets code out of the VM as a bundle, and the build plan's rule is
# that host code never runs Git in a .git written by the VM. A bundle carries
# commits, so it cannot carry .git/hooks directly, but it can carry everything
# that makes the host run code once it is unpacked or opened: a hooks directory
# the repo points core.hooksPath at, a .gitattributes filter driver, and a
# VS Code task set to run on folder open.
#
# The probe builds such a bundle in the guest and leaves it in the engine's
# guest-state share, where the host can see it. Whether it arrived is decided
# on the host by tests/leak/verify.py, so the plant line is SKIPPED here.
#
# Nothing here pushes anywhere and nothing is run: the payload scripts only
# echo, so a stray execution is visible rather than harmful.
#
# Env: PROBE_GUEST_STATE guest path of the share (default /.msb)
#      PROBE_RUN_ID      makes the bundle name unique to this run
. "$(dirname "$0")/lib.sh"

if ! have git; then
    emit bundle.control SKIPPED "no-git"
    emit bundle.created SKIPPED "no-git"
    emit bundle.hook-in-tree SKIPPED "no-git"
    exit 0
fi

_run=${PROBE_RUN_ID:-0}
_w=$(mktemp -d 2>/dev/null) || { emit bundle.control SKIPPED "no-tmpdir"; exit 0; }
GIT="git -c user.name=leak-probe -c user.email=leak-probe@invalid -c init.defaultBranch=main
     -c commit.gpgsign=false -c protocol.file.allow=never"

cd "$_w" || { emit bundle.control SKIPPED "cannot-enter-tmpdir"; exit 0; }
# shellcheck disable=SC2086
if ! $GIT init -q repo >/dev/null 2>&1; then
    emit bundle.control SKIPPED "git-init-failed"
    rm -rf "$_w"
    exit 0
fi
emit bundle.control REACHED "git-usable-here"

cd "$_w/repo" || exit 0
mkdir -p .githooks .vscode
cat >.githooks/post-checkout <<'HOOK'
#!/bin/sh
echo "LEAK-PROBE-HOOK-RAN post-checkout"
HOOK
cat >.githooks/pre-commit <<'HOOK'
#!/bin/sh
echo "LEAK-PROBE-HOOK-RAN pre-commit"
HOOK
chmod +x .githooks/post-checkout .githooks/pre-commit
printf '* filter=leakprobe diff=leakprobe\n' >.gitattributes
cat >.vscode/tasks.json <<'JSON'
{"version":"2.0.0","tasks":[{"label":"leak-probe","type":"shell",
 "command":"echo LEAK-PROBE-TASK-RAN","runOptions":{"runOn":"folderOpen"}}]}
JSON
printf 'leak probe %s\n' "$_run" >README

# core.hooksPath lives in .git/config, which a bundle does not carry. It is set
# here so the repo is the shape an attacker would hand over, and so a host-side
# check has something to find if the whole .git is ever copied out instead.
# shellcheck disable=SC2086
$GIT config core.hooksPath .githooks >/dev/null 2>&1
# shellcheck disable=SC2086
$GIT add -A >/dev/null 2>&1
# shellcheck disable=SC2086
if ! $GIT commit -q -m "leak probe payload" >/dev/null 2>&1; then
    emit bundle.created SKIPPED "git-commit-failed"
    emit bundle.hook-in-tree SKIPPED "git-commit-failed"
    cd /; rm -rf "$_w"
    exit 0
fi

_name=leak-$_run.bundle
# shellcheck disable=SC2086
if $GIT bundle create "$_w/$_name" HEAD >/dev/null 2>&1 && [ -s "$_w/$_name" ]; then
    emit bundle.created REACHED "bundle-built-in-guest:$_name"
else
    emit bundle.created SKIPPED "git-bundle-create-failed"
    cd /; rm -rf "$_w"
    exit 0
fi

# shellcheck disable=SC2086
_tree=$($GIT ls-tree -r --name-only HEAD 2>/dev/null)
_carried=
for _f in .githooks/post-checkout .githooks/pre-commit .gitattributes .vscode/tasks.json; do
    printf '%s\n' "$_tree" | grep -qxF "$_f" && _carried="$_carried $_f"
done
if [ -n "$_carried" ]; then
    emit bundle.hook-in-tree REACHED "payload-travels-in-the-bundle:$_carried"
else
    emit bundle.hook-in-tree SKIPPED "payload-not-in-the-committed-tree"
fi

_share=${PROBE_GUEST_STATE:-/.msb}
if [ -d "$_share" ] && cp "$_w/$_name" "$_share/$_name" 2>/dev/null; then
    emit plant.bundle SKIPPED "planted:$_name"
else
    emit bundle.plant SKIPPED "no-share-to-leave-the-bundle-in:$_share"
fi
cd /
rm -rf "$_w"
