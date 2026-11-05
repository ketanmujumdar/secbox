#!/usr/bin/env python3
"""Turns sends and plants into results by looking at what reached the host.

Reads probe output and the host listener's log, prints every probe line, and
adds one line per canary and one per plant. A canary or plant that did not
arrive is BLOCKED only if that context's control passed; otherwise SKIPPED.

Usage: verify.py PROBE_OUTPUT LISTENER_LOG [GUEST_STATE_DIR]

GUEST_STATE_DIR is the host directory behind the guest's /.msb share. This
script only ever calls os.lstat and os.readlink on names inside it. It never
opens a planted path and never follows a planted link, because the build plan's
rule is that host code must not follow paths under guest state: the guest can
make a real host symlink there.
"""
import os
import re
import stat as statmod
import sys

lines = [l.rstrip("\n") for l in open(sys.argv[1], errors="replace")]
log = open(sys.argv[2], errors="replace").read()
state_dir = sys.argv[3] if len(sys.argv) > 3 else ""

passed = set()
for line in lines:
    m = re.match(r"PROBE (\S*?)control\.allowed REACHED", line)
    if m:
        passed.add(m.group(1))


def planted_kind(rel):
    """What the host really has at this name, without following it."""
    if not state_dir:
        return None, "guest-state-not-configured"
    if "/" in rel or rel in ("", ".", ".."):
        return None, "unsafe-plant-name"
    path = os.path.join(state_dir, rel)
    try:
        st = os.lstat(path)
    except OSError:
        return None, "not-on-host"
    if statmod.S_ISLNK(st.st_mode):
        try:
            return "symlink", "real-host-symlink-to:" + os.readlink(path)
        except OSError:
            return "symlink", "real-host-symlink"
    if statmod.S_ISDIR(st.st_mode):
        return "dir", "real-host-directory"
    return "file", "real-host-file:%d-bytes" % st.st_size


# A plain file planted in the share is the control for the plants: if even that
# did not appear on the host, nothing about the others can be concluded.
control_arrived = {}
for line in lines:
    m = re.match(r"PROBE (\S*?)plant\.control SKIPPED planted:(\S+)", line)
    if m:
        kind, _ = planted_kind(m.group(2))
        control_arrived[m.group(1)] = kind is not None

for line in lines:
    print(line)

    m = re.match(r"PROBE (\S*?)canary\.send\.(\S+) SKIPPED sent:(\S+)", line)
    if m:
        prefix, what, canary = m.groups()
        name = f"{prefix}canary.arrived.{what}"
        if re.search(re.escape(canary) + r"(\s|$)", log):
            print(f"PROBE {name} REACHED arrived-at-host-listener")
        elif prefix in passed:
            print(f"PROBE {name} BLOCKED not-in-listener-log")
        else:
            print(f"PROBE {name} SKIPPED no-positive-control:not-in-listener-log")
        continue

    m = re.match(r"PROBE (\S*?)plant\.(\S+) SKIPPED planted:(\S+)", line)
    if m:
        prefix, what, rel = m.groups()
        name = f"{prefix}plant.arrived.{what}"
        kind, detail = planted_kind(rel)
        if kind is not None:
            print(f"PROBE {name} REACHED guest-write-landed-on-host:{detail}")
        elif what == "control" or not control_arrived.get(prefix):
            print(f"PROBE {name} SKIPPED no-positive-control:{detail}")
        else:
            print(f"PROBE {name} BLOCKED {detail}")
