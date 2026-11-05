#!/bin/sh
# Entry point inside the guest: copy this directory in and run it there.
# See lib.sh for the output contract. Do not run on a host with a Docker daemon
# you care about unless uname is not Linux: docker_ready then refuses to use it.
_dir=$(cd "$(dirname "$0")" && pwd)
# Nested contexts need the guest's own gateway, not their docker bridge.
if [ -z "${PROBE_GUEST_GATEWAY:-}" ] && command -v ip >/dev/null 2>&1; then
    PROBE_GUEST_GATEWAY=$(ip route show default 2>/dev/null | awk '/default/ {print $3; exit}')
    export PROBE_GUEST_GATEWAY
fi
PROBE_PREFIX=guest.
export PROBE_PREFIX
# Host access. These look at the guest itself, so they run once, here, and not
# in every nested context.
sh "$_dir/files-host.sh"
sh "$_dir/host-sockets.sh"
sh "$_dir/host-keychain.sh"
sh "$_dir/host-share.sh"
sh "$_dir/git-bundle.sh"
sh "$_dir/context.sh"
unset PROBE_PREFIX
sh "$_dir/nested-container.sh"
sh "$_dir/nested-build.sh"
