# Security model

secbox treats your machine as trusted and everything inside a project's VM as compromised. Each control limits what a compromise can reach. None of them removes the risk completely, and several are not built yet.

> secbox cannot tell whether code is safe. Its job is to keep a compromise contained until a person approves something leaving the sandbox.

## Threats and controls

The **State** column says what is real today.

| Threat | Control | What remains | State |
| --- | --- | --- | --- |
| A malicious package runs at install or build time | It runs inside the VM, with no host files or credentials | It can read anything placed in the VM | Built. Tested on one real VM |
| Data is sent out | Deny-by-default network with an allowlist of hostnames | Leaks through an allowed host | Hostname allowlist only. Method and path rules are not enforced |
| A worm publishes to npm with a stolen token | npm allowed read-only, so publish requests are blocked | Other allowed write paths | Policy can say this. Nothing enforces it yet |
| An API key is stolen | The key stays on the host, and a proxy adds it to requests for one host | Misuse through the proxy while the sandbox runs | Not built. Secrets assigned to the proxy route are refused |
| Files are planted for your machine, such as `.vscode/tasks.json` | No shared folder. Changes come back only through review | Malicious code you approve | No shared folder: built. Review and export: not built |
| An AI agent is tricked | The agent reaches the code only through the sandbox, and cannot change policy, read stored secrets or push | It can still damage the sandbox | Built for MCP, with caveats. See [AI agents](../guides/ai-agents.md) |
| A VM or hypervisor escape | One VM per project, a patched engine, a small host interface | A zero-day could still reach the host | Depends on the engine |
| An attack on secbox's own interface | No local web port, plain-text rendering of everything from a sandbox | Bugs in the app or daemon | Built |

## Principles

**Project files are data.** `check` and the daemon read the Compose file, Dockerfile and `secbox.yaml` without running anything. Every name from a project is shown with control characters escaped, so a hostile name cannot rewrite your terminal.

**A project's requests are only requests.** A project can ask for 999 TB of memory, ports or hostnames. It gets no more than your limits allow, and ports are forwarded only when you approve them.

**What you approve is what runs.** The plan you see carries a digest. The start request must name the same digest, or the daemon refuses.

**No network ports on the host side.** The CLI, the desktop app and the MCP server talk to the daemon over an owner-only Unix socket. Nothing listens on `localhost`, where any local process or a web page could reach it.

**Destroy, then rotate.** After a suspected compromise, run `secbox down --destroy` and rotate every secret that sandbox was given. A build cache and a data volume can both carry a compromise.

## Inside one project

The VM is one boundary around the whole project. **Inside it, a compromise can spread.** If the `app` service is compromised it can attack `db` on the same network.

Because every service's secrets are delivered inside the same VM, a project with any secret assigned will not start if a service asks for a host mount, the Docker socket or `privileged: true`. One hostile service could otherwise read another service's secrets. Without secrets assigned, these are reported as findings.

## Things secbox cannot protect

- **Your browser.** A web app from the sandbox, opened on your machine, runs its JavaScript in your browser, outside the VM's network rules. Use a separate browser profile for it.
- **Code you approve.** Malicious code that passes review can run later on your machine or in CI.
- **An AI agent's own host tools.** If an agent on your machine has a shell, secbox cannot take it away. See [AI agents](../guides/ai-agents.md#what-this-does-not-protect).
- **Your user account.** The daemon checks which user connects, not which program. Anything running as you can run `secbox` as you.
- **What a service prints.** A service given a secret can print it, and `secbox logs` will show it. Logs are not masked.
- **Secrets you deliver.** A service can read every secret it was given, and so can anything sharing its container.

## Blocked traffic is only partly visible

`secbox net blocked` shows hostname lookups the engine refused. Attempts to raw IP addresses, the cloud metadata address and UDP are blocked but **not recorded**. An empty list does not mean nothing was tried. See [Network](../guides/network.md#blocked-hostnames).

## Further reading

The full design, with the incidents that motivated it, is in [`docs/ARCHITECTURE.md`](https://github.com/ketanmujumdar/secbox/blob/main/docs/ARCHITECTURE.md) in the repository.
