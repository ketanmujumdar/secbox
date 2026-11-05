#!/bin/sh
# UDP out of the sandbox, using DNS queries as the payload because they get a
# reply. A reply proves the round trip; silence does not, so it needs the control.
# Env: PROBE_UDP_RESOLVER (default 1.1.1.1), PROBE_UDP_NAME (default example.com)
. "$(dirname "$0")/lib.sh"

if ! have nslookup; then
    emit udp.external-dns SKIPPED "no-nslookup"
    exit 0
fi
_name=${PROBE_UDP_NAME:-example.com}
_srv=${PROBE_UDP_RESOLVER:-1.1.1.1}
_out=$(timeout "$((PROBE_TIMEOUT + 2))" nslookup "$_name" "$_srv" 2>&1)
_rc=$?
if [ "$_rc" -eq 0 ] && printf '%s\n' "$_out" | sed -n '/^Name:/,$p' | grep -q '^Address'; then
    emit udp.external-dns REACHED "answer-from:$_srv"
elif [ "$_rc" -eq 127 ]; then
    emit udp.external-dns SKIPPED "timeout-tool-missing"
else
    gated_block udp.external-dns "no-answer-from:$_srv"
fi
