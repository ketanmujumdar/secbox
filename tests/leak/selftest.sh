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
trap '[ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null; rm -rf "$T"' EXIT

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
cat >"$T/srv.py" <<PY
import http.server
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        hdr = self.headers.get("X-Probe-Secret")
        body = ("$TOKEN\n" if hdr is None else "X-Probe-Secret: %s\n" % hdr).encode()
        self.send_response(200); self.end_headers(); self.wfile.write(body)
    def log_message(self, *a): pass
s = http.server.HTTPServer(("127.0.0.1", 0), H)
print(s.server_address[1], flush=True)
s.serve_forever()
PY
cat >"$T/deny.py" <<'PY'
import http.server
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b'{"code":"network_policy_denied"}'
        self.send_response(403); self.end_headers(); self.wfile.write(body)
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
no "no BLOCKED net result without control" '^PROBE (lan|host|metadata|denied)[^ ]* BLOCKED' "$out"
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
kill "$DENY_PID" 2>/dev/null

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

printf '%s checks, %s failed\n' "$CHECKS" "$FAILS"
[ "$FAILS" -eq 0 ]
