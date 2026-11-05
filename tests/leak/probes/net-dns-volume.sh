#!/bin/sh
# DNS tunnelling by volume. One lookup of a denied name is already covered by
# net-denied.sh; this asks the different question of whether a run of distinct
# subdomain lookups can carry data out, which is how DNS exfiltration works.
# Every name here is unique, so a cached answer cannot hide a block, and each
# label carries hex so a resolver log would show the payload arriving.
#
# dns.control must resolve an allowed name in this same context, otherwise a
# silent resolver cannot be told from a filtered one and everything is SKIPPED.
#
# Env: PROBE_DNS_CONTROL_NAME a name policy allows, the positive control
#      PROBE_DNS_TUNNEL_BASE  a denied base domain to build subdomains under
#      PROBE_DNS_TUNNEL_COUNT how many distinct lookups (default 12)
#      PROBE_RUN_ID           makes the names unique to this run
. "$(dirname "$0")/lib.sh"

if ! have nslookup; then
    emit dns.control SKIPPED "no-nslookup"
    emit dns.tunnel.volume SKIPPED "no-nslookup"
    emit dns.tunnel.payload SKIPPED "no-nslookup"
    exit 0
fi

DNS_OK=0
if [ -z "${PROBE_DNS_CONTROL_NAME:-}" ]; then
    emit dns.control SKIPPED "control-not-configured"
elif dns_resolves "$PROBE_DNS_CONTROL_NAME"; then
    emit dns.control REACHED "resolved:$PROBE_DNS_CONTROL_NAME"
    DNS_OK=1
else
    emit dns.control SKIPPED "control-failed:$PROBE_DNS_CONTROL_NAME"
fi

_base=${PROBE_DNS_TUNNEL_BASE:-}
if [ -z "$_base" ]; then
    emit dns.tunnel.volume SKIPPED "base-domain-not-configured"
    emit dns.tunnel.payload SKIPPED "base-domain-not-configured"
    exit 0
fi

# Hex of the run id, so the label is in the character set a tunnel would use.
_run=${PROBE_RUN_ID:-0}
_hex=$(printf '%s' "$_run" | od -An -tx1 2>/dev/null | tr -d ' \n')
[ -n "$_hex" ] || _hex=00

_want=${PROBE_DNS_TUNNEL_COUNT:-12}
_got=0
_i=1
while [ "$_i" -le "$_want" ]; do
    if dns_resolves "d${_i}x${_hex}.$_base"; then _got=$((_got + 1)); fi
    _i=$((_i + 1))
done
if [ "$_got" -gt 0 ]; then
    emit dns.tunnel.volume REACHED "answers:$_got/$_want:$_base"
else
    gated_block_if dns.tunnel.volume "$DNS_OK" "no-answers:0/$_want:$_base"
fi

# One maximum-length label, the shape used to push a whole chunk out at once.
_label=$(printf '%s' "p${_hex}$(printf '%s' "$_hex$_hex$_hex$_hex" | cut -c1-60)" | cut -c1-63)
if dns_resolves "$_label.$_base"; then
    emit dns.tunnel.payload REACHED "long-label-resolved:$_base"
else
    gated_block_if dns.tunnel.payload "$DNS_OK" "long-label-no-answer:$_base"
fi
