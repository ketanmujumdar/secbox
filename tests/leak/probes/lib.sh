# Shared helpers for leak probes. POSIX sh; runs in the guest, in nested
# containers and in `docker build` steps. Sourced, never executed.
#
# Output contract, one line per check:
#   PROBE <name> REACHED|BLOCKED|SKIPPED <detail>
#
# REACHED  the target answered (for raw TCP this can overstate: see check_tcp).
# BLOCKED  the check really ran, it failed, and the positive control for this
#          same context passed. Only then is a failure evidence of blocking.
# SKIPPED  the check could not give an answer: missing tool, Docker not ready,
#          no control, or an ambiguous result. Never counts as a pass.

PROBE_PREFIX=${PROBE_PREFIX:-}
PROBE_TIMEOUT=${PROBE_TIMEOUT:-5}

emit() {
    _detail=$(printf '%s' "${3:--}" | tr '\n\r\t' '   ' | sed 's/^ *//')
    printf 'PROBE %s%s %s %s\n' "$PROBE_PREFIX" "$1" "$2" "${_detail:--}"
}

have() { command -v "$1" >/dev/null 2>&1; }

# A failure is only reported as BLOCKED once this context's control has passed.
gated_block() {
    if [ "${PROBE_CONTROL_OK:-0}" = 1 ]; then
        emit "$1" BLOCKED "$2"
    else
        emit "$1" SKIPPED "no-positive-control:$2"
    fi
}

# tcp_try HOST PORT: 0 connected, 1 failed to connect, 2 no usable tool.
tcp_try() {
    _err=$(mktemp 2>/dev/null) || return 2
    if have nc; then
        # Not every nc applies -w to the connect phase, so a watchdog enforces it.
        nc -z -w "$PROBE_TIMEOUT" "$1" "$2" >/dev/null 2>"$_err" &
        _nc=$!
        (sleep "$((PROBE_TIMEOUT + 1))"; kill "$_nc" 2>/dev/null) >/dev/null 2>&1 &
        _wd=$!
        wait "$_nc"
        _rc=$?
        kill "$_wd" 2>/dev/null
        wait "$_wd" 2>/dev/null
        if [ "$_rc" -eq 0 ]; then rm -f "$_err"; return 0; fi
        if ! grep -qiE 'usage|invalid option|unrecognized|illegal option|not supported' "$_err"; then
            rm -f "$_err"
            return 1
        fi
    fi
    rm -f "$_err"
    if have curl; then
        curl -sS -o /dev/null -m "$PROBE_TIMEOUT" --connect-timeout "$PROBE_TIMEOUT" \
            "http://$1:$2/" >/dev/null 2>&1
        case $? in
            6|7|28) return 1 ;;
            *) return 0 ;;
        esac
    fi
    return 2
}

# check_tcp NAME HOST:PORT. Connect-only: a proxy that accepts then drops looks
# REACHED. That overstates reach, which is the safe direction for a leak test.
check_tcp() {
    if [ -z "$2" ]; then emit "$1" SKIPPED "target-not-configured"; return; fi
    _host=${2%:*}
    _port=${2##*:}
    tcp_try "$_host" "$_port"
    case $? in
        0) emit "$1" REACHED "tcp-connected:$2" ;;
        1) gated_block "$1" "connect-failed:$2" ;;
        *) emit "$1" SKIPPED "no-tcp-tool" ;;
    esac
}

