#!/bin/sh
# The host's LAN address and the LAN router. Env: PROBE_LAN_TARGET=ip:port
# PROBE_LAN_ROUTER=ip:port. Check both answer from the host first: that is the
# positive control showing there is something to reach.
. "$(dirname "$0")/lib.sh"
for _pair in "lan.host:${PROBE_LAN_TARGET:-}" "lan.router:${PROBE_LAN_ROUTER:-}"; do
    _n=${_pair%%:*}
    _t=${_pair#*:}
    if [ -z "$_t" ]; then
        emit "$_n" SKIPPED "target-not-configured"
    else
        check_http "$_n" "${_t%:*}" "${_t##*:}"
    fi
done
