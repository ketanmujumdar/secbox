# Bringing a project in

`secbox new` brings a project in from an SSH remote or a folder, shows what it will be able to do, asks once, and starts it. It needs the daemon (`secboxd`) running.

> Built, and has run on a real Mac against a real Git server. See [What works today](../start/status.md).

```sh
secbox new git@github.com:acme/shop.git               # the remote's default branch
secbox new git@github.com:acme/shop.git --ref release # a branch, tag or full commit id
secbox new .                                          # the project in this folder
secbox new ~/code/app                                 # another folder
```

`new` has no powers of its own. It sends the daemon the same requests as `secbox up`, in the same order, and the daemon checks everything again. Only the way the plan is shown is different.

## What it prints

This is the output of a test, against a fake engine and a fake remote, with `y` typed at the prompt. The commit and the digest change from run to run:

```text
· Fetching git@example.invalid:org/shop.git on this machine, with your SSH key
✓ Fetched the default branch @ 2e577ba with your SSH key (the key stays here)
✓ Read compose.yaml: 1 service (nothing has run yet)

shop will be able to…
  reach      registry-1.docker.io, auth.docker.io,   while building, and after
             production.cloudflare.docker.com,
             production.cloudfront.docker.com,
             docker-images-prod.6aa30f8b08e16409b46e0173d6de2f56.r2.cloudflarestorage.com
  open       no ports: the project publishes none
  get        no secrets
  use        4.0 GB memory · 2 CPUs · 30.0 GB disk
! build hosts stay reachable after the build, because the sandbox's rules cannot be changed once it exists
plan 469e5f18 · secbox up git@example.invalid:org/shop.git --plan-only shows all of it, after fetching again

Start with this plan? [y/N] y
✓ Plan checked again: it is the one you approved  0s
✓ Virtual machine started  0s
✓ Cloned into the VM through the relay, with no key inside  0s
✓ Docker running inside the VM  0s
✓ No secrets to deliver  0s
· Building images and starting services. The first start can take several minutes
✓ Images built and services started  0s
· No ports opened

Started in 0s.
What runs inside the sandbox is untrusted. Its own reports can be wrong.

Next: secbox                                                   opens the full-screen view
      secbox logs --project git@example.invalid:org/shop.git   shows what the services print
      secbox down git@example.invalid:org/shop.git             stops it, keeping its data
```

## In order

1. **Fetch, for a remote.** The daemon fetches on your machine with your SSH key, into its own folder, and exports the commit's files there. The key, your agent and your Git credentials stay on your machine. This happens before the question, so the files are on your machine even if you then say no. Nothing from them runs. `secbox remove REMOTE` deletes that copy. If a sandbox already holds the remote, nothing is fetched again and `--ref` is not used: bring in newer commits with `secbox git pull`. See [Git: import, pull and push](https://github.com/ketanmujumdar/secbox/blob/main/docs/GIT.md).
   For a folder, its last commit is used. Uncommitted changes stay out.
2. **Read.** The daemon reads the Compose file, or the `Dockerfile` and the Compose file it writes for it, and makes the plan. Nothing from the project has run.
3. **Review.** The plan, grouped:
   - **reach**: the hostnames the sandbox may reach while building and while running. Until the sandbox's rules can change after the build, build hosts stay reachable afterwards, and the review says so.
   - **open**: the ports forwarded to `127.0.0.1`, and the service each goes to.
   - **get**: each secret, the services it goes to, and how it is delivered.
   - **use**: memory, CPUs and disk, and the memory for the build if it is different.
   - **`!` lines**: Critical and High findings, notes on ports, and where secrets could be sent. That last list is every host the sandbox can reach while it runs, because any service holding a secret can send it to any of them.
   - **plan**: the start of the plan's digest, and the command that shows the whole plan.
4. **Ask.** `Start with this plan? [y/N]`. Only `y` or `yes` starts it. Anything else, or no answer, starts nothing and exits 1. If something blocks the start, `new` lists it under **This cannot be started yet** and stops before asking.
5. **Start.** The daemon builds the plan again and refuses if it changed since you saw it. Then it starts the VM, copies the commit in or clones the remote through the relay, starts Docker, delivers secrets and runs `docker compose up --build`. Each step gets a ✓ when the next one begins. Build output is not shown here: use `secbox logs` once it is running.

## Ports

Saying yes forwards every port in the review's **open** line. To change that:

```sh
secbox new . --port 3000     # only 3000; repeat for more
secbox new . --no-ports      # none
```

`--port` accepts only ports the plan lists. Ports not chosen are listed as not opened in the review.

## Scripts and CI

`new` asks only when both stdin and stdout are a terminal. Otherwise it still prints the review, then refuses to start and exits 1 unless you pass `--yes`:

```sh
secbox new git@github.com:acme/shop.git --yes
```

`--yes` approves the plan as printed. It skips no check: the daemon still compares the plan's digest and refuses anything that blocks the start.

## Compared with up

| | `new` | `up` |
| --- | --- | --- |
| Shows the plan | As reach, open, get and use | As a list of fields |
| Asks | Once: start with this plan, default no | Only about ports, default no |
| Without a terminal | Refuses unless `--yes` | Starts, with no ports unless `--port` or `--all-ports` |
| Ports by default | All in the plan, once you say yes | None |
| Choosing the commit | The last commit, or `--ref` for a remote | Also `--commit ID` |
| `--plan-only` | No: use `secbox up … --plan-only` | Yes |

`new` has no `--name`: the daemon names a project after its folder, or after the remote's repository, and nothing yet lets you choose.