# fetch URL [HEADER]: body goes to FETCH_BODY file.
# 0 got a response, 1 could not connect, 2 no tool, 3 ambiguous failure.
fetch() {
    FETCH_ERR=
    FETCH_BODY=$(mktemp 2>/dev/null) || return 2
    _err=$(mktemp 2>/dev/null) || return 2
    if have curl; then
        if [ -n "${2:-}" ]; then
            curl -sS -m "$PROBE_TIMEOUT" --connect-timeout "$PROBE_TIMEOUT" -H "$2" "$1" >"$FETCH_BODY" 2>"$_err"
        else
            curl -sS -m "$PROBE_TIMEOUT" --connect-timeout "$PROBE_TIMEOUT" "$1" >"$FETCH_BODY" 2>"$_err"
        fi
        _rc=$?
        FETCH_ERR=$(head -c 400 "$_err" 2>/dev/null)
        rm -f "$_err"
        case $_rc in
            0) return 0 ;;
            6|7|28) return 1 ;;
            *) return 3 ;;
        esac
    fi
    if have wget; then
        if [ -n "${2:-}" ]; then
            # shellcheck disable=SC2086
            wget -q -T "$PROBE_TIMEOUT" ${PROBE_WGET_INSECURE:+--no-check-certificate} -O - --header="$2" "$1" >"$FETCH_BODY" 2>"$_err"
        else
            # shellcheck disable=SC2086
            wget -q -T "$PROBE_TIMEOUT" ${PROBE_WGET_INSECURE:+--no-check-certificate} -O - "$1" >"$FETCH_BODY" 2>"$_err"
        fi
        _rc=$?
        if [ "$_rc" -eq 0 ]; then rm -f "$_err"; return 0; fi
        FETCH_ERR=$(head -c 400 "$_err")
        if grep -q 'server returned error' "$_err"; then
            rm -f "$_err"
            return 4
        fi
        if grep -qiE 'refused|timed out|bad address|unreachable|could not resolve|unable to resolve' "$_err"; then
            rm -f "$_err"
            return 1
        fi
        rm -f "$_err"
        return 3
    fi
    rm -f "$_err"
    return 2
}

# The microsandbox gateway accepts TCP on port 80 for any address and answers
# with this policy error, so a connect there proves nothing. Seen on v0.7.13.
GATEWAY_DENY_MARKER=network_policy_denied

# raw_request METHOD HOST PORT PATH HOST_HEADER [BODY]: a cleartext HTTP request
# written by hand, so the method, the Host header and the body are all ours to
# choose. The whole response, headers included, goes to FETCH_BODY, which keeps
# the body of error responses that wget throws away.
# 0 got bytes, 1 could not connect, 2 no nc, 3 connected but no bytes.
raw_request() {
    have nc || return 2
    FETCH_BODY=$(mktemp 2>/dev/null) || return 2
    _rq_body=${6:-}
    _rq_len=$(printf '%s' "$_rq_body" | wc -c | tr -d ' ')
    # stdin stays open: BusyBox nc half-closes at stdin EOF, and some servers then
    # hang up without replying. The sleep is left to expire after nc exits.
    {
        printf '%s %s HTTP/1.0\r\nHost: %s\r\nConnection: close\r\n' "$1" "$4" "$5"
        if [ -n "$_rq_body" ]; then
            printf 'Content-Type: application/json\r\nContent-Length: %s\r\n\r\n%s' \
                "$_rq_len" "$_rq_body"
        else
            printf '\r\n'
        fi
        sleep "$PROBE_TIMEOUT"
    } | nc -w "$PROBE_TIMEOUT" "$2" "$3" >"$FETCH_BODY" 2>/dev/null &
    _nc=$!
    (sleep "$((PROBE_TIMEOUT + 2))"; kill "$_nc" 2>/dev/null) >/dev/null 2>&1 &
    _wd=$!
    wait "$_nc"
    _rc=$?
    kill "$_wd" 2>/dev/null
    wait "$_wd" 2>/dev/null
    [ -s "$FETCH_BODY" ] && return 0
    [ "$_rc" -eq 0 ] && return 3
    return 1
}

# raw_http HOST PORT PATH [HOST_HEADER]: a plain GET. HOST_HEADER defaults to the
# host connected to; passing a different one is the cleartext fronting case.
raw_http() {
    raw_request GET "$1" "$2" "$3" "${4:-$1}"
}

