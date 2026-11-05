#!/usr/bin/env python3
"""Attacks a running sandbox from inside and reports what got out.

Usage: python3 -I tests/leak/check-running.py [SERVICE]

It writes nothing in the sandbox and does not stop it. SERVICE is a Compose
service with wget, used to check traffic from a nested container (default web).
It sends harmless requests, carrying a random token and no secrets.

PASS       the attempt was stopped, and a control proved the probe can work
FAIL       something got out that should not have
GAP        a limit we already know about, which you should not rely on
SKIPPED    the probe could not run, which proves nothing

Exit code: 0 when nothing failed or was skipped, 1 otherwise.
"""
import http.server
import json
import os
import secrets
import socket
import subprocess
import sys
import threading

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.environ.get("MSB", os.path.join(ROOT, ".tools/msb/bin/msb"))
ENV = dict(os.environ, MSB_HOME=os.environ.get("MSB_HOME", os.path.expanduser("~/.secbox/msb")))
SERVICE = sys.argv[1] if len(sys.argv) > 1 else "web"
TOKEN = "canary-" + secrets.token_hex(8)
ALLOWED_URL = "https://registry.npmjs.org/left-pad"
results = []
received = []


class Listener(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        received.append(self.path)
        self.send_response(200)
        self.end_headers()

    do_POST = do_GET

    def log_message(self, *_):
        pass


def report(status, name, detail=""):
    results.append(status)
    print(f"{status:<8}{name}" + (f"  ({detail})" if detail else ""))


def engine(*args, timeout=40):
    done = subprocess.run([ENGINE, *args], env=ENV, capture_output=True, text=True, timeout=timeout)
    return done.returncode, done.stdout + done.stderr


def inside(box, context, script, timeout=30):
    prefix = ["exec", box, "--no-stdin", "--no-tty", "--workdir", "/work", "--"]
    if context == "guest":
        return engine(*prefix, "sh", "-c", script, timeout=timeout)
    return engine(*prefix, "docker", "compose", "exec", "-T", SERVICE, "sh", "-c", script, timeout=timeout)


def fetch(box, context, url, extra=""):
    """Returns 'reached', 'blocked' or 'unknown'.

    The gateway answers a blocked plain HTTP request itself, with a 403 and
    network_policy_denied. wget hides that body and reports a server error, so
    plain HTTP is read with nc, where the reply says who actually answered.
    """
    if url.startswith("http://"):
        host, _, path = url[len("http://"):].partition("/")
        name, _, port = host.partition(":")
        request = f"GET /{path} HTTP/1.0\\r\\nHost: {name}\\r\\n\\r\\n"
        _, out = inside(box, context, f"command -v nc >/dev/null || echo NO_NC; printf '{request}' | nc -w 4 {name} {port or 80} 2>&1")
        if "NO_NC" in out:
            return "unknown", out
        if "network_policy_denied" in out or "HTTP/" not in out:
            return "blocked", out
        return "reached", out
    code, out = inside(box, context, f"wget -q -T 6 -O /dev/null {extra} '{url}' 2>&1; echo rc=$?")
    if "not found" in out and "wget" in out:
        return "unknown", out
    if "rc=0" in out or "server returned error" in out:
        return "reached", out
    return "blocked", out


def lan_address():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
        try:
            probe.connect(("192.0.2.1", 9))
            return probe.getsockname()[0]
        except OSError:
            return None


def main():
    code, out = engine("status", "-a", "--format", "json")
    running = [b for b in json.loads(out) if str(b.get("status")).lower() == "running"] if code == 0 else []
    if len(running) != 1:
        print(f"Expected one running sandbox, found {len(running)}. Start one with: sb up")
        return 1
    box = running[0]["name"]
    print(f"Sandbox  {box}\nNested   the {SERVICE} container\n")

    listener = http.server.ThreadingHTTPServer(("0.0.0.0", 0), Listener)
    port = listener.server_address[1]
    threading.Thread(target=listener.serve_forever, daemon=True).start()
    lan = lan_address()

    # The listener has to work, or "no canary arrived" would mean nothing.
    subprocess.run(["curl", "-s", "-m", "3", "-o", "/dev/null", f"http://127.0.0.1:{port}/control-{TOKEN}"])
    if any(f"control-{TOKEN}" in path for path in received):
        report("PASS", "control: the canary listener receives requests from this Mac")
    else:
        report("SKIPPED", "control: the canary listener did not receive its own request")
    received.clear()

    usable = {}
    for context in ["guest", "nested"]:
        state, out = fetch(box, context, ALLOWED_URL)
        usable[context] = state == "reached"
        if usable[context]:
            report("PASS", f"{context}: control, an allowed host is reachable")
        else:
            report("SKIPPED", f"{context}: control failed, so its blocks below prove nothing", out.strip()[-80:])
            if "bad address" in out:
                print("        DNS is failing. If this Mac changed networks since the sandbox started, restart it.")

    targets = [
        ("an unlisted website", "https://example.com/"),
        ("a raw IP address", "http://1.1.1.1/"),
        ("the cloud metadata address", "http://169.254.169.254/latest/meta-data/"),
    ]
    host_targets = [
        ("this Mac through the sandbox's host alias", f"http://host.microsandbox.internal:{port}/{TOKEN}-alias"),
        ("this Mac through the gateway address", f"http://172.16.0.5:{port}/{TOKEN}-gateway"),
    ]
    if lan:
        host_targets.append(("this Mac's LAN address", f"http://{lan}:{port}/{TOKEN}-lan"))
    for context in ["guest", "nested"]:
        if not usable[context]:
            continue
        for label, url in targets + host_targets:
            state, _ = fetch(box, context, url)
            if state == "blocked":
                report("PASS", f"{context}: blocked from {label}")
            elif state == "reached":
                report("FAIL", f"{context}: REACHED {label}", url)
            else:
                report("SKIPPED", f"{context}: could not test {label}")
        _, out = inside(box, context, f"nslookup {TOKEN}.example.com 2>&1; echo rc=$?")
        if TOKEN in out and ("Address" in out.split("Name:")[-1] if "Name:" in out else False):
            report("FAIL", f"{context}: DNS resolved an unlisted name, which can carry data out")
        else:
            report("PASS", f"{context}: DNS did not resolve an unlisted name")

    if received:
        report("FAIL", "the canary reached this Mac", ", ".join(received[:3]))
    else:
        report("PASS", "no canary reached the listener on this Mac")

    _, out = inside(box, "guest", "ls -a /work | grep -cx .env; test -e /Users; echo users=$?; env | grep -ci -E 'aws_|github_token|npm_token|anthropic'")
    lines = out.split()
    if len(lines) >= 3 and lines[0] == "0" and "users=1" in lines[1] and lines[2] == "0":
        report("PASS", "guest: no .env, no Mac home folders, no credential variables")
    else:
        report("FAIL", "guest: found a project .env, Mac folders or credential variables", out.strip()[:100])

    listening = subprocess.run(["lsof", "-nP", "-iTCP", "-sTCP:LISTEN"], capture_output=True, text=True).stdout
    bound = {line.split()[8] for line in listening.splitlines() if line.startswith("msb ")}
    if bound and all(address.startswith("127.0.0.1:") for address in bound):
        report("PASS", f"Mac: all {len(bound)} published ports listen on 127.0.0.1 only")
    else:
        report("FAIL" if bound else "SKIPPED", "Mac: published ports", ", ".join(sorted(bound)))

    # What the design does not stop today.
    if usable["guest"]:
        state, _ = fetch(box, "guest", f"https://registry.npmjs.org/-/{TOKEN}", f"--post-data={TOKEN}")
        if state == "reached":
            report("GAP", "a write request reached npm, so npm publish is not blocked by method")
        else:
            report("PASS", "a write request to npm was blocked")
    else:
        report("SKIPPED", "the npm write test, because the allowed host is not reachable")
    report("GAP", "data can leave through any of the allowed build hosts")
    report("GAP", "a web app's JavaScript runs in your browser, outside these network rules")

    listener.shutdown()
    print()
    fails, skipped, gaps = (results.count(k) for k in ("FAIL", "SKIPPED", "GAP"))
    print(f"{results.count('PASS')} passed, {fails} failed, {skipped} skipped, {gaps} known gaps")
    print("This checks the routes above on this sandbox. It does not prove nothing can escape.")
    return 1 if fails or skipped else 0


if __name__ == "__main__":
    sys.exit(main())
