#!/bin/sh
# Sends the guest's placeholder to echo services that return request headers,
# then looks for the real value by hash.
#   secret.inject-allowed REACHED is the expected result: injection works.
#   secret.inject-denied  REACHED is a leak.
# Fake values only, using characters [A-Za-z0-9_-].
# Env: PROBE_SECRET_ENV PROBE_SECRET_SHA256 PROBE_SECRET_ALLOWED_URL PROBE_SECRET_DENIED_URL
. "$(dirname "$0")/lib.sh"

check_inject() {
    if [ -z "$2" ]; then emit "$1" SKIPPED "url-not-configured"; return; fi
    if [ -z "${PROBE_SECRET_ENV:-}" ] || [ -z "${PROBE_SECRET_SHA256:-}" ]; then
        emit "$1" SKIPPED "secret-not-configured"
        return
    fi
    eval "_val=\${$PROBE_SECRET_ENV:-}"
    if [ -z "$_val" ]; then emit "$1" SKIPPED "variable-not-set"; return; fi
    if ! sha256_of x >/dev/null; then emit "$1" SKIPPED "no-sha256-tool"; return; fi
    FETCH_ERR=
    fetch "$2" "X-Probe-Secret: $_val"
    _rc=$?
    case $_rc in
        0)
            _hit=0
            for _tok in $(tr -cs 'A-Za-z0-9_-' '\n' <"$FETCH_BODY"); do
                if [ "$(sha256_of "$_tok")" = "$PROBE_SECRET_SHA256" ]; then _hit=1; break; fi
            done
            if [ "$_hit" = 1 ]; then
                emit "$1" REACHED "real-value-arrived:$2"
            elif grep -qiF 'X-Probe-Secret' "$FETCH_BODY"; then
                gated_block "$1" "header-arrived-without-real-value:$2"
            else
                emit "$1" SKIPPED "response-does-not-echo-header:$2"
            fi
            ;;
        1) gated_block "$1" "connect-failed:$2" ;;
        4) gated_block "$1" "refused-by-http-status:$(printf '%s' "$FETCH_ERR" | tail -n 1)" ;;
        2) emit "$1" SKIPPED "no-http-tool" ;;
        *) emit "$1" SKIPPED "ambiguous-failure:$(printf '%s' "${FETCH_ERR:-}" | tail -n 1)" ;;
    esac
    rm -f "$FETCH_BODY"
}

check_inject secret.inject-allowed "${PROBE_SECRET_ALLOWED_URL:-}"
check_inject secret.inject-denied "${PROBE_SECRET_DENIED_URL:-}"
check_inject secret.inject-plain-http "${PROBE_SECRET_PLAIN_URL:-}"
