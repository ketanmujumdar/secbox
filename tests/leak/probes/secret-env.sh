#!/bin/sh
# Is the real secret visible in this context's environment? Compares hashes, so
# no value is printed. Env: PROBE_SECRET_ENV=<var name>, PROBE_SECRET_SHA256
. "$(dirname "$0")/lib.sh"
if [ -z "${PROBE_SECRET_ENV:-}" ] || [ -z "${PROBE_SECRET_SHA256:-}" ]; then
    emit secret.in-env SKIPPED "secret-not-configured"
    exit 0
fi
eval "_val=\${$PROBE_SECRET_ENV:-}"
if [ -z "$_val" ]; then
    emit secret.in-env SKIPPED "variable-not-set:$PROBE_SECRET_ENV"
    exit 0
fi
if ! _hash=$(sha256_of "$_val") || [ -z "$_hash" ]; then
    emit secret.in-env SKIPPED "no-sha256-tool"
elif [ "$_hash" = "$PROBE_SECRET_SHA256" ]; then
    emit secret.in-env REACHED "real-value-in-env:$PROBE_SECRET_ENV"
else
    emit secret.in-env BLOCKED "placeholder-in-env:$PROBE_SECRET_ENV"
fi
