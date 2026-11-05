#!/bin/sh
# `npm publish` against a registry host the allowlist permits. Reads from that
# host have to work for a build, so a host rule cannot stop a publish: only a
# method rule can, and the spike records that method rules need TLS
# interception. This probe is what tells the two apart.
#
#   write.npm-read   GET on the same host. This is the probe's own control: if
#                    reads do not work either, a blocked write says nothing
#                    about methods, so the write is SKIPPED.
#   write.npm-put    the HTTP request `npm publish` makes, a PUT of the package
#                    document. REACHED means the write left the sandbox and the
#                    registry judged it, not that anything was published: the
#                    token is fake, so the registry answers 401.
#   write.npm-cli    the real client, when the guest has one.
#
# Nothing here can publish: the package name is a scope that does not exist and
# the token is a fixed fake string. No real credential is ever read.
#
# Env: PROBE_NPM_READ_URL PROBE_NPM_READ_MARKER PROBE_NPM_PUT_URL PROBE_NPM_PUT_MARKER
. "$(dirname "$0")/lib.sh"

FAKE_TOKEN=npm_0000000000000000000000000000000000ff

NPM_OK=0
if [ -z "${PROBE_NPM_READ_URL:-}" ]; then
    emit write.npm-read SKIPPED "read-url-not-configured"
else
    FETCH_ERR=
    fetch "$PROBE_NPM_READ_URL"
    _rc=$?
    if [ "$_rc" -eq 0 ] && [ -n "${PROBE_NPM_READ_MARKER:-}" ] &&
        grep -qF "$PROBE_NPM_READ_MARKER" "$FETCH_BODY"; then
        emit write.npm-read REACHED "registry-readable:$PROBE_NPM_READ_URL"
        NPM_OK=1
    elif { [ -s "${FETCH_BODY:-/dev/null}" ] && grep -q "$GATEWAY_DENY_MARKER" "$FETCH_BODY"; } ||
        printf '%s' "${FETCH_ERR:-}" | grep -q "$GATEWAY_DENY_MARKER"; then
        emit write.npm-read BLOCKED "gateway-denied:registry-host-not-allowed-here"
    else
        emit write.npm-read SKIPPED "registry-not-readable:$PROBE_NPM_READ_URL"
    fi
    rm -f "${FETCH_BODY:-}"
fi

if [ -z "${PROBE_NPM_PUT_URL:-}" ]; then
    emit write.npm-put SKIPPED "put-url-not-configured"
else
    _pkg=${PROBE_NPM_PUT_URL##*/}
    http_send PUT "$PROBE_NPM_PUT_URL" \
        "{\"_id\":\"$_pkg\",\"name\":\"$_pkg\",\"description\":\"leak probe, never published\"}" \
        "Authorization: Bearer $FAKE_TOKEN"
    write_result write.npm-put $? "${PROBE_NPM_PUT_MARKER:-unauthorized}" "$NPM_OK"
    rm -f "${FETCH_BODY:-}"
fi

# The real client, if the guest has one. docker:dind has no npm, so this is
# normally SKIPPED.
if ! have npm; then
    emit write.npm-cli SKIPPED "no-npm"
    exit 0
fi
if [ -z "${PROBE_NPM_READ_URL:-}" ]; then
    emit write.npm-cli SKIPPED "registry-not-configured"
    exit 0
fi
_reg=${PROBE_NPM_READ_URL%/*}
_dir=$(mktemp -d 2>/dev/null) || { emit write.npm-cli SKIPPED "no-tmpdir"; exit 0; }
_name=secbox-leak-probe-${PROBE_RUN_ID:-0}
cat >"$_dir/package.json" <<JSON
{"name":"$_name","version":"0.0.0-leakprobe","private":false,
 "description":"leak probe, never published","license":"UNLICENSED"}
JSON
printf '//registry.npmjs.org/:_authToken=%s\nregistry=%s\n' "$FAKE_TOKEN" "$_reg" >"$_dir/.npmrc"
if have timeout; then
    _out=$(cd "$_dir" && HOME=$_dir timeout "$((PROBE_TIMEOUT * 4))" npm publish --registry "$_reg" 2>&1)
else
    _out=$(cd "$_dir" && HOME=$_dir npm publish --registry "$_reg" 2>&1)
fi
rm -rf "$_dir"
if printf '%s' "$_out" | grep -q "$GATEWAY_DENY_MARKER"; then
    emit write.npm-cli BLOCKED "refused-by-policy:gateway-denied"
elif printf '%s' "$_out" | grep -qiF "${PROBE_NPM_PUT_MARKER:-unauthorized}"; then
    emit write.npm-cli REACHED "write-reached-the-registry:marker-seen"
elif printf '%s' "$_out" | grep -qiE 'ENOTFOUND|EAI_AGAIN|ECONNREFUSED|ETIMEDOUT|network'; then
    gated_block_if write.npm-cli "$NPM_OK" "npm-could-not-connect"
else
    emit write.npm-cli SKIPPED "npm-output-not-conclusive:$(printf '%s' "$_out" | tail -n 1)"
fi
