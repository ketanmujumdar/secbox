#!/bin/sh
# Tests the probes' reporting logic on the machine running it, with a throwaway
# listener on 127.0.0.1 and a stub docker. It proves that the probes report
# correctly. It proves nothing about any VM's isolation.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
P=$HERE/probes
T=$(mktemp -d)
FAILS=0
CHECKS=0
SRV_PID=
LIS_PID=
DENY_PID=
trap '[ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null;
      [ -n "$LIS_PID" ] && kill "$LIS_PID" 2>/dev/null;
      [ -n "$DENY_PID" ] && kill "$DENY_PID" 2>/dev/null; rm -rf "$T"' EXIT

ok() { # ok LABEL PATTERN OUTPUT
    CHECKS=$((CHECKS + 1))
    if printf '%s\n' "$3" | grep -qE "$2"; then
        printf 'ok   %s\n' "$1"
    else
        printf 'FAIL %s\n  wanted: %s\n  output: %s\n' "$1" "$2" "$3"
        FAILS=$((FAILS + 1))
    fi
}
no() { # no LABEL PATTERN OUTPUT
    CHECKS=$((CHECKS + 1))
    if printf '%s\n' "$3" | grep -qE "$2"; then
        printf 'FAIL %s\n  unwanted: %s\n  output: %s\n' "$1" "$2" "$3"
        FAILS=$((FAILS + 1))
    else
        printf 'ok   %s\n' "$1"
    fi
}
wellformed() { # every non-empty line must match the contract
    CHECKS=$((CHECKS + 1))
    _bad=$(printf '%s\n' "$2" | grep -vE '^PROBE [^ ]+ (REACHED|BLOCKED|SKIPPED) [^ ].*$' | grep -v '^$')
    if [ -z "$_bad" ]; then printf 'ok   %s well-formed\n' "$1"; else
        printf 'FAIL %s malformed: %s\n' "$1" "$_bad"; FAILS=$((FAILS + 1)); fi
}

TOKEN=selftest-token-1234
FRONT_MARKER=FRONTED-DENIED-HOST
# One stub stands in for every service the probes talk to: it echoes the Host
# header when it is not our own (the fronting case), answers PUT the way a
# registry answers a publish with a bad token, answers POST the way a payment
# API answers a bad key, and otherwise returns the token.
cat >"$T/srv.py" <<PY
import http.server
class H(http.server.BaseHTTPRequestHandler):
    def answer(self):
        host = self.headers.get("Host", "").split(":")[0]
        if host and host != "127.0.0.1":
            body = ("$FRONT_MARKER %s\n" % host).encode()
        elif self.command == "PUT":
            body = b'{"error":"unauthorized","reason":"fake token"}\n'
        elif self.command == "POST":
            body = b'{"error":{"type":"invalid_request_error"}}\n'
        else:
            hdr = self.headers.get("X-Probe-Secret")
            body = ("$TOKEN\n" if hdr is None else "X-Probe-Secret: %s\n" % hdr).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    do_GET = do_PUT = do_POST = answer
    def log_message(self, *a): pass
s = http.server.HTTPServer(("127.0.0.1", 0), H)
print(s.server_address[1], flush=True)
s.serve_forever()
PY
cat >"$T/deny.py" <<'PY'
import http.server
class H(http.server.BaseHTTPRequestHandler):
    def answer(self):
        body = b'{"code":"network_policy_denied"}'
        self.send_response(403)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    do_GET = do_PUT = do_POST = answer
    def log_message(self, *a): pass
s = http.server.HTTPServer(("127.0.0.1", 0), H)
print(s.server_address[1], flush=True)
s.serve_forever()
PY
python3 -I "$T/srv.py" >"$T/port" &
SRV_PID=$!
i=0
while [ ! -s "$T/port" ] && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
PORT=$(cat "$T/port")
CLOSED=47913
export PROBE_TIMEOUT=2 PROBE_HOST_ALIAS=127.0.0.1

# Positive control must report REACHED before any BLOCKED is trusted.
out=$(PROBE_CONTROL_TARGET=127.0.0.1:$PORT sh "$P/control.sh")
wellformed control "$out"
ok "control.local reaches own listener" '^PROBE control\.local REACHED' "$out"
ok "control.closed detects closed port" '^PROBE control\.closed BLOCKED' "$out"
ok "control.allowed reaches listener" '^PROBE control\.allowed REACHED' "$out"
out=$(PROBE_CONTROL_TARGET=127.0.0.1:$CLOSED sh "$P/control.sh")
ok "failed control is SKIPPED, not BLOCKED" '^PROBE control\.allowed SKIPPED control-failed' "$out"
out=$(sh "$P/control.sh")
ok "unconfigured control is SKIPPED" '^PROBE control\.allowed SKIPPED target-not-configured' "$out"

