#!/bin/sh
# IPv6 out of the sandbox. With no IPv6 route there is no way to run a positive
# control, so that case is SKIPPED, not BLOCKED.
# Env: PROBE_IPV6_TARGET (default [2606:4700:4700::1111]:443 as host,port)
. "$(dirname "$0")/lib.sh"

if [ ! -r /proc/net/if_inet6 ] && ! have ip; then
    emit ipv6.status SKIPPED "cannot-inspect-ipv6"
    exit 0
fi
_addrs=$(ip -6 addr show scope global 2>/dev/null | awk '/inet6/ {printf "%s ", $2}')
_route=$(ip -6 route show default 2>/dev/null | head -n 1)
emit ipv6.status SKIPPED "info:global-addrs=[${_addrs}]default-route=[${_route}]"

try6() { # try6 NAME ADDR PORT
    if [ -z "$_route" ]; then
        emit "$1" SKIPPED "no-ipv6-route-in-this-context:[$2]:$3"
        return
    fi
    if ! have wget; then emit "$1" SKIPPED "no-wget"; return; fi
    _msg=$(wget -q -T "$PROBE_TIMEOUT" -O /dev/null "http://[$2]:$3/" 2>&1)
    _rc=$?
    if [ "$_rc" -eq 0 ] || printf '%s' "$_msg" | grep -q 'server returned error'; then
        emit "$1" REACHED "response:[$2]:$3"
    else
        gated_block "$1" "no-response:[$2]:$3"
    fi
}

try6 ipv6.public 2606:4700:4700::1111 80
try6 ipv6.metadata fd00:ec2::254 80
