#!/bin/sh
# The host's credential store seen from inside the guest: macOS Keychain files,
# Linux keyring files, the tools that read them, and the D-Bus session socket
# the Secret Service answers on.
#
# This probe only tests whether these exist. It never runs `security`, never
# runs `secret-tool` and never opens a keychain file, because the build plan's
# rule is that automated tests never touch the real keychain.
#
# Env: PROBE_KEYCHAIN_CONTROL   a path that exists here, proving the method works
#                               (defaults to PROBE_FILE_CONTROL)
#      PROBE_KEYCHAIN_PATHS     extra paths to check, space separated
#      PROBE_KEYCHAIN_PATH_LIST replaces the built-in path list
#      PROBE_KEYCHAIN_TOOL_LIST replaces the built-in tool list
# The two list overrides exist so the self-test can drive both branches on a
# machine that really does have a keychain.
. "$(dirname "$0")/lib.sh"

KC_OK=0
_ctl=${PROBE_KEYCHAIN_CONTROL:-${PROBE_FILE_CONTROL:-}}
if [ -n "$_ctl" ] && [ -e "$_ctl" ]; then
    emit keychain.control REACHED "path-test-works:$_ctl"
    KC_OK=1
else
    emit keychain.control SKIPPED "control-failed-or-not-configured"
fi
# This probe runs outside context.sh, so its own control is the gate.
# shellcheck disable=SC2034
PROBE_CONTROL_OK=$KC_OK

KEYCHAIN_PATHS="/Library/Keychains/System.keychain
/Library/Keychains/System.keychain-db
/private/var/db/SystemKey
/Users /System/Library/Keychains
$HOME/Library/Keychains
$HOME/.local/share/keyrings
$HOME/.gnupg/private-keys-v1.d
$HOME/.ssh/id_rsa
$HOME/.ssh/id_ed25519
$HOME/.docker/config.json
$HOME/.netrc
$HOME/.aws/credentials"

_found=
for _p in ${PROBE_KEYCHAIN_PATH_LIST:-$KEYCHAIN_PATHS} ${PROBE_KEYCHAIN_PATHS:-}; do
    [ -e "$_p" ] && _found="$_found $_p"
done
if [ -n "$_found" ]; then
    emit keychain.paths REACHED "credential-stores-present:$_found"
else
    gated_block keychain.paths "no-credential-store-paths-present"
fi

_tools=
for _t in ${PROBE_KEYCHAIN_TOOL_LIST:-security secret-tool gnome-keyring-daemon kwalletcli pass}; do
    have "$_t" && _tools="$_tools $_t"
done
if [ -n "$_tools" ]; then
    emit keychain.tools REACHED "readers-present-not-run:$_tools"
else
    gated_block keychain.tools "no-credential-reader-installed"
fi

# The Secret Service lives behind the session bus, so the socket alone is reach.
if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
    gated_block keychain.dbus "no-session-bus-in-env"
else
    _sock=${DBUS_SESSION_BUS_ADDRESS#*path=}
    _sock=${_sock%%,*}
    if [ -S "$_sock" ]; then
        emit keychain.dbus REACHED "session-bus-socket-present"
    else
        emit keychain.dbus SKIPPED "session-bus-set-but-socket-absent"
    fi
fi
