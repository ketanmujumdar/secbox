# Hostile demo

Two small projects that play a compromised dependency trying to steal a key.
Each attack copies a technique from a real campaign: Shai-Hulud and the 2026
Keyv/ChainDrop worm (tokens from the environment, a sweep of the disk), LiteLLM
in March 2026 (cloud metadata), s1ngularity and the October 2026 testmetest
worm (a hook planted for AI coding tools), DNS tunnelling and a raw IP. A
hostile sidecar tries the Docker socket, `/proc` and the VM's `/run`.

## Without secbox, for comparison

```sh
cd tests/fixtures/hostile-demo
docker compose -p baseline -f without-secbox.yaml up --build -d
sleep 30; docker compose -p baseline -f without-secbox.yaml logs api evil attacker
docker compose -p baseline -f without-secbox.yaml down
```

Every send goes to `attacker`, a listener on your own machine. Run in plain
Docker on Linux, seven attacks printed LEAKED and the listener received the
token four ways. The metadata and `/run` probes print N/A there, because there
is no cloud metadata service and no secbox.

## `hostile-demo`: must be refused

`api` is given `DEMO_TOKEN`. `evil` is given nothing and tries to take it from
inside the same VM: `privileged`, `pid: host`, a mount of `/run` (where the
secrets are) and the Docker socket.

```sh
secbox secrets set DEMO_TOKEN --service api --project tests/fixtures/hostile-demo
secbox up tests/fixtures/hostile-demo
```

Expected: the plan lists four blockers, all naming `evil`, and nothing starts.
The daemon tests prove this against the fake engine.

## `hostile-demo-run`: starts, and every probe should be blocked

Only `api`, with no escape settings. Its `probe.sh` tries to send the token
over HTTPS to a host not on the allowlist, over HTTP to a raw IP, to the cloud
metadata address, inside a DNS name, and to read the secret folder. Its build
also tries to phone home.

```sh
P=tests/fixtures/hostile-demo-run
secbox secrets set DEMO_TOKEN --service api --project $P   # any fake value
secbox up $P
secbox logs api --project $P        # each line: BLOCKED or LEAKED
secbox net blocked --project $P
secbox status $P                    # "Where secrets could go"
secbox diff $P; echo "exit $?"      # the planted hook, flagged
secbox down --destroy $P
```

Expected: every probe prints BLOCKED, and `net blocked` lists
`exfil.example.net` and `build-exfil.example.net`.

The probe also plants a hook for AI coding tools, as s1ngularity and
testmetest did. `api` has the project mounted at `/work`, as a dev setup
would, so the hook lands in the sandbox's copy of the project, and only
there: your own folder is untouched. Its report line says LEAKED, because the
write itself succeeds. What matters is that `diff` starts with

```text
FLAG  .claude/settings.json  adds a hook for an AI coding tool, which would run on your machine
```

and exits 5. The daemon tests check that this exact hook is flagged, against
the fake engine. This needs a real VM and has
not been run yet: see `docs/NEEDS_A_VM.md`.

The token is printed to the log on purpose. `logs` should show `[DEMO_TOKEN]`
in its place, including in the base64 the probe sends. Use a fake value anyway.
