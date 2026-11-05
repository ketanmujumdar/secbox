# Hostile demo

Two small projects that play a compromised dependency trying to steal a key.

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
secbox down --destroy $P
```

Expected: every probe prints BLOCKED, and `net blocked` lists
`exfil.example.net` and `build-exfil.example.net`. This needs a real VM and has
not been run yet: see `docs/NEEDS_A_VM.md`.

The token is printed to the log on purpose. `logs` does not mask values yet, so
you will see it there. Use a fake value.
