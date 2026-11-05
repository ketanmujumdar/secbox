#!/bin/sh
# Runs context.sh inside `docker build` RUN steps, with BuildKit and with the
# legacy builder, since their networking differs. Each runs its own controls.
_dir=$(cd "$(dirname "$0")" && pwd)
. "$_dir/lib.sh"

if ! docker_ready; then
    emit build SKIPPED "$DOCKER_WHY"
    exit 0
fi

for _mode in buildkit legacy; do
    _tag=build.$_mode.
    _ctx=$(mktemp -d)
    mkdir "$_ctx/probes"
    cp "$_dir"/*.sh "$_ctx/probes/"
    # Sourced by sh, so values with spaces must be quoted. A value containing a
    # single quote is not supported; only harness-made test values go here.
    write_env_file "$_ctx/probes/env.raw"
    sed "s/=\(.*\)$/='\1'/" "$_ctx/probes/env.raw" >"$_ctx/probes/env"
    rm -f "$_ctx/probes/env.raw"
    cat >"$_ctx/Dockerfile" <<DOCKERFILE
FROM $PROBE_IMAGE
COPY probes /probes
RUN set -a; . /probes/env; set +a; PROBE_PREFIX=$_tag sh /probes/context.sh
DOCKERFILE
    if [ "$_mode" = buildkit ]; then
        _out=$(DOCKER_BUILDKIT=1 "$DOCKER" build --no-cache --progress=plain --pull=false -t probe-leak-build "$_ctx" 2>&1)
    else
        _out=$(DOCKER_BUILDKIT=0 "$DOCKER" build --no-cache -t probe-leak-build "$_ctx" 2>&1)
    fi
    # BuildKit prefixes step output with "#7 0.123 ".
    _lines=$(printf '%s\n' "$_out" | sed -n "s/^.*\(PROBE ${_tag}\)/\1/p")
    if [ -n "$_lines" ]; then
        printf '%s\n' "$_lines"
    else
        PROBE_PREFIX=$_tag emit run SKIPPED "no-probe-output:$(printf '%s' "$_out" | tail -n 1)"
    fi
    rm -rf "$_ctx"
done
"$DOCKER" image rm -f probe-leak-build >/dev/null 2>&1
exit 0