# The same probe gives REACHED with a live target and BLOCKED with a closed one.
out=$(PROBE_CONTROL_TARGET=127.0.0.1:$PORT PROBE_LAN_TARGET=127.0.0.1:$PORT sh "$P/context.sh")
wellformed context "$out"
ok "lan probe REACHED on live target" '^PROBE lan\.host REACHED' "$out"
out=$(PROBE_CONTROL_TARGET=127.0.0.1:$PORT PROBE_LAN_TARGET=127.0.0.1:$CLOSED sh "$P/context.sh")
ok "lan probe BLOCKED after control passed" '^PROBE lan\.host BLOCKED' "$out"
out=$(PROBE_CONTROL_TARGET=127.0.0.1:$CLOSED PROBE_LAN_TARGET=127.0.0.1:$CLOSED sh "$P/context.sh")
ok "lan probe SKIPPED when control failed" '^PROBE lan\.host SKIPPED no-positive-control' "$out"
no "no BLOCKED net result without control" \
    '^PROBE (lan|host|metadata|denied|quic|ipv6|dns|fronting|write)[^ ]* BLOCKED' "$out"
out=$(PROBE_LAN_TARGET=127.0.0.1:$CLOSED sh "$P/net-lan.sh")
ok "probe run alone cannot report BLOCKED" '^PROBE lan\.host SKIPPED no-positive-control' "$out"
out=$(PROBE_CONTROL_OK=1 sh "$P/net-lan.sh")
ok "unconfigured target is SKIPPED" '^PROBE lan\.host SKIPPED target-not-configured' "$out"

# A target that drops packets must fail within the limit, not hang.
t0=$(date +%s)
out=$(PROBE_CONTROL_OK=1 sh "$P/net-metadata.sh")
t1=$(date +%s)
wellformed metadata "$out"
ok "metadata check finishes within the limit" '^1$' "$([ $((t1 - t0)) -le 8 ] && echo 1 || echo "took $((t1 - t0))s")"

# The gateway's own error page: BLOCKED even though it answered, and a token
# check must not count it as REACHED. An ordinary response is REACHED.
python3 -I "$T/deny.py" >"$T/denyport" &
DENY_PID=$!
i=0
while [ ! -s "$T/denyport" ] && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
DENY=$(cat "$T/denyport")
out=$(PROBE_LAN_TARGET=127.0.0.1:$DENY sh "$P/net-lan.sh")
wellformed net-lan "$out"
ok "gateway deny page is BLOCKED" '^PROBE lan\.host BLOCKED gateway-denied' "$out"
out=$(PROBE_HOST_PORT=$DENY PROBE_HOST_TOKEN=$TOKEN sh "$P/net-host.sh")
ok "deny page fails a token check" '^PROBE host\.loopback BLOCKED gateway-denied' "$out"
out=$(PROBE_LAN_TARGET=127.0.0.1:$PORT sh "$P/net-lan.sh")
ok "real response is REACHED" '^PROBE lan\.host REACHED response' "$out"
# $DENY stays up: the write and fronting sections below use it as the gateway.

# Token checks.
out=$(PROBE_CONTROL_OK=1 PROBE_HOST_PORT=$PORT PROBE_HOST_TOKEN=$TOKEN sh "$P/net-host.sh")
wellformed net-host "$out"
ok "token returned is REACHED" '^PROBE host\.loopback REACHED token-returned' "$out"
out=$(PROBE_CONTROL_OK=1 PROBE_HOST_PORT=$PORT PROBE_HOST_TOKEN=wrong-token sh "$P/net-host.sh")
ok "response without token is SKIPPED" '^PROBE host\.loopback SKIPPED ambiguous-response' "$out"
out=$(PROBE_CONTROL_OK=1 PROBE_HOST_PORT=$CLOSED PROBE_HOST_TOKEN=$TOKEN sh "$P/net-host.sh")
ok "closed port is BLOCKED with control" '^PROBE host\.loopback BLOCKED' "$out"

# Missing tools.
mkdir "$T/bin"
for t in cat grep sed tr sleep mktemp rm uname awk cut dirname sh; do
    ln -s "$(command -v "$t")" "$T/bin/$t"
done
out=$(PATH=$T/bin PROBE_CONTROL_OK=1 PROBE_LAN_TARGET=127.0.0.1:$PORT sh "$P/net-lan.sh")
ok "no nc is SKIPPED" '^PROBE lan\.host SKIPPED no-nc' "$out"
out=$(PATH=$T/bin PROBE_CONTROL_OK=1 PROBE_HOST_PORT=$PORT PROBE_HOST_TOKEN=$TOKEN sh "$P/net-host.sh")
ok "no http tool is SKIPPED" '^PROBE host\.loopback SKIPPED no-http-tool' "$out"
out=$(PATH=$T/bin PROBE_CONTROL_TARGET=127.0.0.1:$PORT sh "$P/control.sh")
ok "control without tools is SKIPPED" '^PROBE control\.closed SKIPPED no-tcp-tool' "$out"
no "no BLOCKED without tools" 'BLOCKED' "$out"

# Files.
touch "$T/canary" "$T/control"
out=$(PROBE_FILE_CONTROL=$T/control PROBE_HOST_CANARY=$T/canary sh "$P/files-host.sh")
wellformed files "$out"
ok "canary present is REACHED" '^PROBE files\.canary REACHED' "$out"
out=$(PROBE_FILE_CONTROL=$T/control PROBE_HOST_CANARY=$T/absent sh "$P/files-host.sh")
ok "canary absent is BLOCKED with control" '^PROBE files\.canary BLOCKED' "$out"
out=$(PROBE_HOST_CANARY=$T/absent sh "$P/files-host.sh")
ok "canary absent without control is SKIPPED" '^PROBE files\.canary SKIPPED' "$out"

