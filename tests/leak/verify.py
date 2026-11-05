#!/usr/bin/env python3
"""Turns canary sends into results by checking the host listener's log.

Reads probe output and the log, prints every probe line, and adds one line per
canary. A canary that did not arrive is BLOCKED only if that context's controls
passed. Usage: verify.py PROBE_OUTPUT LISTENER_LOG
"""
import re
import sys

lines = [l.rstrip("\n") for l in open(sys.argv[1], errors="replace")]
log = open(sys.argv[2], errors="replace").read()

passed = set()
for line in lines:
    m = re.match(r"PROBE (\S*?)control\.allowed REACHED", line)
    if m:
        passed.add(m.group(1))

for line in lines:
    print(line)
    m = re.match(r"PROBE (\S*?)canary\.send\.(\S+) SKIPPED sent:(\S+)", line)
    if not m:
        continue
    prefix, what, canary = m.groups()
    name = f"{prefix}canary.arrived.{what}"
    if re.search(re.escape(canary) + r"(\s|$)", log):
        print(f"PROBE {name} REACHED arrived-at-host-listener")
    elif prefix in passed:
        print(f"PROBE {name} BLOCKED not-in-listener-log")
    else:
        print(f"PROBE {name} SKIPPED no-positive-control:not-in-listener-log")
