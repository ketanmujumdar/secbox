#!/bin/sh
# Boots a sandbox for one scenario, runs the probes in it, verifies canaries on
# the host, then removes the sandbox and stops the listener.
# Usage: run-guest.sh default|allowlist|allowlist-dns|allowlist-intercept|host-allowed|secrets [OUTPUT_FILE]
# Run host-allowed too: it is the positive control showing that the canary and
# host checks report REACHED when the host really is reachable.
# Env: MSB (path to the CLI), MSB_HOME
set -u
SCENARIO=$1
OUT=${2:-/dev/stdout}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
MSB=${MSB:-$ROOT/.tools/msb/bin/msb}
export MSB_HOME=${MSB_HOME:-$HOME/.secbox/msb}
NAME=leak-$SCENARIO
T=$(mktemp -d)
LPID=
cleanup() {
    "$MSB" remove --force --quiet "$NAME" 2>/dev/null
    [ -n "$LPID" ] && kill "$LPID" 2>/dev/null
    rm -rf "$T"
}
trap cleanup EXIT

RUN_ID=$(date +%s)$$
TOKEN=token-$RUN_ID
touch "$T/listener.log"
python3 -I "$HERE/host-listener.py" "$T/listener.log" "$T/ports" "$TOKEN" &
LPID=$!
i=0
while [ ! -s "$T/ports" ] && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
read -r TCP_PORT UDP_PORT <"$T/ports"

# registry.npmjs.org is allowed on purpose: a build needs to read from it, so a
# host rule cannot stop `npm publish`. Allowing it is what makes write-npm.sh a
# test of method rules instead of host rules. api.stripe.com is deliberately
# never allowed, so write-payment.sh tests the host rule.
DOCKER_HOSTS="registry-1.docker.io auth.docker.io production.cloudflare.docker.com production.cloudfront.docker.com docker-images-prod.6aa30f8b08e16409b46e0173d6de2f56.r2.cloudflarestorage.com registry.npmjs.org"
rules=
for h in $DOCKER_HOSTS; do rules="$rules --net-rule allow@$h"; done
CONTROL=registry-1.docker.io:443
ALLOWED_URL=http://registry-1.docker.io/v2/
ALLOWED_MARKER=awselb
SECRET_FLAGS=
FAKE_VALUE=fake-secret-$RUN_ID
case $SCENARIO in
    default) FLAGS= ; ALLOWED_URL=http://example.com/ ; ALLOWED_MARKER='Example Domain' ;;
    # Hostname rules alone deny HTTPS in strict mode, so each allowlist needs
    # either --net-strict=false (SNI only) or --tls-intercept.
    # No allow@dns: without it DNS is filtered to the allowed names. See
    # allowlist-dns for what that rule opens up.
    allowlist) FLAGS="--no-net $rules --net-strict=false" ;;
    allowlist-dns) FLAGS="--no-net $rules --net-rule allow@dns --net-strict=false" ;;
    allowlist-intercept) FLAGS="--no-net $rules --tls-intercept" ;;
    host-allowed) FLAGS="--net host --net public" ;;
    secrets)
        FLAGS="--no-net $rules --net-rule allow@httpbin.org --net-rule allow@postman-echo.com --tls-intercept"
        SECRET_FLAGS=1
        ;;
    *) echo "unknown scenario $SCENARIO" >&2; exit 2 ;;
esac

# shellcheck disable=SC2086
if [ -n "$SECRET_FLAGS" ]; then
    PROBE_FAKE_SECRET=$FAKE_VALUE "$MSB" create docker:dind --name "$NAME" --replace --memory 2G --cpus 2 \
        --root-disk flat:10G --quiet $FLAGS --secret 'PROBE_FAKE_SECRET@httpbin.org' || exit 1
else
    "$MSB" create docker:dind --name "$NAME" --replace --memory 2G --cpus 2 \
        --root-disk flat:10G --quiet $FLAGS || exit 1
fi

"$MSB" exec -d "$NAME" --no-stdin -- sh -c 'dockerd >/tmp/dockerd.log 2>&1' >/dev/null
i=0
until "$MSB" exec "$NAME" --no-stdin --no-tty -- docker info >/dev/null 2>&1 || [ "$i" -ge 120 ]; do
    i=$((i + 1)); sleep 0.5
done
"$MSB" exec "$NAME" --no-stdin --no-tty --timeout 120s -- docker pull -q alpine:3.20 >"$T/pull" 2>&1
echo "# scenario=$SCENARIO flags=$FLAGS pull: $(tail -n 1 "$T/pull")" >&2

"$MSB" copy --quiet "$HERE/probes" "$NAME:/probes" || exit 1

CANARY_FILE=$T/host-canary-$RUN_ID
touch "$CANARY_FILE"
LAN_IP=$(ipconfig getifaddr en0 2>/dev/null)
ROUTER=$(route -n get default 2>/dev/null | awk '/gateway:/ {print $2}')
# AirPlay on the LAN address: already listening, harmless to connect to.
LAN_TARGET=${PROBE_LAN_TARGET:-${LAN_IP:+$LAN_IP:7000}}
LAN_ROUTER=${PROBE_LAN_ROUTER:-${ROUTER:+$ROUTER:80}}

