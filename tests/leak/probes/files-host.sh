#!/bin/sh
# Host files visible in the guest outside the permitted mount.
# Env: PROBE_FILE_CONTROL  file that must be visible (positive control)
#      PROBE_HOST_CANARY   absolute path of a canary file created on the host
#      PROBE_ALLOWED_MOUNT guest path of the permitted mount, if any
. "$(dirname "$0")/lib.sh"

if [ -n "${PROBE_FILE_CONTROL:-}" ] && [ -r "$PROBE_FILE_CONTROL" ]; then
    emit files.control REACHED "$PROBE_FILE_CONTROL"
    PROBE_CONTROL_OK=1
else
    emit files.control SKIPPED "control-failed-or-not-configured"
    PROBE_CONTROL_OK=0
fi

if [ -z "${PROBE_HOST_CANARY:-}" ]; then
    emit files.canary SKIPPED "canary-not-configured"
elif [ -e "$PROBE_HOST_CANARY" ]; then
    emit files.canary REACHED "$PROBE_HOST_CANARY"
else
    gated_block files.canary "absent:$PROBE_HOST_CANARY"
fi

_found=
for _p in /Users /Volumes /Applications /System/Library /private/etc; do
    [ -e "$_p" ] && _found="$_found $_p"
done
if [ -n "$_found" ]; then
    emit files.macos-paths REACHED "$_found"
else
    gated_block files.macos-paths "none-present"
fi

if [ ! -r /proc/mounts ]; then
    emit files.host-shares SKIPPED "no-proc-mounts"
else
    _shares=$(awk -v ok="${PROBE_ALLOWED_MOUNT:-}" \
        '$3 ~ /virtiofs|9p|fuse/ && $2 != ok && $2 != "/.msb" { printf "%s(%s) ", $2, $1 }' /proc/mounts)
    # /.msb is the engine's own share. It is a writable host directory, so it is
    # reported on its own line and not hidden.
    if grep -q ' /.msb ' /proc/mounts; then
        emit files.runtime-share SKIPPED "info:engine-share-at-/.msb:$(grep ' /.msb ' /proc/mounts | awk '{print $4}' | cut -d, -f1)"
    fi
    if [ -n "$_shares" ]; then
        emit files.host-shares REACHED "$_shares"
    else
        gated_block files.host-shares "no-unexpected-shares"
    fi
fi
