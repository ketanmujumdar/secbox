#!/usr/bin/env python3
"""Prints probe output as a table: one row per check, one column per context.
R=REACHED B=BLOCKED S=SKIPPED. Usage: summary.py PROBE_OUTPUT
"""
import re
import sys

CONTEXTS = ["guest", "container.bridge", "container.compose", "container.host",
            "build.buildkit", "build.legacy"]
rows = {}
for line in open(sys.argv[1], errors="replace"):
    m = re.match(r"PROBE (\S+) (REACHED|BLOCKED|SKIPPED) ", line)
    if not m or "canary.send" in m.group(1):
        continue
    for c in CONTEXTS:
        if m.group(1).startswith(c + "."):
            rows.setdefault(m.group(1)[len(c) + 1:], {})[c] = m.group(2)[0]
            break
print("check".ljust(30), " ".join(c.split(".")[-1][:8].ljust(8) for c in CONTEXTS))
for name, vals in rows.items():
    print(name.ljust(30), " ".join(vals.get(c, "-").ljust(8) for c in CONTEXTS))
