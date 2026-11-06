# Network policy and blocked hostnames

A sandbox's network is deny-by-default. Today the only rule on real traffic is the engine's **hostname allowlist**. secbox can also explain a fuller policy, with HTTP methods, ports and paths, but nothing enforces that policy yet.

## Approving a host

```sh
secbox net blocked                 # what was refused
secbox net allow api.example.com   # let the sandbox reach it
```

In the desktop app, press **Approve** on a row of Blocked traffic. In the terminal interface, press `n` for Network, then `a` for the bottom row.

Name one exact host, with no patterns. The approval is stored on your machine, never in the project, and the sandbox has no way to ask for it itself.

**Everything in the sandbox can then reach that host**, including any service holding a secret. What runs inside chooses which names it looks up, so a name can itself be data trying to get out. Approve only names you recognise.

The engine cannot change the rules of an existing sandbox, so secbox replaces it: it stops the services, keeps the disk, creates a new sandbox with the new rules, copies the data across and starts the services again. This takes about half a minute on the small project it was measured on.

- **Kept:** `/work`, Docker's images, containers and volumes, and `/root`, `/home`, `/opt`, `/srv` and `/usr/local`.
- **Not kept:** anything in memory, or packages installed in the VM itself. Services restart.
- If a step fails, the data stays in the kept disk, and approving again carries on from it.
- The kept disk stays in the project's state folder until the sandbox is destroyed.

## What is enforced today

When the daemon creates a sandbox, it gives the engine the list of hostnames the project may reach. Lookups for any other name are refused. Raw IP addresses, the cloud metadata address (`169.254.169.254`) and UDP were also blocked when tested.

The hostnames appear in the plan that `secbox up` shows before you approve it.

### Build hosts

A build needs to download images and packages. The daemon picks the build hosts from a fixed menu, according to the project's files:

| Group | Hosts | Given when |
| --- | --- | --- |
| Container images | `registry-1.docker.io`, `auth.docker.io` and Docker Hub's download hosts | Always |
| Alpine packages | `dl-cdn.alpinelinux.org` | The project uses Alpine |
| npm packages | `registry.npmjs.org` | The project uses npm |
| Go modules | `proxy.golang.org`, `sum.golang.org`, `storage.googleapis.com` | The project uses Go |
| Web fonts | `fonts.googleapis.com`, `fonts.gstatic.com` | The build fetches web fonts, as Next.js does |

A project can switch on a group it might really need, but it **can never name a host of its own**. Hosts for the running app come only from approvals stored on your machine, never from the project.

If the detection misses something, the build fails on a refused name, and `secbox net blocked` shows which.

> **Build hosts stay reachable while the project runs.** The engine fixes a sandbox's rules when it is created, so every service, including one that holds a secret, can reach the build hosts. Some of them, such as Docker Hub and `storage.googleapis.com`, accept uploads from anyone. A separate build sandbox with no secrets would fix this, and is not built yet.

## Explaining a policy: `net check`

```sh
secbox net check POLICY METHOD URL
```

Answers one question: would this policy allow this request? It sends nothing and looks nothing up.

```sh
secbox net check policy.yaml GET https://registry.npmjs.org/left-pad    # allow, exit 0
secbox net check policy.yaml PUT https://registry.npmjs.org/left-pad    # deny, exit 4
```

```text
Policy    policy.yaml, 2 rules
Request   PUT https://registry.npmjs.org:443/left-pad
Decision  deny
Reason    rule npm-read-only does not allow this method

This only explains the policy. Nothing was sent, and no name was looked up.
A real proxy would also check the address a name resolves to.
```

![secbox net check allowing a GET from the npm registry](../images/cli/net-check-allow.png)

Usernames, passwords and query strings in the URL are never printed.

### Policy file format

```yaml
version: 1
rules:
  - name: npm-read-only
    host: registry.npmjs.org
    ports: [443]
    methods: [GET, HEAD]
  - name: anthropic-messages
    host: api.anthropic.com
    ports: [443]
    methods: [POST]
    path_prefix: /v1/messages
```

| Key | Meaning |
| --- | --- |
| `version` | Always `1` |
| `rules` | A list. A request is allowed only if a rule allows it |
| `name` | Shown in the explanation |
| `host` | The exact hostname |
| `ports` | Allowed ports |
| `methods` | Allowed HTTP methods, in uppercase |
| `path_prefix` | Optional. The path must start with this |

Everything else is denied. Some addresses are denied whatever the rules say: cloud metadata endpoints, loopback, link-local, multicast and broadcast. Private ranges (such as `10.0.0.0/8` and `192.168.0.0/16`) are reachable only by a rule naming that exact IP.

## Blocked hostnames

```sh
secbox net blocked                # time, count and hostname, newest last
secbox net blocked -f             # keep checking; Ctrl-C stops
secbox net blocked --project PATH
secbox status                     # includes a count
```

In the [full-screen interface](terminal-ui.md), press `n` for the selected project's Network tab, whose name carries the number of hostnames refused. The Overview tab shows the count in a Blocked box, in red when anything was refused.

**Read this before relying on the list:**

- **Hostnames only.** Raw IP, metadata and UDP attempts are blocked but not recorded. An empty list does not mean nothing was tried.
- **Only sandboxes created recently record anything.** A sandbox's log level is fixed when it is created. An older sandbox says nothing is being recorded. To record, recreate it with `secbox down --destroy` and `secbox up`, which deletes its data.
- **The list lasts, but events can still be lost.** The engine keeps about 50 MB of log, about an hour on an idle sandbox, so the daemon collects names into its own file every 30 seconds while it runs. A name stays listed until you approve it or destroy the sandbox. A busy or hostile sandbox can still push events out faster than that, or while the daemon is not running, and the output says when older events were cut off. At most 1000 names are kept, the newest.
- **The daemon's file holds names as they were looked up.** It is owner-only, in the project's state folder, and goes when the sandbox is destroyed. A name can carry a secret, and it is masked only when shown.
- **Counts are refused queries.** One lookup is usually two queries, for IPv4 and IPv6.
- **The guest chooses each name.** A name shows what was asked for and proves nothing else.
- **Secret values are masked.** A secret cut into a hostname label shows as its name, such as `[API_KEY]`.

The record is the engine's log, read on the host by the daemon. In testing the guest could not write to it. If a check on the folders holding it fails, the output says the record is not trustworthy.

## Planned

- A gateway that enforces the policy above, including methods and paths, and records every decision on the host.
- `secbox net log`, to show allowed requests too.
