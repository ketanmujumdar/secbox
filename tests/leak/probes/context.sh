#!/bin/sh
# Runs the controls and then every network and secret probe in one context. The
# control result decides whether failures in this same context may be BLOCKED.
_dir=$(dirname "$0")
. "$_dir/lib.sh"

_out=$(sh "$_dir/control.sh")
printf '%s\n' "$_out"
PROBE_CONTROL_OK=0
if printf '%s\n' "$_out" | grep -q "^PROBE ${PROBE_PREFIX}control\.local REACHED " &&
    printf '%s\n' "$_out" | grep -q "^PROBE ${PROBE_PREFIX}control\.closed BLOCKED " &&
    printf '%s\n' "$_out" | grep -q "^PROBE ${PROBE_PREFIX}control\.allowed REACHED "; then
    PROBE_CONTROL_OK=1
fi
export PROBE_CONTROL_OK PROBE_PREFIX

for _p in net-host net-canary net-udp net-ipv6 net-lan net-metadata net-denied secret-env secret-inject; do
    sh "$_dir/$_p.sh"
done