# check_http NAME HOST PORT: for targets with no token. The gateway's own error is
# BLOCKED; any other response is REACHED, with its first line as the detail.
check_http() {
    raw_http "$2" "$3" /
    case $? in
        0)
            if grep -q "$GATEWAY_DENY_MARKER" "$FETCH_BODY"; then
                emit "$1" BLOCKED "gateway-denied:$2:$3"
            else
                emit "$1" REACHED "response:$2:$3:$(head -n 1 "$FETCH_BODY" | tr -d '\r')"
            fi
            ;;
        1) gated_block "$1" "connect-failed:$2:$3" ;;
        2) emit "$1" SKIPPED "no-nc" ;;
        *) emit "$1" SKIPPED "connected-no-response:$2:$3" ;;
    esac
    rm -f "${FETCH_BODY:-}"
}

# check_https NAME URL MARKER: REACHED only if MARKER is in the body or the status
# text. An intercepting gateway completes TLS itself and returns its own 403, so
# neither a TCP connect nor a TLS handshake proves the real server answered.
check_https() {
    if [ -z "$2" ]; then emit "$1" SKIPPED "url-not-configured"; return; fi
    FETCH_ERR=
    fetch "$2"
    _rc=$?
    if { [ "$_rc" -eq 0 ] || [ "$_rc" -eq 4 ]; } && {
        grep -qF "$3" "$FETCH_BODY" || printf '%s' "$FETCH_ERR" | grep -qF "$3"
    }; then
        emit "$1" REACHED "marker-seen:$2"
    else
        case $_rc in
            0) emit "$1" SKIPPED "response-without-marker:$2" ;;
            4) gated_block "$1" "http-error-no-marker:$(printf '%s' "$FETCH_ERR" | tail -n 1)" ;;
            1) gated_block "$1" "connect-failed:$2" ;;
            2) emit "$1" SKIPPED "no-http-tool" ;;
            *) emit "$1" SKIPPED "tls-or-other-failure:$(printf '%s' "$FETCH_ERR" | tail -n 1)" ;;
        esac
    fi
    rm -f "${FETCH_BODY:-}"
}

