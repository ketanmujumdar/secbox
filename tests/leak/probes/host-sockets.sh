#!/bin/sh
# Unix sockets belonging to the host that are visible inside the guest. A host
# socket is a hole straight past the network policy: the host's Docker socket
# is root on the host, an SSH agent socket signs for the user, and a D-Bus
# socket reaches the login session.
#
# The control is that the method finds a socket that really is there, so a
# clean result cannot come from a test that never worked.
#
# This probe only looks. It never connects to any socket it finds.
#
# Env: PROBE_SOCKET_CONTROL a path that is a socket here (default: the guest's
#                           own Docker socket)
#      PROBE_HOST_SOCKETS   extra paths to check, space separated
#      PROBE_SOCKET_PATH_LIST replaces the built-in path list, so the self-test
#                           can drive both branches on a machine that has one
#      PROBE_MOUNTS_FILE    the mount table to read (default /proc/mounts)
#      PROBE_ALLOWED_MOUNT  guest path of the permitted mount, if any
. "$(dirname "$0")/lib.sh"

SOCK_OK=0
_ctl=${PROBE_SOCKET_CONTROL:-/var/run/docker.sock}
if [ -S "$_ctl" ]; then
    emit sockets.control REACHED "socket-detected:$_ctl"
    SOCK_OK=1
else
    emit sockets.control SKIPPED "control-failed:no-socket-at:$_ctl"
fi
# This probe runs outside context.sh, so its own control is the gate.
# shellcheck disable=SC2034
PROBE_CONTROL_OK=$SOCK_OK

# Paths that only exist when something of the host's has been passed in. The
# guest's own /var/run/docker.sock is deliberately not in this list.
HOST_SOCKET_PATHS="/var/run/docker.sock.raw /var/run/com.docker.vmnetd.sock
/var/run/mDNSResponder /var/run/docker.sock.host /host/var/run/docker.sock
/run/host-services/ssh-auth.sock /run/host-services/docker.sock
/run/dbus/system_bus_socket /run/systemd/private /var/run/libvirt/libvirt-sock"

_found=
for _p in ${PROBE_SOCKET_PATH_LIST:-$HOST_SOCKET_PATHS} ${PROBE_HOST_SOCKETS:-}; do
    [ -S "$_p" ] && _found="$_found $_p"
done
if [ -n "$_found" ]; then
    emit sockets.host-paths REACHED "host-sockets-present:$_found"
else
    gated_block sockets.host-paths "none-of-the-known-host-sockets-present"
fi

# Anything socket-shaped inside a share that comes from the host, including the
# engine's own guest-state share.
_mounts=${PROBE_MOUNTS_FILE:-/proc/mounts}
if [ ! -r "$_mounts" ]; then
    emit sockets.in-shares SKIPPED "no-proc-mounts"
else
    _dirs=$(awk -v ok="${PROBE_ALLOWED_MOUNT:-}" \
        '$3 ~ /virtiofs|9p|fuse/ && $2 != ok { print $2 }' "$_mounts")
    _hits=$(printf '%s\n' "$_dirs" | while IFS= read -r _d; do
        [ -n "$_d" ] && [ -d "$_d" ] && find "$_d" -maxdepth 3 -type s 2>/dev/null
    done | tr '\n' ' ')
    if [ -n "$_hits" ]; then
        emit sockets.in-shares REACHED "sockets-in-host-shares:$_hits"
    elif [ -z "$_dirs" ]; then
        emit sockets.in-shares SKIPPED "no-host-shares-to-search"
    else
        gated_block sockets.in-shares "no-sockets-in-host-shares"
    fi
fi

# An agent socket handed in would let the guest sign with the user's SSH key.
if [ -z "${SSH_AUTH_SOCK:-}" ]; then
    gated_block sockets.ssh-agent "no-ssh-auth-sock-in-env"
elif [ -S "$SSH_AUTH_SOCK" ]; then
    emit sockets.ssh-agent REACHED "agent-socket-present"
else
    emit sockets.ssh-agent SKIPPED "ssh-auth-sock-set-but-absent"
fi
