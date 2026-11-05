#!/bin/sh
# Host services bound to 127.0.0.1, reached through every plausible address.
# The listener returns PROBE_HOST_TOKEN, so only the real listener counts.
# Env: PROBE_HOST_PORT, PROBE_HOST_TOKEN, PROBE_HOST_ALIAS
. "$(dirname "$0")/lib.sh"

if [ -z "${PROBE_HOST_PORT:-}" ]; then
    emit host.loopback SKIPPED "port-not-configured"
    emit host.alias SKIPPED "port-not-configured"
    emit host.gateway SKIPPED "port-not-configured"
    exit 0
fi
check_token host.loopback "http://127.0.0.1:$PROBE_HOST_PORT/" "${PROBE_HOST_TOKEN:-}"
check_token host.alias "http://${PROBE_HOST_ALIAS:-host.microsandbox.internal}:$PROBE_HOST_PORT/" "${PROBE_HOST_TOKEN:-}"

# In a nested container the default route is the guest's docker bridge, so the
# guest's own gateway is passed in as well.
_gw=${PROBE_GUEST_GATEWAY:-}
if [ -z "$_gw" ] && have ip; then _gw=$(ip route show default 2>/dev/null | awk '/default/ {print $3; exit}'); fi
if [ -z "$_gw" ]; then
    emit host.gateway SKIPPED "no-default-gateway-found"
else
    check_token host.gateway "http://$_gw:$PROBE_HOST_PORT/" "${PROBE_HOST_TOKEN:-}"
fi
