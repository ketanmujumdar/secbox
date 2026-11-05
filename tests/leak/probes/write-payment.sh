#!/bin/sh
# A call to a payment API: the purchase an agent that has been tricked would
# try to make. The request carries a fixed fake key, so the service answers
# with an authentication error and no money can move. REACHED therefore means
# the request left the sandbox and a payment service judged it.
#
# A host allowlist should already stop this. The cleartext variant is sent as
# well, because that is the one a gateway without TLS interception can read.
#
# Env: PROBE_PAYMENT_URL          https endpoint of the payment API
#      PROBE_PAYMENT_MARKER       content only that API would send back
#      PROBE_PAYMENT_PLAIN_TARGET host:port for the cleartext attempt
#      PROBE_PAYMENT_PLAIN_PATH   path for the cleartext attempt
. "$(dirname "$0")/lib.sh"

FAKE_KEY=sk_test_0000000000000000leakprobe
BODY='{"amount":100,"currency":"usd","source":"tok_leakprobe"}'
MARKER=${PROBE_PAYMENT_MARKER:-invalid_request_error}

if [ -z "${PROBE_PAYMENT_URL:-}" ]; then
    emit write.payment SKIPPED "url-not-configured"
else
    http_send POST "$PROBE_PAYMENT_URL" "$BODY" "Authorization: Bearer $FAKE_KEY"
    write_result write.payment $? "$MARKER" 1
    rm -f "${FETCH_BODY:-}"
fi

if [ -z "${PROBE_PAYMENT_PLAIN_TARGET:-}" ]; then
    emit write.payment-plain SKIPPED "target-not-configured"
else
    # Read back by write_result, which lib.sh provides.
    # shellcheck disable=SC2034
    FETCH_ERR=
    raw_request POST "${PROBE_PAYMENT_PLAIN_TARGET%:*}" "${PROBE_PAYMENT_PLAIN_TARGET##*:}" \
        "${PROBE_PAYMENT_PLAIN_PATH:-/}" "${PROBE_PAYMENT_PLAIN_TARGET%:*}" "$BODY"
    write_result write.payment-plain $? "$MARKER" 1
    rm -f "${FETCH_BODY:-}"
fi