# Secrets. Fake value only.
FAKE=fake-secret-value-abc
SHA=$(printf '%s' "$FAKE" | shasum -a 256 | cut -d' ' -f1)
out=$(FAKE_S=$FAKE PROBE_SECRET_ENV=FAKE_S PROBE_SECRET_SHA256=$SHA sh "$P/secret-env.sh")
ok "real value in env is REACHED" '^PROBE secret\.in-env REACHED' "$out"
no "secret value is never printed" "$FAKE" "$out"
out=$(FAKE_S=placeholder PROBE_SECRET_ENV=FAKE_S PROBE_SECRET_SHA256=$SHA sh "$P/secret-env.sh")
ok "placeholder in env is BLOCKED" '^PROBE secret\.in-env BLOCKED' "$out"
out=$(PROBE_SECRET_ENV=FAKE_UNSET PROBE_SECRET_SHA256=$SHA sh "$P/secret-env.sh")
ok "unset variable is SKIPPED" '^PROBE secret\.in-env SKIPPED' "$out"
out=$(PROBE_CONTROL_OK=1 FAKE_S=$FAKE PROBE_SECRET_ENV=FAKE_S PROBE_SECRET_SHA256=$SHA \
    PROBE_SECRET_ALLOWED_URL=http://127.0.0.1:$PORT/ sh "$P/secret-inject.sh")
ok "real value echoed is REACHED" '^PROBE secret\.inject-allowed REACHED' "$out"
no "inject does not print the value" "$FAKE" "$out"
out=$(PROBE_CONTROL_OK=1 FAKE_S=placeholder PROBE_SECRET_ENV=FAKE_S PROBE_SECRET_SHA256=$SHA \
    PROBE_SECRET_DENIED_URL=http://127.0.0.1:$PORT/ sh "$P/secret-inject.sh")
ok "placeholder echoed is BLOCKED" '^PROBE secret\.inject-denied BLOCKED header-arrived' "$out"

# Docker gating: on a non-Linux machine the real daemon must never be used.
if [ "$(uname -s)" != Linux ]; then
    out=$(sh "$P/nested-container.sh")
    ok "no stub: container refuses host daemon" '^PROBE container SKIPPED not-in-linux-guest' "$out"
    out=$(sh "$P/nested-build.sh")
    ok "no stub: build refuses host daemon" '^PROBE build SKIPPED not-in-linux-guest' "$out"
fi

# Stub docker. MODE: down | noimage | silent | ok
cat >"$T/docker" <<'STUB'
#!/bin/sh
case "$1" in
    info) [ "$STUB_MODE" != down ] ;;
    image) if [ "$2" = inspect ]; then [ "$STUB_MODE" != noimage ]; fi ;;
    network) exit 0 ;;
    run)
        [ "$STUB_MODE" = silent ] && { echo "exec failure"; exit 1; }
        echo "noise line"
        echo "PROBE container.stub.lan.host BLOCKED connect-failed:x"
        ;;
    build)
        [ "$STUB_MODE" = silent ] && { echo "pull denied"; exit 1; }
        for a in "$@"; do last=$a; done
        tag=$(sed -n 's/.*PROBE_PREFIX=\([^ ]*\) .*/\1/p' "$last/Dockerfile")
        echo "#7 0.123 PROBE ${tag}lan.host BLOCKED connect-failed:x"
        ;;
esac
STUB
chmod +x "$T/docker"
for m in down noimage; do
    out=$(STUB_MODE=$m PROBE_DOCKER=$T/docker sh "$P/nested-container.sh")
    ok "container SKIPPED when stub is $m" '^PROBE container SKIPPED' "$out"
    out=$(STUB_MODE=$m PROBE_DOCKER=$T/docker sh "$P/nested-build.sh")
    ok "build SKIPPED when stub is $m" '^PROBE build SKIPPED' "$out"
done
out=$(STUB_MODE=silent PROBE_DOCKER=$T/docker sh "$P/nested-container.sh")
ok "silent container run is SKIPPED" '^PROBE container\.bridge\.run SKIPPED no-probe-output' "$out"
no "silent container run has no BLOCKED" 'BLOCKED' "$out"
out=$(STUB_MODE=silent PROBE_DOCKER=$T/docker sh "$P/nested-build.sh")
ok "silent build is SKIPPED" '^PROBE build\.buildkit\.run SKIPPED no-probe-output' "$out"
no "silent build has no BLOCKED" 'BLOCKED' "$out"
out=$(STUB_MODE=ok PROBE_DOCKER=$T/docker sh "$P/nested-container.sh")
wellformed nested-container "$out"
ok "container lines pass through" '^PROBE container\.stub\.lan\.host BLOCKED' "$out"
no "container noise filtered" 'noise' "$out"
out=$(STUB_MODE=ok PROBE_DOCKER=$T/docker sh "$P/nested-build.sh")
wellformed nested-build "$out"
ok "buildkit prefix stripped" '^PROBE build\.buildkit\.lan\.host BLOCKED' "$out"
ok "legacy builder reported too" '^PROBE build\.legacy\.lan\.host BLOCKED' "$out"

