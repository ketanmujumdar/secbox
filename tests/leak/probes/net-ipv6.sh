#!/bin/sh
# IPv6 out of the sandbox: reachability, an IPv6 literal target and IPv6
# metadata.
#
# The spike found the guest has no global IPv6 address and no IPv6 default
# route, so an IPv6 target that stays silent says nothing about policy. The
# gate here is therefore IPv6's own: ipv6.control must reach an allowed IPv6
# literal in this same context. Without that every IPv6 result is SKIPPED, and
# a machine with no IPv6 at all can never report a false BLOCKED.
#
# Env: PROBE_IPV6_CONTROL_URL  an allowed IPv6 literal URL, the positive control
#      PROBE_IPV6_TARGET_URL   a denied IPv6 literal URL
#      PROBE_IPV6_METADATA_URL cloud metadata over IPv6
. "$(dirname "$0")/lib.sh"

# Informational: what IPv6 this context has at all. Always SKIPPED, because it
# is a description, not a check.
_addrs=
_route=
if have ip; then
    _addrs=$(ip -6 addr show scope global 2>/dev/null | awk '/inet6/ {printf "%s ", $2}')
    _route=$(ip -6 route show default 2>/dev/null | head -n 1 | tr -s ' ')
elif [ -r /proc/net/if_inet6 ]; then
    _addrs=$(awk '$6 !~ /^(10|20)$/ {printf "%s/%s ", $1, $3}' /proc/net/if_inet6)
fi
emit ipv6.status SKIPPED "info:global-addrs=[${_addrs}]default-route=[${_route}]"

# The control. A failed control is SKIPPED, never BLOCKED.
IPV6_OK=0
if [ -z "${PROBE_IPV6_CONTROL_URL:-}" ]; then
    emit ipv6.control SKIPPED "control-not-configured"
else
    _out=$(check_any_response ipv6.control "$PROBE_IPV6_CONTROL_URL" 0)
    case $_out in
        *" REACHED "*)
            IPV6_OK=1
            printf '%s\n' "$_out"
            ;;
        *)
            # Anything short of a reach means IPv6 itself may be absent here.
            emit ipv6.control SKIPPED "control-failed:${PROBE_IPV6_CONTROL_URL}"
            ;;
    esac
fi

if [ "$IPV6_OK" = 1 ]; then
    check_any_response ipv6.public "${PROBE_IPV6_TARGET_URL:-}" 1
    check_any_response ipv6.metadata "${PROBE_IPV6_METADATA_URL:-}" 1
else
    emit ipv6.public SKIPPED "no-ipv6-control-in-this-context:${PROBE_IPV6_TARGET_URL:-unset}"
    emit ipv6.metadata SKIPPED "no-ipv6-control-in-this-context:${PROBE_IPV6_METADATA_URL:-unset}"
fi
