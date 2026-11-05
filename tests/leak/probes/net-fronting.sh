#!/bin/sh
# Domain fronting: a request whose TLS server name is an allowed host but whose
# HTTP Host header names a denied one.
#
# This is the specific weakness of --net-strict=false, which the spike records
# as trusting the TLS server name only. If the denied host's own content comes
# back, the allowlist was fronted. The spike lists this as untested.
#
# Fronting only works when the allowed host's front end is willing to serve the
# denied host, so a response without the denied host's marker proves nothing
# either way and is SKIPPED, not BLOCKED.
#
# Env: PROBE_FRONT_URL          https URL on a host policy allows (sets the TLS name)
#      PROBE_FRONT_MARKER       content proving the allowed host answered (control)
#      PROBE_FRONT_HOST         the denied host name to put in the Host header
#      PROBE_FRONT_HOST_MARKER  content only the denied host would send back
#      PROBE_FRONT_PLAIN_TARGET host:port of the allowed host, for the cleartext case
#      PROBE_FRONT_PLAIN_PATH   path for the cleartext case (default /)
. "$(dirname "$0")/lib.sh"

FRONT_OK=0
if [ -z "${PROBE_FRONT_URL:-}" ] || [ -z "${PROBE_FRONT_MARKER:-}" ]; then
    emit fronting.control SKIPPED "control-not-configured"
else
    FETCH_ERR=
    fetch "$PROBE_FRONT_URL"
    _rc=$?
    if { [ "$_rc" -eq 0 ] || [ "$_rc" -eq 4 ]; } && {
        { [ -s "${FETCH_BODY:-/dev/null}" ] && grep -qF "$PROBE_FRONT_MARKER" "$FETCH_BODY"; } ||
            printf '%s' "${FETCH_ERR:-}" | grep -qF "$PROBE_FRONT_MARKER"
    }; then
        emit fronting.control REACHED "allowed-host-answered:$PROBE_FRONT_URL"
        FRONT_OK=1
    else
        emit fronting.control SKIPPED "control-failed:$PROBE_FRONT_URL"
    fi
    rm -f "${FETCH_BODY:-}"
fi

# judge NAME RC: FETCH_BODY and FETCH_ERR hold the answer to a fronted request.
judge() {
    if { [ -n "${PROBE_FRONT_HOST_MARKER:-}" ] && [ -s "${FETCH_BODY:-/dev/null}" ] &&
        grep -qF "$PROBE_FRONT_HOST_MARKER" "$FETCH_BODY"; }; then
        emit "$1" REACHED "denied-host-answered-behind-allowed-name:$PROBE_FRONT_HOST"
    elif { [ -s "${FETCH_BODY:-/dev/null}" ] && grep -q "$GATEWAY_DENY_MARKER" "$FETCH_BODY"; } ||
        printf '%s' "${FETCH_ERR:-}" | grep -q "$GATEWAY_DENY_MARKER"; then
        emit "$1" BLOCKED "gateway-denied:host-header-checked"
    else
        case $2 in
            0) emit "$1" SKIPPED "response-without-denied-marker:front-end-did-not-serve-it" ;;
            4) emit "$1" SKIPPED "refused-by-http-status-source-unknown:$(printf '%s' "${FETCH_ERR:-}" | tail -n 1)" ;;
            1) gated_block_if "$1" "$FRONT_OK" "connect-failed" ;;
            2) emit "$1" SKIPPED "no-http-tool" ;;
            3) emit "$1" SKIPPED "no-response-or-tls-failure:$(printf '%s' "${FETCH_ERR:-}" | tail -n 1)" ;;
            *) emit "$1" SKIPPED "ambiguous" ;;
        esac
    fi
    rm -f "${FETCH_BODY:-}"
}

# TLS name allowed, Host header denied. Whether a given wget keeps our Host
# header or adds a second one is not guaranteed, so only the denied host's own
# content is read as a reach.
if [ -z "${PROBE_FRONT_URL:-}" ] || [ -z "${PROBE_FRONT_HOST:-}" ]; then
    emit fronting.https SKIPPED "not-configured"
else
    FETCH_ERR=
    fetch "$PROBE_FRONT_URL" "Host: $PROBE_FRONT_HOST"
    judge fronting.https $?
fi

# The same trick in cleartext, where the gateway can read the Host header.
if [ -z "${PROBE_FRONT_PLAIN_TARGET:-}" ] || [ -z "${PROBE_FRONT_HOST:-}" ]; then
    emit fronting.http SKIPPED "not-configured"
else
    FETCH_ERR=
    raw_request GET "${PROBE_FRONT_PLAIN_TARGET%:*}" "${PROBE_FRONT_PLAIN_TARGET##*:}" \
        "${PROBE_FRONT_PLAIN_PATH:-/}" "$PROBE_FRONT_HOST"
    judge fronting.http $?
fi
