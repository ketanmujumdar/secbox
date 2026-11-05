#!/bin/sh
# Targets the allowlist must refuse, by name, by raw IP and through DNS.
# Env: PROBE_DENIED_TARGET=host:port PROBE_DENIED_IP=ip:port PROBE_DENIED_DNS_NAME=name
. "$(dirname "$0")/lib.sh"
check_tcp denied.by-name "${PROBE_DENIED_TARGET:-}"
check_tcp denied.by-ip "${PROBE_DENIED_IP:-}"

# A gateway can accept a TCP connect and then drop it, so raw connects are not
# proof either way. These require the real content to come back.
if [ -n "${PROBE_DENIED_URL:-}" ]; then
    check_token denied.content "$PROBE_DENIED_URL" "${PROBE_DENIED_MARKER:-}"
else
    emit denied.content SKIPPED "url-not-configured"
fi
if [ -n "${PROBE_ALLOWED_URL:-}" ]; then
    check_token allowed.content "$PROBE_ALLOWED_URL" "${PROBE_ALLOWED_MARKER:-}"
else
    emit allowed.content SKIPPED "url-not-configured"
fi
check_https denied.https "${PROBE_DENIED_HTTPS_URL:-}" "${PROBE_DENIED_MARKER:-Example Domain}"
check_https allowed.https "${PROBE_ALLOWED_HTTPS_URL:-}" "401"

if [ -z "${PROBE_DENIED_DNS_NAME:-}" ]; then
    emit denied.dns SKIPPED "name-not-configured"
elif ! have nslookup; then
    emit denied.dns SKIPPED "no-nslookup"
else
    _out=$(nslookup "$PROBE_DENIED_DNS_NAME" 2>&1)
    _rc=$?
    _answers=$(printf '%s\n' "$_out" | sed -n '/^Name:/,$p' | grep -c '^Address')
    if [ "$_rc" -eq 0 ] && [ "$_answers" -gt 0 ]; then
        emit denied.dns REACHED "resolved:$PROBE_DENIED_DNS_NAME"
    else
        gated_block denied.dns "no-answer:$PROBE_DENIED_DNS_NAME"
    fi
fi