# check_token NAME URL TOKEN: REACHED only when the response carries TOKEN, so
# a gateway error page cannot be mistaken for the real listener.
check_token() {
    if [ -z "$3" ]; then emit "$1" SKIPPED "token-not-configured"; return; fi
    case $2 in
        http://*)
            _hp=${2#http://}
            _path=/${_hp#*/}
            [ "$_hp" = "${_hp#*/}" ] && _path=/
            _hp=${_hp%%/*}
            _h=${_hp%:*}
            _p=${_hp##*:}
            [ "$_h" = "$_hp" ] && _p=80
            raw_http "$_h" "$_p" "$_path"
            _rc=$?
            if [ "$_rc" -ne 2 ]; then
                case $_rc in
                    0)
                        if grep -qF "$3" "$FETCH_BODY"; then
                            emit "$1" REACHED "token-returned:$2"
                        elif grep -q "$GATEWAY_DENY_MARKER" "$FETCH_BODY"; then
                            emit "$1" BLOCKED "gateway-denied:$2"
                        else
                            emit "$1" SKIPPED "ambiguous-response-without-token:$2"
                        fi
                        ;;
                    1) gated_block "$1" "connect-failed:$2" ;;
                    *) emit "$1" SKIPPED "connected-no-response:$2" ;;
                esac
                rm -f "${FETCH_BODY:-}"
                return
            fi
            ;;
    esac
    fetch "$2"
    _rc=$?
    case $_rc in
        0)
            if grep -qF "$3" "$FETCH_BODY"; then
                emit "$1" REACHED "token-returned:$2"
            else
                emit "$1" SKIPPED "ambiguous-response-without-token:$2"
            fi
            ;;
        1) gated_block "$1" "connect-failed:$2" ;;
        2) emit "$1" SKIPPED "no-http-tool" ;;
        *) emit "$1" SKIPPED "ambiguous-failure:$2" ;;
    esac
    rm -f "$FETCH_BODY"
}

# BLOCKED only when this check's own control passed as well as the context's.
# A check whose own control failed cannot tell blocking from a broken setup.
gated_block_if() { # NAME EXTRA_OK DETAIL
    if [ "${2:-0}" = 1 ]; then
        gated_block "$1" "$3"
    else
        emit "$1" SKIPPED "no-positive-control:$3"
    fi
}

# check_any_response NAME URL [EXTRA_OK]: for targets whose content we cannot
# predict, such as an IPv6 literal. The gateway's own denial is BLOCKED. A bare
# 403 is SKIPPED, because the gateway answers with 403 too and wget drops the
# body that would tell them apart.
check_any_response() {
    if [ -z "$2" ]; then emit "$1" SKIPPED "url-not-configured"; return; fi
    fetch "$2"
    _rc=$?
    if { [ "$_rc" -eq 0 ] || [ "$_rc" -eq 4 ]; } && {
        { [ -s "${FETCH_BODY:-/dev/null}" ] && grep -q "$GATEWAY_DENY_MARKER" "$FETCH_BODY"; } ||
            printf '%s' "${FETCH_ERR:-}" | grep -q "$GATEWAY_DENY_MARKER"
    }; then
        emit "$1" BLOCKED "gateway-denied:$2"
    else
        case $_rc in
            0) emit "$1" REACHED "response:$2" ;;
            4)
                if printf '%s' "${FETCH_ERR:-}" | grep -q '403'; then
                    emit "$1" SKIPPED "http-403-could-be-the-gateway:$2"
                else
                    emit "$1" REACHED "http-status:$(printf '%s' "${FETCH_ERR:-}" | tail -n 1)"
                fi
                ;;
            1) gated_block_if "$1" "${3:-1}" "connect-failed:$2" ;;
            2) emit "$1" SKIPPED "no-http-tool" ;;
            *) emit "$1" SKIPPED "no-response:$(printf '%s' "${FETCH_ERR:-}" | tail -n 1)" ;;
        esac
    fi
    rm -f "${FETCH_BODY:-}"
}

# http_send METHOD URL [BODY] [HEADER]: a request with a method of our choosing,
# which is what a write or a purchase is. Body goes to FETCH_BODY, error text to
# FETCH_ERR. Returns as fetch does, plus 5 when nothing here can send this method
# (BusyBox wget does GET and POST only).
http_send() {
    FETCH_ERR=
    FETCH_BODY=$(mktemp 2>/dev/null) || return 2
    _err=$(mktemp 2>/dev/null) || return 2
    if have curl; then
        if [ -n "${4:-}" ]; then
            curl -sS -m "$PROBE_TIMEOUT" --connect-timeout "$PROBE_TIMEOUT" \
                -X "$1" -H 'Content-Type: application/json' -H "$4" \
                --data-binary "${3:-}" "$2" >"$FETCH_BODY" 2>"$_err"
        else
            curl -sS -m "$PROBE_TIMEOUT" --connect-timeout "$PROBE_TIMEOUT" \
                -X "$1" -H 'Content-Type: application/json' \
                --data-binary "${3:-}" "$2" >"$FETCH_BODY" 2>"$_err"
        fi
        _rc=$?
        FETCH_ERR=$(head -c 400 "$_err" 2>/dev/null)
        rm -f "$_err"
        case $_rc in
            0) return 0 ;;
            6|7|28) return 1 ;;
            *) return 3 ;;
        esac
    fi
    if have wget && [ "$1" = POST ]; then
        if [ -n "${4:-}" ]; then
            # shellcheck disable=SC2086
            wget -q -T "$PROBE_TIMEOUT" ${PROBE_WGET_INSECURE:+--no-check-certificate} \
                -O - --header="${4:-}" --post-data="${3:-}" "$2" >"$FETCH_BODY" 2>"$_err"
        else
            # shellcheck disable=SC2086
            wget -q -T "$PROBE_TIMEOUT" ${PROBE_WGET_INSECURE:+--no-check-certificate} \
                -O - --post-data="${3:-}" "$2" >"$FETCH_BODY" 2>"$_err"
        fi
        _rc=$?
        FETCH_ERR=$(head -c 400 "$_err" 2>/dev/null)
        rm -f "$_err"
        [ "$_rc" -eq 0 ] && return 0
        printf '%s' "$FETCH_ERR" | grep -q 'server returned error' && return 4
        printf '%s' "$FETCH_ERR" |
            grep -qiE 'refused|timed out|bad address|unreachable|could not resolve|unable to resolve' && return 1
        return 3
    fi
    rm -f "$_err"
    have wget && return 5
    return 2
}

# write_result NAME RC MARKER EXTRA_OK: turns an attempted write or purchase into
# a result. MARKER is content that only the real service would send back, so
# REACHED means the write left the sandbox and the service judged it, not that it
# succeeded. The gateway's own denial is BLOCKED on its own; everything else
# needs both controls.
write_result() {
    if { [ -s "${FETCH_BODY:-/dev/null}" ] && grep -q "$GATEWAY_DENY_MARKER" "$FETCH_BODY"; } ||
        printf '%s' "${FETCH_ERR:-}" | grep -q "$GATEWAY_DENY_MARKER"; then
        emit "$1" BLOCKED "refused-by-policy:gateway-denied"
        return
    fi
    case $2 in
        0 | 4)
            if { [ -n "$3" ] && [ -s "${FETCH_BODY:-/dev/null}" ] && grep -qiF "$3" "$FETCH_BODY"; } ||
                { [ -n "$3" ] && printf '%s' "${FETCH_ERR:-}" | grep -qiF "$3"; }; then
                emit "$1" REACHED "write-reached-the-service:marker-seen"
            elif [ "$2" = 4 ]; then
                # Only a status, no body: the gateway answers 403 as well, so
                # there is no telling it from the service's own refusal.
                emit "$1" SKIPPED "refused-by-http-status-source-unknown:$(printf '%s' "${FETCH_ERR:-}" | tail -n 1)"
            else
                emit "$1" SKIPPED "response-without-marker"
            fi
            ;;
        1) gated_block_if "$1" "$4" "connect-failed" ;;
        2) emit "$1" SKIPPED "no-http-tool" ;;
        5) emit "$1" SKIPPED "no-tool-for-this-method" ;;
        *) emit "$1" SKIPPED "ambiguous-failure:$(printf '%s' "${FETCH_ERR:-}" | tail -n 1)" ;;
    esac
}

# One QUIC Initial carrying version 0x0a0a0a0a, which no server implements, so a
# server that is really there answers with a Version Negotiation packet. Padded,
# because a server may discard an Initial smaller than 1200 bytes.
quic_packet() {
    printf '\300\012\012\012\012'
    printf '\010SBXPROBE'
    printf '\010sbxprobe'
    if have dd; then
        dd if=/dev/zero bs=1177 count=1 2>/dev/null
    else
        head -c 1177 /dev/zero 2>/dev/null
    fi
}

# quic_try HOST PORT: bytes coming back prove UDP to that port reached something
# that speaks QUIC. Silence proves nothing on its own, so it needs the control.
# 0 answered, 1 silent, 2 no usable tool.
quic_try() {
    have nc || return 2
    if ! have dd && ! have head; then return 2; fi
    _q=$(mktemp 2>/dev/null) || return 2
    # stdin is held open past the packet: nc quits at EOF and would miss the reply.
    { quic_packet; sleep "$PROBE_TIMEOUT"; } |
        nc -u -w "$PROBE_TIMEOUT" "$1" "$2" >"$_q" 2>/dev/null &
    _nc=$!
    (sleep "$((PROBE_TIMEOUT + 2))"; kill "$_nc" 2>/dev/null) >/dev/null 2>&1 &
    _wd=$!
    wait "$_nc"
    kill "$_wd" 2>/dev/null
    wait "$_wd" 2>/dev/null
    if [ -s "$_q" ]; then rm -f "$_q"; return 0; fi
    rm -f "$_q"
    return 1
}

# dns_resolves NAME: 0 the resolver answered with an address, 1 no answer, 2 no
# nslookup. BusyBox nslookup prints the answer section after a "Name:" line.
dns_resolves() {
    have nslookup || return 2
    _dns_out=$(nslookup "$1" 2>&1)
    _dns_rc=$?
    if [ "$_dns_rc" -eq 0 ] &&
        printf '%s\n' "$_dns_out" | sed -n '/^Name:/,$p' | grep -q '^Address'; then
        return 0
    fi
    return 1
}

sha256_of() {
    if have sha256sum; then
        printf '%s' "$1" | sha256sum | cut -d' ' -f1
    elif have shasum; then
        printf '%s' "$1" | shasum -a 256 | cut -d' ' -f1
    else
        return 1
    fi
}

# Probes must never touch a daemon other than the guest's own. On a non-Linux
# machine a plain `docker` would be the host's daemon, so only an explicit
# PROBE_DOCKER (the self-test's stub) is accepted there.
docker_ready() {
    DOCKER=${PROBE_DOCKER:-docker}
    if [ -z "${PROBE_DOCKER:-}" ] && [ "$(uname -s)" != Linux ]; then
        DOCKER_WHY=not-in-linux-guest
        return 1
    fi
    if ! have "$DOCKER"; then DOCKER_WHY=docker-not-installed; return 1; fi
    if ! "$DOCKER" info >/dev/null 2>&1; then DOCKER_WHY=docker-not-ready; return 1; fi
    PROBE_IMAGE=${PROBE_IMAGE:-alpine:3.20}
    # Never pull here: a failed pull would be a setup failure, not a block.
    if ! "$DOCKER" image inspect "$PROBE_IMAGE" >/dev/null 2>&1; then
        DOCKER_WHY="image-not-loaded:$PROBE_IMAGE"
        return 1
    fi
    return 0
}

# Variables forwarded into nested contexts. Fake test values only.
write_env_file() {
    : >"$1"
    for _v in PROBE_TIMEOUT PROBE_CONTROL_TARGET PROBE_HOST_PORT PROBE_HOST_TOKEN \
        PROBE_HOST_ALIAS PROBE_LAN_TARGET PROBE_DENIED_TARGET PROBE_DENIED_IP \
        PROBE_DENIED_DNS_NAME PROBE_SECRET_ENV PROBE_SECRET_SHA256 \
        PROBE_SECRET_ALLOWED_URL PROBE_SECRET_DENIED_URL PROBE_HOST_UDP_PORT \
        PROBE_GUEST_GATEWAY PROBE_RUN_ID PROBE_DENIED_URL PROBE_DENIED_MARKER \
        PROBE_ALLOWED_URL PROBE_ALLOWED_MARKER PROBE_WGET_INSECURE PROBE_LAN_ROUTER \
        PROBE_DENIED_HTTPS_URL PROBE_ALLOWED_HTTPS_URL PROBE_SECRET_PLAIN_URL \
        PROBE_IPV6_CONTROL_URL PROBE_IPV6_TARGET_URL PROBE_IPV6_METADATA_URL \
        PROBE_QUIC_CONTROL PROBE_QUIC_TARGET PROBE_QUIC_IP \
        PROBE_DNS_CONTROL_NAME PROBE_DNS_TUNNEL_BASE PROBE_DNS_TUNNEL_COUNT \
        PROBE_FRONT_URL PROBE_FRONT_MARKER PROBE_FRONT_HOST PROBE_FRONT_HOST_MARKER \
        PROBE_FRONT_PLAIN_TARGET PROBE_FRONT_PLAIN_PATH \
        PROBE_NPM_READ_URL PROBE_NPM_READ_MARKER \
        PROBE_NPM_PUT_URL PROBE_NPM_PUT_MARKER PROBE_PAYMENT_URL PROBE_PAYMENT_MARKER \
        PROBE_PAYMENT_PLAIN_TARGET PROBE_PAYMENT_PLAIN_PATH; do
        eval "_val=\${$_v:-}"
        [ -n "$_val" ] && printf '%s=%s\n' "$_v" "$_val" >>"$1"
    done
    if [ -n "${PROBE_SECRET_ENV:-}" ]; then
        eval "_val=\${$PROBE_SECRET_ENV:-}"
        [ -n "$_val" ] && printf '%s=%s\n' "$PROBE_SECRET_ENV" "$_val" >>"$1"
    fi
    return 0
}
