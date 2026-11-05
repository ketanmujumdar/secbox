#!/bin/sh
# The engine's guest-state share. The spike found /.msb in the guest is a real
# writable host directory, ~/.secbox/msb/sandboxes/<name>/guest-state, and that
# a symlink the guest makes there becomes a real symlink on the host. That is
# why the build plan forbids host code from following paths under guest state.
#
# The guest cannot see what the host ends up with, so each plant is SKIPPED
# here and tests/leak/verify.py decides from the host side, exactly as it does
# for canaries. plant.control is a plain file: if that one does not appear on
# the host, nothing about the others can be concluded.
#
# Env: PROBE_GUEST_STATE guest path of the share (default /.msb)
#      PROBE_RUN_ID      makes the planted names unique to this run
. "$(dirname "$0")/lib.sh"

_share=${PROBE_GUEST_STATE:-/.msb}
_run=${PROBE_RUN_ID:-0}

# Control: symlinks work in this guest at all, tested somewhere harmless.
SHARE_OK=0
_t=$(mktemp -d 2>/dev/null)
if [ -n "$_t" ] && printf 'x' >"$_t/f" 2>/dev/null && ln -s /etc/hosts "$_t/l" 2>/dev/null &&
    [ -L "$_t/l" ]; then
    emit share.control REACHED "write-and-symlink-work-here"
    SHARE_OK=1
else
    emit share.control SKIPPED "control-failed:cannot-write-or-symlink-in-tmp"
fi
# This probe runs outside context.sh, so its own control is the gate.
# shellcheck disable=SC2034
PROBE_CONTROL_OK=$SHARE_OK
[ -n "$_t" ] && rm -rf "$_t"

if [ ! -d "$_share" ]; then
    emit share.present SKIPPED "no-share-at:$_share"
    emit share.writable SKIPPED "no-share-at:$_share"
    emit share.symlink-created SKIPPED "no-share-at:$_share"
    exit 0
fi
emit share.present SKIPPED "info:engine-share-present:$_share"

_plain=leak-$_run-plain
_abs=leak-$_run-abs
_esc=leak-$_run-esc

if printf 'leak probe %s\n' "$_run" >"$_share/$_plain" 2>/dev/null; then
    emit share.writable REACHED "guest-wrote-into-host-directory:$_share"
    emit plant.control SKIPPED "planted:$_plain"
else
    gated_block share.writable "cannot-write-into:$_share"
fi

# An absolute link, and one that climbs out of the share. On the host both are
# real symlinks; inside the guest they resolve against the guest's own root.
if ln -s /etc/hosts "$_share/$_abs" 2>/dev/null && [ -L "$_share/$_abs" ]; then
    emit share.symlink-created REACHED "symlink-made-in-host-directory:$_abs"
    emit plant.symlink-abs SKIPPED "planted:$_abs"
else
    gated_block share.symlink-created "cannot-symlink-in:$_share"
fi
if ln -s ../../../../../../../../etc/passwd "$_share/$_esc" 2>/dev/null; then
    emit plant.symlink-traversal SKIPPED "planted:$_esc"
fi
