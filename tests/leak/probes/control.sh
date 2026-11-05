#!/bin/sh
# Positive and negative controls for this context. A failed control is SKIPPED,
# never BLOCKED, so a parser cannot count it as a pass.
#   control.local   the method reports success against a listener we started
#   control.closed  the method reports failure against a closed port
#   control.allowed the same method reaches PROBE_CONTROL_TARGET through this
#                   context's real network path, which policy must allow
# Env: PROBE_CONTROL_TARGET=host:port
. "$(dirname "$0")/lib.sh"

_lport=${PROBE_LOCAL_PORT:-47911}
_cport=${PROBE_CLOSED_PORT:-47912}
_pid=

start_listener() {
    have nc || return 1
    for _args in "-l -p $_lport" "-l $_lport"; do
        # shellcheck disable=SC2086
        nc $_args </dev/null >/dev/null 2>&1 &
        _pid=$!
        sleep 1
        if kill -0 "$_pid" 2>/dev/null; then return 0; fi
        _pid=
    done
    return 1
}

if start_listener; then
    tcp_try 127.0.0.1 "$_lport"
    case $? in
        0) emit control.local REACHED "own-listener:$_lport" ;;
        1) emit control.local SKIPPED "control-failed:own-listener-not-detected" ;;
        *) emit control.local SKIPPED "no-tcp-tool" ;;
    esac
    kill "$_pid" 2>/dev/null
    wait "$_pid" 2>/dev/null
else
    emit control.local SKIPPED "could-not-start-listener"
fi

tcp_try 127.0.0.1 "$_cport"
case $? in
    1) emit control.closed BLOCKED "closed-port-detected:$_cport" ;;
    0) emit control.closed SKIPPED "control-failed:closed-port-reported-open" ;;
    *) emit control.closed SKIPPED "no-tcp-tool" ;;
esac

if [ -z "${PROBE_CONTROL_TARGET:-}" ]; then
    emit control.allowed SKIPPED "target-not-configured"
else
    tcp_try "${PROBE_CONTROL_TARGET%:*}" "${PROBE_CONTROL_TARGET##*:}"
    case $? in
        0) emit control.allowed REACHED "tcp-connected:$PROBE_CONTROL_TARGET" ;;
        1) emit control.allowed SKIPPED "control-failed:$PROBE_CONTROL_TARGET" ;;
        *) emit control.allowed SKIPPED "no-tcp-tool" ;;
    esac
fi