SHA=$(printf '%s' "$FAKE_VALUE" | shasum -a 256 | cut -d' ' -f1)
# shellcheck disable=SC2086
"$MSB" exec "$NAME" --no-stdin --no-tty --timeout 15m \
    -e PROBE_TIMEOUT="${PROBE_TIMEOUT:-5}" \
    -e PROBE_RUN_ID="$RUN_ID" \
    -e PROBE_CONTROL_TARGET="$CONTROL" \
    -e PROBE_HOST_PORT="$TCP_PORT" \
    -e PROBE_HOST_UDP_PORT="$UDP_PORT" \
    -e PROBE_HOST_TOKEN="$TOKEN" \
    -e PROBE_LAN_TARGET="$LAN_TARGET" \
    -e PROBE_LAN_ROUTER="$LAN_ROUTER" \
    -e PROBE_HOST_CANARY="$CANARY_FILE" \
    -e PROBE_DENIED_TARGET=example.com:443 \
    -e PROBE_DENIED_IP=1.1.1.1:443 \
    -e PROBE_DENIED_DNS_NAME=example.org \
    -e PROBE_DENIED_URL=http://example.com/ \
    -e PROBE_DENIED_MARKER='Example Domain' \
    -e PROBE_ALLOWED_URL="$ALLOWED_URL" \
    -e PROBE_DENIED_HTTPS_URL=https://example.com/ \
    -e PROBE_ALLOWED_HTTPS_URL=https://registry-1.docker.io/v2/ \
    -e PROBE_ALLOWED_MARKER="$ALLOWED_MARKER" \
    -e PROBE_FILE_CONTROL=/probes/lib.sh \
    -e PROBE_SECRET_ENV="${SECRET_FLAGS:+PROBE_FAKE_SECRET}" \
    -e PROBE_SECRET_SHA256="${SECRET_FLAGS:+$SHA}" \
    -e PROBE_SECRET_ALLOWED_URL="${SECRET_FLAGS:+https://httpbin.org/headers}" \
    -e PROBE_SECRET_DENIED_URL="${SECRET_FLAGS:+https://postman-echo.com/headers}" \
    -e PROBE_SECRET_PLAIN_URL="${SECRET_FLAGS:+http://httpbin.org/headers}" \
    -e PROBE_WGET_INSECURE="${PROBE_WGET_INSECURE:-}" \
    -e PROBE_GUEST_STATE=/.msb \
    -e PROBE_IPV6_CONTROL_URL="http://[2606:4700:4700::1111]/" \
    -e PROBE_IPV6_TARGET_URL="http://[2001:4860:4860::8888]/" \
    -e PROBE_IPV6_METADATA_URL="http://[fd00:ec2::254]/latest/meta-data/" \
    -e PROBE_QUIC_CONTROL=production.cloudflare.docker.com:443 \
    -e PROBE_QUIC_TARGET=www.google.com:443 \
    -e PROBE_QUIC_IP=8.8.8.8:443 \
    -e PROBE_DNS_CONTROL_NAME=registry-1.docker.io \
    -e PROBE_DNS_TUNNEL_BASE=example.org \
    -e PROBE_DNS_TUNNEL_COUNT="${PROBE_DNS_TUNNEL_COUNT:-12}" \
    -e PROBE_FRONT_URL=https://registry-1.docker.io/v2/ \
    -e PROBE_FRONT_MARKER=401 \
    -e PROBE_FRONT_HOST=example.com \
    -e PROBE_FRONT_HOST_MARKER='Example Domain' \
    -e PROBE_FRONT_PLAIN_TARGET=registry-1.docker.io:80 \
    -e PROBE_FRONT_PLAIN_PATH=/v2/ \
    -e PROBE_NPM_READ_URL=https://registry.npmjs.org/npm \
    -e PROBE_NPM_READ_MARKER=dist-tags \
    -e PROBE_NPM_PUT_URL="https://registry.npmjs.org/secbox-leak-probe-$RUN_ID" \
    -e PROBE_NPM_PUT_MARKER=unauthorized \
    -e PROBE_PAYMENT_URL=https://api.stripe.com/v1/charges \
    -e PROBE_PAYMENT_MARKER=invalid_request_error \
    -e PROBE_PAYMENT_PLAIN_TARGET=api.stripe.com:80 \
    -e PROBE_PAYMENT_PLAIN_PATH=/v1/charges \
    -- sh /probes/run-all.sh >"$T/raw" 2>&1
echo "# exec exit=$?" >&2
# The host side of the engine's guest-state share. verify.py only lstats names
# inside it: nothing here follows a path the guest wrote.
GUEST_STATE=$MSB_HOME/sandboxes/$NAME/guest-state
[ -d "$GUEST_STATE" ] || GUEST_STATE=
python3 -I "$HERE/verify.py" "$T/raw" "$T/listener.log" "$GUEST_STATE" >"$OUT"
echo "# listener log:" >&2
cut -c1-160 "$T/listener.log" | sort | uniq -c >&2
