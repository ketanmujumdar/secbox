#!/bin/sh
# Runs context.sh inside nested containers: default bridge, user-defined bridge
# (what Compose uses) and host network. Each runs its own controls.
_dir=$(cd "$(dirname "$0")" && pwd)
. "$_dir/lib.sh"

if ! docker_ready; then
    emit container SKIPPED "$DOCKER_WHY"
    exit 0
fi

_envf=$(mktemp)
write_env_file "$_envf"
_net=probe-leak-net
"$DOCKER" network create "$_net" >/dev/null 2>&1

for _mode in bridge "$_net" host; do
    case $_mode in
        bridge) _tag=container.bridge. ;;
        host) _tag=container.host. ;;
        *) _tag=container.compose. ;;
    esac
    _out=$("$DOCKER" run --rm --pull=never --network "$_mode" --env-file "$_envf" \
        -e "PROBE_PREFIX=$_tag" -v "$_dir:/probes:ro" "$PROBE_IMAGE" sh /probes/context.sh 2>&1)
    _lines=$(printf '%s\n' "$_out" | grep '^PROBE ')
    if [ -n "$_lines" ]; then
        printf '%s\n' "$_lines"
    else
        PROBE_PREFIX=$_tag emit run SKIPPED "no-probe-output:$(printf '%s' "$_out" | tail -n 1)"
    fi
done

"$DOCKER" network rm "$_net" >/dev/null 2>&1
rm -f "$_envf"
