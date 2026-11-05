#!/bin/sh
# QUIC, that is UDP to port 443. A hostname allowlist that only reads TLS server
# names from TCP sees none of this, so it is worth its own probe.
#
# The packet sent is a QUIC Initial carrying a version number no server
# implements. A server that is really there answers with a Version Negotiation
# packet. Silence means either the policy dropped it or nothing was listening,
# which is why quic.control has to answer first in this same context.
#
# Env: PROBE_QUIC_CONTROL host:port that policy allows and that speaks QUIC
#      PROBE_QUIC_TARGET  host:port that policy should deny
#      PROBE_QUIC_IP      the same by raw IP, to skip name resolution
. "$(dirname "$0")/lib.sh"

QUIC_OK=0
if [ -z "${PROBE_QUIC_CONTROL:-}" ]; then
    emit quic.control SKIPPED "control-not-configured"
else
    quic_try "${PROBE_QUIC_CONTROL%:*}" "${PROBE_QUIC_CONTROL##*:}"
    case $? in
        0)
            emit quic.control REACHED "version-negotiation-from:$PROBE_QUIC_CONTROL"
            QUIC_OK=1
            ;;
        1) emit quic.control SKIPPED "control-failed:no-answer:$PROBE_QUIC_CONTROL" ;;
        *) emit quic.control SKIPPED "no-udp-tool" ;;
    esac
fi

check_quic() { # check_quic NAME host:port
    if [ -z "$2" ]; then emit "$1" SKIPPED "target-not-configured"; return; fi
    quic_try "${2%:*}" "${2##*:}"
    case $? in
        0) emit "$1" REACHED "udp-443-answered:$2" ;;
        1) gated_block_if "$1" "$QUIC_OK" "no-answer:$2" ;;
        *) emit "$1" SKIPPED "no-udp-tool" ;;
    esac
}

check_quic quic.denied "${PROBE_QUIC_TARGET:-}"
check_quic quic.denied-ip "${PROBE_QUIC_IP:-}"
