#!/bin/sh
# Sends a canary over TCP and UDP to the host-side listener. The guest cannot
# know whether it arrived, so each send is SKIPPED here and tests/leak/verify.py
# decides from the listener's log on the host.
# Env: PROBE_HOST_PORT PROBE_HOST_UDP_PORT PROBE_HOST_ALIAS PROBE_GUEST_GATEWAY PROBE_RUN_ID
. "$(dirname "$0")/lib.sh"

if [ -z "${PROBE_RUN_ID:-}" ] || ! have nc; then
    emit canary SKIPPED "no-run-id-or-nc"
    exit 0
fi
_gw=${PROBE_GUEST_GATEWAY:-}
if [ -z "$_gw" ] && have ip; then _gw=$(ip route show default 2>/dev/null | awk '/default/ {print $3; exit}'); fi

send() { # send VIA HOST
    [ -n "$2" ] || { emit "canary.send.tcp.$1" SKIPPED "no-address"; return; }
    if [ -n "${PROBE_HOST_PORT:-}" ]; then
        _id="CANARY-$PROBE_RUN_ID-${PROBE_PREFIX}tcp.$1"
        printf '%s\n' "$_id" | nc -w 2 "$2" "$PROBE_HOST_PORT" >/dev/null 2>&1
        emit "canary.send.tcp.$1" SKIPPED "sent:$_id"
    fi
    if [ -n "${PROBE_HOST_UDP_PORT:-}" ]; then
        _id="CANARY-$PROBE_RUN_ID-${PROBE_PREFIX}udp.$1"
        printf '%s\n' "$_id" | nc -u -w 1 "$2" "$PROBE_HOST_UDP_PORT" >/dev/null 2>&1
        emit "canary.send.udp.$1" SKIPPED "sent:$_id"
    fi
}

send alias "${PROBE_HOST_ALIAS:-host.microsandbox.internal}"
send gateway "$_gw"
send loopback 127.0.0.1
