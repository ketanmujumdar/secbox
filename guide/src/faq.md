# FAQ

Short answers to common questions about secbox.

### Is it safe to use for untrusted code today?

No. secbox is an early build. Its main flows have run on a real Mac, but containment has not been shown under attack, and only the hostname allowlist applies to real network traffic. See [What works today](start/status.md).

### How is this different from Docker?

A Docker container shares your machine's kernel, and Docker Desktop's own boundary has had container-to-host bugs. secbox gives each project a microVM with its own kernel and its own Docker daemon. It also keeps secrets in your keychain, restricts the network by default and forwards ports only to `127.0.0.1` when you approve them.

### Do I need Docker installed?

No. Docker runs inside each VM.

### Does my project need changes?

Usually not. A Compose project runs with its own Compose files. A project with only a `Dockerfile` gets a generated one-service Compose file. You can add a `secbox.yaml` to request memory, CPU and disk.

### Which platforms are supported?

macOS on Apple Silicon, and Linux with KVM. Not Windows. Confirming with Touch ID is macOS only: on Linux, starts, approvals and pushes go ahead without a prompt.

### Why does it ask for my fingerprint?

Before it starts a sandbox, lets one reach a new hostname, or pushes, so that a program running as you can ask but only you can agree. Read what the prompt names, and cancel it if you did not ask. See [Confirming with Touch ID](concepts/confirming.md).

### Where do my changes go?

They stay inside the sandbox. `secbox diff` shows what changed and flags risky files, but `export`, which would bring them back through Git, is not built. Until then, treat a sandbox as a place to run and test, and make your changes on your machine.

### Why does it copy a commit and not my folder?

A shared folder would let code in the VM plant files on your machine, such as `.vscode/tasks.json` or `.claude/settings.json`, which some recent attacks did. A one-way copy of a commit keeps the boundary clear, and leaves out files that look like secrets.

### Can I use Claude Code or Codex?

Yes, through MCP. The agent runs on your machine and works in the sandbox through `secbox mcp serve`, with its own host tools turned off. See [AI agents over MCP](guides/ai-agents.md). Running the agent inside the VM is planned.

### Can I SSH into the sandbox, or use VS Code Remote?

There is no SSH server and no open port in the VM, and VS Code Remote-SSH is not offered, because a compromised remote has been reported to reach a local terminal. To edit the sandbox's files in VS Code, use `secbox open --sandbox`, which works differently. See [Editors](guides/editors.md#editing-inside-the-sandbox). For commands, use `secbox shell` and `secbox exec`.

### Can a secret show up in `secbox logs`?

`logs` replaces each assigned secret's value with its name, such as `[API_KEY]`, including base64, URL-encoded and hex forms. A value the service encrypted or reworked is not recognised, and `shell` and `exec` output is never masked.

### What happens on `down --destroy`?

The VM and everything in it is deleted: the project copy, images, build caches, data volumes and any changes. Secrets in your keychain stay. If you suspect a compromise, rotate the secrets that sandbox was given.

### Will the name change?

Possibly. `secbox` is a working name, and an unrelated openSUSE package already uses it.

### What license is it under?

The [Functional Source License](https://fsl.software), FSL-1.1-ALv2. You can use, change and share secbox for anything, including at work, except to sell it or offer it as a product or service that competes with it. Two years after each version is released, that version is also available under Apache-2.0, with no restriction. The full text is [LICENSE.md](https://github.com/ketanmujumdar/secbox/blob/main/LICENSE.md).

This makes secbox source-available, not open source as the OSI defines it.