# --- Probes added for the gaps the spike lists as untested ----------------
# Everything below drives reporting logic against local stubs. None of it is
# evidence about any VM.

# Stub tools, kept in their own directory so the rest of PATH still works.
mkdir "$T/stub"
cat >"$T/stub/nslookup" <<'STUB'
#!/bin/sh
# Resolves a name only when it contains $STUB_DNS_OK, in BusyBox's output shape.
case "$1" in
    *"$STUB_DNS_OK"*) ;;
    *) echo "nslookup: can't resolve '$1'" >&2; exit 1 ;;
esac
printf 'Server:\t127.0.0.1\nAddress:\t127.0.0.1:53\n\nName:\t%s\nAddress: 127.0.0.1\n' "$1"
STUB
cat >"$T/stub/npm" <<'STUB'
#!/bin/sh
# Never talks to a registry: it prints what $STUB_NPM_OUT says and fails.
echo "$STUB_NPM_OUT"
exit 1
STUB
chmod +x "$T/stub/nslookup" "$T/stub/npm"

# IPv6. Loopback IPv4 URLs stand in for IPv6 literals here, because this check
# is about which branch the probe takes, not about having IPv6.
out=$(PROBE_CONTROL_OK=1 PROBE_IPV6_CONTROL_URL=http://127.0.0.1:$PORT/ \
    PROBE_IPV6_TARGET_URL=http://127.0.0.1:$PORT/ \
    PROBE_IPV6_METADATA_URL=http://127.0.0.1:$CLOSED/ sh "$P/net-ipv6.sh")
wellformed net-ipv6 "$out"
ok "ipv6 status line is informational" '^PROBE ipv6\.status SKIPPED info:' "$out"
ok "ipv6 control reached is REACHED" '^PROBE ipv6\.control REACHED' "$out"
ok "ipv6 literal target REACHED" '^PROBE ipv6\.public REACHED' "$out"
ok "ipv6 metadata BLOCKED with ipv6 control" '^PROBE ipv6\.metadata BLOCKED' "$out"
out=$(PROBE_CONTROL_OK=1 PROBE_IPV6_CONTROL_URL=http://127.0.0.1:$CLOSED/ \
    PROBE_IPV6_TARGET_URL=http://127.0.0.1:$CLOSED/ \
    PROBE_IPV6_METADATA_URL=http://127.0.0.1:$CLOSED/ sh "$P/net-ipv6.sh")
ok "no ipv6 route: control is SKIPPED" '^PROBE ipv6\.control SKIPPED control-failed' "$out"
ok "no ipv6 route: target is SKIPPED" '^PROBE ipv6\.public SKIPPED no-ipv6-control' "$out"
no "no ipv6 route: nothing is BLOCKED" 'BLOCKED' "$out"
out=$(PROBE_CONTROL_OK=1 PROBE_IPV6_CONTROL_URL=http://127.0.0.1:$PORT/ \
    PROBE_IPV6_TARGET_URL=http://127.0.0.1:$DENY/ sh "$P/net-ipv6.sh")
ok "ipv6 gateway deny page is BLOCKED" '^PROBE ipv6\.public BLOCKED gateway-denied' "$out"
out=$(sh "$P/net-ipv6.sh")
ok "unconfigured ipv6 is SKIPPED" '^PROBE ipv6\.control SKIPPED control-not-configured' "$out"
no "unconfigured ipv6 has no BLOCKED" 'BLOCKED' "$out"

# QUIC over UDP, against the real host listener, which answers UDP.
python3 -I "$HERE/host-listener.py" "$T/listener.log" "$T/lports" "$TOKEN" &
LIS_PID=$!
i=0
while [ ! -s "$T/lports" ] && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
read -r L_TCP L_UDP <"$T/lports"
: "$L_TCP"
UDP_CLOSED=47914
out=$(PROBE_TIMEOUT=1 PROBE_CONTROL_OK=1 PROBE_QUIC_CONTROL=127.0.0.1:$L_UDP \
    PROBE_QUIC_TARGET=127.0.0.1:$L_UDP sh "$P/net-quic.sh")
wellformed net-quic "$out"
ok "quic control answered is REACHED" '^PROBE quic\.control REACHED' "$out"
ok "quic answer is REACHED" '^PROBE quic\.denied REACHED udp-443-answered' "$out"
ok "unconfigured quic target is SKIPPED" '^PROBE quic\.denied-ip SKIPPED target-not-configured' "$out"
out=$(PROBE_TIMEOUT=1 PROBE_CONTROL_OK=1 PROBE_QUIC_CONTROL=127.0.0.1:$L_UDP \
    PROBE_QUIC_TARGET=127.0.0.1:$UDP_CLOSED sh "$P/net-quic.sh")
ok "quic silence is BLOCKED after control" '^PROBE quic\.denied BLOCKED no-answer' "$out"
out=$(PROBE_TIMEOUT=1 PROBE_CONTROL_OK=1 PROBE_QUIC_CONTROL=127.0.0.1:$UDP_CLOSED \
    PROBE_QUIC_TARGET=127.0.0.1:$UDP_CLOSED sh "$P/net-quic.sh")
ok "quic control failure is SKIPPED" '^PROBE quic\.control SKIPPED control-failed' "$out"
ok "quic silence without control is SKIPPED" '^PROBE quic\.denied SKIPPED no-positive-control' "$out"
no "quic without control has no BLOCKED" 'BLOCKED' "$out"
kill "$LIS_PID" 2>/dev/null
LIS_PID=

# DNS tunnelling by volume.
out=$(PATH=$T/stub:$PATH STUB_DNS_OK=. PROBE_CONTROL_OK=1 PROBE_RUN_ID=st \
    PROBE_DNS_CONTROL_NAME=allowed.example PROBE_DNS_TUNNEL_BASE=tunnel.invalid \
    PROBE_DNS_TUNNEL_COUNT=3 sh "$P/net-dns-volume.sh")
wellformed net-dns-volume "$out"
ok "dns control resolves" '^PROBE dns\.control REACHED' "$out"
ok "resolved subdomains are REACHED" '^PROBE dns\.tunnel\.volume REACHED answers:3/3' "$out"
ok "long label resolved is REACHED" '^PROBE dns\.tunnel\.payload REACHED' "$out"
out=$(PATH=$T/stub:$PATH STUB_DNS_OK=allowed PROBE_CONTROL_OK=1 PROBE_RUN_ID=st \
    PROBE_DNS_CONTROL_NAME=allowed.example PROBE_DNS_TUNNEL_BASE=tunnel.invalid \
    PROBE_DNS_TUNNEL_COUNT=3 sh "$P/net-dns-volume.sh")
ok "filtered subdomains are BLOCKED" '^PROBE dns\.tunnel\.volume BLOCKED no-answers:0/3' "$out"
ok "filtered long label is BLOCKED" '^PROBE dns\.tunnel\.payload BLOCKED' "$out"
out=$(PATH=$T/stub:$PATH STUB_DNS_OK=nothing-resolves PROBE_CONTROL_OK=1 PROBE_RUN_ID=st \
    PROBE_DNS_CONTROL_NAME=allowed.example PROBE_DNS_TUNNEL_BASE=tunnel.invalid \
    PROBE_DNS_TUNNEL_COUNT=3 sh "$P/net-dns-volume.sh")
ok "dns control failure is SKIPPED" '^PROBE dns\.control SKIPPED control-failed' "$out"
ok "no dns control means SKIPPED" '^PROBE dns\.tunnel\.volume SKIPPED no-positive-control' "$out"
no "no dns control means no BLOCKED" 'BLOCKED' "$out"
out=$(PATH=$T/bin PROBE_CONTROL_OK=1 PROBE_DNS_CONTROL_NAME=a PROBE_DNS_TUNNEL_BASE=b \
    sh "$P/net-dns-volume.sh")
ok "no nslookup is SKIPPED" '^PROBE dns\.tunnel\.volume SKIPPED no-nslookup' "$out"

# Domain fronting: the Host header names a host the TLS name does not.
out=$(PROBE_CONTROL_OK=1 PROBE_FRONT_URL=http://127.0.0.1:$PORT/ PROBE_FRONT_MARKER=$TOKEN \
    PROBE_FRONT_HOST=denied.example PROBE_FRONT_HOST_MARKER=$FRONT_MARKER \
    PROBE_FRONT_PLAIN_TARGET=127.0.0.1:$PORT sh "$P/net-fronting.sh")
wellformed net-fronting "$out"
ok "fronting control reaches allowed host" '^PROBE fronting\.control REACHED' "$out"
ok "fronted denied host is REACHED" '^PROBE fronting\.https REACHED denied-host-answered' "$out"
ok "cleartext fronting is REACHED" '^PROBE fronting\.http REACHED denied-host-answered' "$out"
out=$(PROBE_CONTROL_OK=1 PROBE_FRONT_URL=http://127.0.0.1:$PORT/ PROBE_FRONT_MARKER=$TOKEN \
    PROBE_FRONT_HOST=127.0.0.1 PROBE_FRONT_HOST_MARKER=$FRONT_MARKER \
    sh "$P/net-fronting.sh")
ok "front end that did not serve it is SKIPPED" \
    '^PROBE fronting\.https SKIPPED response-without-denied-marker' "$out"
out=$(PROBE_CONTROL_OK=1 PROBE_FRONT_URL=http://127.0.0.1:$DENY/ PROBE_FRONT_MARKER=$TOKEN \
    PROBE_FRONT_HOST=denied.example PROBE_FRONT_HOST_MARKER=$FRONT_MARKER \
    PROBE_FRONT_PLAIN_TARGET=127.0.0.1:$DENY sh "$P/net-fronting.sh")
ok "gateway deny of a fronted request is BLOCKED" '^PROBE fronting\.https BLOCKED gateway-denied' "$out"
ok "gateway deny of cleartext fronting is BLOCKED" '^PROBE fronting\.http BLOCKED gateway-denied' "$out"
out=$(PROBE_FRONT_URL=http://127.0.0.1:$CLOSED/ PROBE_FRONT_MARKER=$TOKEN \
    PROBE_FRONT_HOST=denied.example PROBE_FRONT_HOST_MARKER=$FRONT_MARKER \
    PROBE_FRONT_PLAIN_TARGET=127.0.0.1:$CLOSED sh "$P/net-fronting.sh")
ok "fronting control failure is SKIPPED" '^PROBE fronting\.control SKIPPED control-failed' "$out"
no "fronting without control has no BLOCKED" 'BLOCKED' "$out"
out=$(sh "$P/net-fronting.sh")
ok "unconfigured fronting is SKIPPED" '^PROBE fronting\.https SKIPPED not-configured' "$out"

# npm publish: a write to a host whose reads are allowed.
out=$(PATH=$T/stub:$PATH PROBE_CONTROL_OK=1 STUB_NPM_OUT='npm ERR! 401 Unauthorized - PUT' \
    PROBE_NPM_READ_URL=http://127.0.0.1:$PORT/npm PROBE_NPM_READ_MARKER=$TOKEN \
    PROBE_NPM_PUT_URL=http://127.0.0.1:$PORT/pkg sh "$P/write-npm.sh")
wellformed write-npm "$out"
ok "registry read control is REACHED" '^PROBE write\.npm-read REACHED' "$out"
ok "publish that reached the registry is REACHED" '^PROBE write\.npm-put REACHED write-reached' "$out"
ok "npm client that reached the registry is REACHED" '^PROBE write\.npm-cli REACHED' "$out"
no "publish never prints the fake token" 'npm_0000' "$out"
out=$(PATH=$T/stub:$PATH PROBE_CONTROL_OK=1 STUB_NPM_OUT='{"code":"network_policy_denied"}' \
    PROBE_NPM_READ_URL=http://127.0.0.1:$DENY/npm PROBE_NPM_READ_MARKER=$TOKEN \
    PROBE_NPM_PUT_URL=http://127.0.0.1:$DENY/pkg sh "$P/write-npm.sh")
ok "registry denied by the gateway is BLOCKED" '^PROBE write\.npm-read BLOCKED gateway-denied' "$out"
ok "publish refused by policy is BLOCKED" '^PROBE write\.npm-put BLOCKED refused-by-policy' "$out"
ok "npm client refused by policy is BLOCKED" '^PROBE write\.npm-cli BLOCKED refused-by-policy' "$out"
out=$(PATH=$T/stub:$PATH PROBE_CONTROL_OK=1 STUB_NPM_OUT='npm ERR! ECONNREFUSED' \
    PROBE_NPM_READ_URL=http://127.0.0.1:$CLOSED/npm PROBE_NPM_READ_MARKER=$TOKEN \
    PROBE_NPM_PUT_URL=http://127.0.0.1:$CLOSED/pkg sh "$P/write-npm.sh")
ok "unreadable registry is SKIPPED" '^PROBE write\.npm-read SKIPPED registry-not-readable' "$out"
ok "publish without a read control is SKIPPED" '^PROBE write\.npm-put SKIPPED no-positive-control' "$out"
no "publish without a read control is never BLOCKED" 'BLOCKED' "$out"
out=$(PATH=$T/bin PROBE_CONTROL_OK=1 PROBE_NPM_READ_URL=http://127.0.0.1:$PORT/npm \
    PROBE_NPM_PUT_URL=http://127.0.0.1:$PORT/pkg sh "$P/write-npm.sh")
ok "no npm client is SKIPPED" '^PROBE write\.npm-cli SKIPPED no-npm' "$out"
out=$(sh "$P/write-npm.sh")
ok "unconfigured publish is SKIPPED" '^PROBE write\.npm-put SKIPPED put-url-not-configured' "$out"

# A call to a payment API.
out=$(PROBE_CONTROL_OK=1 PROBE_PAYMENT_URL=http://127.0.0.1:$PORT/charges \
    PROBE_PAYMENT_PLAIN_TARGET=127.0.0.1:$PORT PROBE_PAYMENT_PLAIN_PATH=/charges \
    sh "$P/write-payment.sh")
wellformed write-payment "$out"
ok "payment call that arrived is REACHED" '^PROBE write\.payment REACHED write-reached' "$out"
ok "cleartext payment call that arrived is REACHED" '^PROBE write\.payment-plain REACHED' "$out"
no "payment call never prints the fake key" 'sk_test_' "$out"
out=$(PROBE_CONTROL_OK=1 PROBE_PAYMENT_URL=http://127.0.0.1:$DENY/charges \
    PROBE_PAYMENT_PLAIN_TARGET=127.0.0.1:$DENY PROBE_PAYMENT_PLAIN_PATH=/charges \
    sh "$P/write-payment.sh")
ok "payment refused by policy is BLOCKED" '^PROBE write\.payment BLOCKED refused-by-policy' "$out"
ok "cleartext payment refused by policy is BLOCKED" '^PROBE write\.payment-plain BLOCKED refused-by-policy' "$out"
out=$(PROBE_CONTROL_OK=1 PROBE_PAYMENT_URL=http://127.0.0.1:$CLOSED/charges sh "$P/write-payment.sh")
ok "payment that could not connect is BLOCKED with control" '^PROBE write\.payment BLOCKED connect-failed' "$out"
out=$(PROBE_PAYMENT_URL=http://127.0.0.1:$CLOSED/charges sh "$P/write-payment.sh")
ok "payment without control is SKIPPED" '^PROBE write\.payment SKIPPED no-positive-control' "$out"
no "payment without control is never BLOCKED" 'BLOCKED' "$out"

# Host unix sockets.
python3 -I -c "import socket, sys; s = socket.socket(socket.AF_UNIX); s.bind(sys.argv[1])" "$T/sock"
mkdir "$T/share" "$T/noshare"
python3 -I -c "import socket, sys; s = socket.socket(socket.AF_UNIX); s.bind(sys.argv[1])" "$T/share/s"
printf 'none %s virtiofs rw 0 0\n' "$T/share" >"$T/mounts"
printf 'none %s virtiofs rw 0 0\n' "$T/noshare" >"$T/mounts-empty"
printf 'none / ext4 rw 0 0\n' >"$T/mounts-none"
out=$(SSH_AUTH_SOCK=$T/sock PROBE_SOCKET_CONTROL=$T/sock PROBE_SOCKET_PATH_LIST=$T/sock \
    PROBE_MOUNTS_FILE=$T/mounts sh "$P/host-sockets.sh")
wellformed host-sockets "$out"
ok "socket control detects a socket" '^PROBE sockets\.control REACHED' "$out"
ok "host socket present is REACHED" '^PROBE sockets\.host-paths REACHED' "$out"
ok "socket in a host share is REACHED" '^PROBE sockets\.in-shares REACHED' "$out"
ok "ssh agent socket is REACHED" '^PROBE sockets\.ssh-agent REACHED' "$out"
out=$(unset SSH_AUTH_SOCK; PROBE_SOCKET_CONTROL=$T/sock PROBE_SOCKET_PATH_LIST=$T/absent \
    PROBE_MOUNTS_FILE=$T/mounts-empty sh "$P/host-sockets.sh")
ok "no host socket is BLOCKED with control" '^PROBE sockets\.host-paths BLOCKED' "$out"
ok "empty host share is BLOCKED with control" '^PROBE sockets\.in-shares BLOCKED' "$out"
ok "no ssh agent is BLOCKED with control" '^PROBE sockets\.ssh-agent BLOCKED' "$out"
out=$(unset SSH_AUTH_SOCK; PROBE_SOCKET_CONTROL=$T/sock PROBE_SOCKET_PATH_LIST=$T/absent \
    PROBE_MOUNTS_FILE=$T/mounts-none sh "$P/host-sockets.sh")
ok "no host share to search is SKIPPED" '^PROBE sockets\.in-shares SKIPPED no-host-shares' "$out"
out=$(unset SSH_AUTH_SOCK; PROBE_SOCKET_CONTROL=$T/absent PROBE_SOCKET_PATH_LIST=$T/absent \
    PROBE_MOUNTS_FILE=$T/mounts-empty sh "$P/host-sockets.sh")
ok "socket control failure is SKIPPED" '^PROBE sockets\.control SKIPPED control-failed' "$out"
no "no socket result is BLOCKED without control" 'BLOCKED' "$out"

# The host keychain. The probe must only look, never read.
no "keychain probe never reads a keychain" \
    '(find-generic-password|secret-tool +lookup|security +find)' "$(cat "$P/host-keychain.sh")"
out=$(DBUS_SESSION_BUS_ADDRESS=unix:path=$T/sock PROBE_KEYCHAIN_CONTROL=$T/control \
    PROBE_KEYCHAIN_PATH_LIST=$T/canary PROBE_KEYCHAIN_TOOL_LIST=sh sh "$P/host-keychain.sh")
wellformed host-keychain "$out"
ok "keychain control works" '^PROBE keychain\.control REACHED' "$out"
ok "keychain path present is REACHED" '^PROBE keychain\.paths REACHED' "$out"
ok "keychain reader present is REACHED" '^PROBE keychain\.tools REACHED' "$out"
ok "session bus socket is REACHED" '^PROBE keychain\.dbus REACHED' "$out"
out=$(unset DBUS_SESSION_BUS_ADDRESS; PROBE_KEYCHAIN_CONTROL=$T/control \
    PROBE_KEYCHAIN_PATH_LIST=$T/absent PROBE_KEYCHAIN_TOOL_LIST=no-such-tool-here \
    sh "$P/host-keychain.sh")
ok "no keychain path is BLOCKED with control" '^PROBE keychain\.paths BLOCKED' "$out"
ok "no keychain reader is BLOCKED with control" '^PROBE keychain\.tools BLOCKED' "$out"
ok "no session bus is BLOCKED with control" '^PROBE keychain\.dbus BLOCKED' "$out"
out=$(unset DBUS_SESSION_BUS_ADDRESS; PROBE_KEYCHAIN_CONTROL=$T/absent \
    PROBE_KEYCHAIN_PATH_LIST=$T/absent PROBE_KEYCHAIN_TOOL_LIST=no-such-tool-here \
    sh "$P/host-keychain.sh")
ok "keychain control failure is SKIPPED" '^PROBE keychain\.control SKIPPED control-failed' "$out"
no "no keychain result is BLOCKED without control" 'BLOCKED' "$out"

# Plants in the engine's guest-state share.
mkdir "$T/gs"
out=$(PROBE_RUN_ID=st1 PROBE_GUEST_STATE=$T/gs sh "$P/host-share.sh")
wellformed host-share "$out"
ok "share control works" '^PROBE share\.control REACHED' "$out"
ok "share presence is informational" '^PROBE share\.present SKIPPED info:' "$out"
ok "writing into the share is REACHED" '^PROBE share\.writable REACHED' "$out"
ok "symlink made in the share is REACHED" '^PROBE share\.symlink-created REACHED' "$out"
ok "plain plant is reported" '^PROBE plant\.control SKIPPED planted:leak-st1-plain' "$out"
ok "absolute symlink plant is reported" '^PROBE plant\.symlink-abs SKIPPED planted:leak-st1-abs' "$out"
ok "traversal symlink plant is reported" '^PROBE plant\.symlink-traversal SKIPPED planted:leak-st1-esc' "$out"
ok "the plants really exist" '^1$' "$([ -f "$T/gs/leak-st1-plain" ] && [ -L "$T/gs/leak-st1-abs" ] && echo 1)"
out=$(PROBE_RUN_ID=st2 PROBE_GUEST_STATE=$T/absent sh "$P/host-share.sh")
ok "no share is SKIPPED" '^PROBE share\.writable SKIPPED no-share-at' "$out"
no "no share is never BLOCKED" 'BLOCKED' "$out"
mkdir "$T/gs3"
out=$(PATH=$T/bin PROBE_RUN_ID=st3 PROBE_GUEST_STATE=$T/gs3 sh "$P/host-share.sh")
ok "share control failure is SKIPPED" '^PROBE share\.control SKIPPED control-failed' "$out"
ok "no symlink without control is SKIPPED" '^PROBE share\.symlink-created SKIPPED no-positive-control' "$out"
no "no share result is BLOCKED without control" 'BLOCKED' "$out"

# A malicious hook carried in a bundle built in the guest.
out=$(PROBE_RUN_ID=st4 PROBE_GUEST_STATE=$T/gs sh "$P/git-bundle.sh")
wellformed git-bundle "$out"
ok "git is usable for the bundle" '^PROBE bundle\.control REACHED' "$out"
ok "bundle is built" '^PROBE bundle\.created REACHED' "$out"
ok "hook travels in the bundle" '^PROBE bundle\.hook-in-tree REACHED payload-travels' "$out"
ok "vscode task travels in the bundle" 'vscode/tasks\.json' "$out"
ok "bundle plant is reported" '^PROBE plant\.bundle SKIPPED planted:leak-st4\.bundle' "$out"
ok "the bundle really exists" '^1$' "$([ -s "$T/gs/leak-st4.bundle" ] && echo 1)"
out=$(PATH=$T/bin sh "$P/git-bundle.sh")
ok "no git is SKIPPED" '^PROBE bundle\.control SKIPPED no-git' "$out"
no "no git is never BLOCKED" 'BLOCKED' "$out"

# verify.py turns plants into results from the host side, without following any
# link the guest made.
: >"$T/empty.log"
cat >"$T/plants.txt" <<TXT
PROBE guest.plant.control SKIPPED planted:leak-st1-plain
PROBE guest.plant.symlink-abs SKIPPED planted:leak-st1-abs
PROBE guest.plant.bundle SKIPPED planted:leak-absent.bundle
TXT
out=$(python3 -I "$HERE/verify.py" "$T/plants.txt" "$T/empty.log" "$T/gs")
wellformed verify-plants "$out"
ok "arrived plain plant is REACHED" \
    '^PROBE guest\.plant\.arrived\.control REACHED guest-write-landed-on-host:real-host-file' "$out"
ok "arrived symlink is reported as a real host symlink" \
    '^PROBE guest\.plant\.arrived\.symlink-abs REACHED .*real-host-symlink-to:/etc/hosts' "$out"
ok "plant that did not arrive is BLOCKED" \
    '^PROBE guest\.plant\.arrived\.bundle BLOCKED not-on-host' "$out"
out=$(python3 -I "$HERE/verify.py" "$T/plants.txt" "$T/empty.log")
ok "no guest-state directory means SKIPPED" \
    '^PROBE guest\.plant\.arrived\.control SKIPPED no-positive-control:guest-state-not-configured' "$out"
no "no guest-state directory means no BLOCKED" 'BLOCKED' "$out"
cat >"$T/plants2.txt" <<TXT
PROBE guest.plant.control SKIPPED planted:leak-absent-plain
PROBE guest.plant.symlink-abs SKIPPED planted:leak-absent-abs
TXT
out=$(python3 -I "$HERE/verify.py" "$T/plants2.txt" "$T/empty.log" "$T/gs")
ok "plants without their control are SKIPPED" \
    '^PROBE guest\.plant\.arrived\.symlink-abs SKIPPED no-positive-control' "$out"
no "plants without their control are never BLOCKED" 'BLOCKED' "$out"
cat >"$T/plants3.txt" <<TXT
PROBE guest.plant.control SKIPPED planted:../outside
TXT
out=$(python3 -I "$HERE/verify.py" "$T/plants3.txt" "$T/empty.log" "$T/gs")
ok "a plant name that climbs out is refused" 'SKIPPED no-positive-control:unsafe-plant-name' "$out"

printf '%s checks, %s failed\n' "$CHECKS" "$FAILS"
[ "$FAILS" -eq 0 ]
