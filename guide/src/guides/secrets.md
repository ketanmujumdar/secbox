# Secrets

secbox keeps secret values in your operating system's keychain and gives each one only to the services you name. Values never go into the project, a secbox file, a command-line argument or the sandbox's disk.

```sh
secbox secrets set API_KEY --service api      # type the value at a hidden prompt
secbox secrets import .env --dry-run          # see which names a .env holds
secbox secrets import .env                    # store them
secbox secrets list                           # names and where they go, never values
secbox secrets rm API_KEY
```

Every command takes `--project PATH` to name a project in another folder. Secrets are stored per project.

## Where values are kept

| OS | Store |
| --- | --- |
| macOS | Keychain, under the service name `secbox` |
| Linux | Secret Service (GNOME Keyring, KWallet and others), over D-Bus |

secbox's own state folder holds **names only**: which secrets exist, how each was stored, and which services get it. The audit log records names, never values.

## Storing one secret

```sh
secbox secrets set NAME [--service S]... [--as-file] [--build] [--proxy-host HOST]
```

You type the value twice at a hidden prompt. There is no way to pass a value as an argument, because arguments are visible to other programs in the process list. If you try, secbox refuses with exit code 2 and does not print the value.

| Option | Meaning |
| --- | --- |
| `--service NAME` | A Compose service that may receive it. Repeat for more than one. Without it, the secret is stored but goes nowhere |
| `--as-file` | Deliver it as a file at `/run/secrets/NAME` instead of an environment variable. Narrower, but the program has to read a file |
| `--build` | Deliver it only to the build, as a build secret. **Recorded, but not delivered yet** |
| `--proxy-host HOST` | Keep it on the host and have a proxy add it to requests for this one host. **Not built: a project with such a secret will not start** |

## Importing a .env file

```sh
secbox secrets import path/to/.env --dry-run
secbox secrets import path/to/.env
```

The file is parsed as data. It is never run through a shell, so a line like `X='$(touch /tmp/pwned)'` stores that text literally and runs nothing. `export NAME=value` and quoted values are understood. Empty values are skipped. Output lists names only:

```text
Dry run. The keychain was not touched.
File       tests/fixtures/fake.env
Project    /home/you/code/myapp
Would import 4 names:
  FAKE_API_KEY
  FAKE_DB_PASSWORD
  FAKE_EXPANSION
  FAKE_EMPTY  (empty, would be skipped)
```

Imported secrets go to no service until you assign them. Run `secbox secrets set NAME --service S` for each, typing the value again.

## Listing

```sh
secbox secrets list
```

Shows the project's id, then each name, how it was stored (`prompt` or `import`) and where it goes (a service, or `no service`). It reads secbox's metadata only and never opens the keychain, so it never shows a value and macOS never asks for permission.

## How a secret reaches a service

On every `secbox up`:

1. The daemon reads the values from the keychain.
2. It mounts a small `tmpfs` inside the VM (`noexec,nosuid,nodev`, owner-only). If that mount cannot be made, the start stops, rather than write a secret to disk.
3. Values are sent in over standard input, never as arguments.
4. Each service gets only the secrets assigned to it, as environment variables or as files under `/run/secrets`.

Because the `tmpfs` disappears when the VM stops, this happens again on each start.

`up` prints a **Where secrets could go** section in the plan, and a `Secrets` line per service once started.

## What this does not protect

- **A service can read every secret it was given**, and so can anything sharing its container.
- **A service can print a secret**, and `secbox logs` shows it unmasked.
- **`secbox exec` and `secbox shell` can read it.** So can an [AI agent](ai-agents.md) using the `exec` tool, and its model provider then sees it.
- **No privileged services.** A project with any secret assigned will not start if a service asks for a host mount, the Docker socket or `privileged`, because that service could read the others' secrets.

After a suspected compromise, run `secbox down --destroy` and rotate every secret that sandbox was given.

## Removing

```sh
secbox secrets rm NAME
```

Deletes it from the keychain and the metadata. `secbox uninstall --purge` does **not** remove keychain entries, so remove secrets with `rm` first.
